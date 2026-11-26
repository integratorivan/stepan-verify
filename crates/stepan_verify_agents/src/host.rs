//! Agents outlive the window.
//!
//! Stepan does not own the `claude` / `codex app-server` process. A small host
//! (`stepan_verify agent-host <dir>`, its own process group) does: it holds the
//! agent's stdin/stdout and a Unix socket. The window connects to the socket
//! and speaks the agent's own protocol through it, line for line. When the
//! window closes, quits or updates, the agent keeps working; the next window
//! connects again and gets:
//!
//! 1. a hello line `{"stepan_host":{"busy":…,"native_id":…}}`;
//! 2. the lines of the turn in progress and the approval requests nobody has
//!    answered yet, so the transcript, the spinner and the approval cards come
//!    back as they were.
//!
//! With no window connected and the agent idle for [`GRACE`], the host closes
//! the agent's stdin; the agent exits, and so does the host. The next message
//! resumes the session in a new process as before.
//!
//! Registry: `<home>/state/agents/<id>/` holds `spec.json` (what to run),
//! `meta.json` (pid, provider, native id, busy) and `host.log`.

use std::{
    io::{BufRead, BufReader, Write},
    os::unix::{
        net::{UnixListener, UnixStream},
        process::CommandExt as _,
    },
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::Provider;

/// The subcommand of the app binary that runs a host.
pub const ENTRY: &str = "agent-host";
/// Idle with no window this long: let the agent go.
const GRACE: Duration = Duration::from_secs(5 * 60);
/// Busy with no window and no output this long: stuck, treat it as idle.
const STUCK: Duration = Duration::from_secs(30 * 60);
/// Lines of one turn kept for a window that connects mid-turn.
const MAX_TURN_LINES: usize = 20_000;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Spec {
    provider: String,
    program: String,
    args: Vec<String>,
    sock: PathBuf,
    #[serde(default)]
    grace_secs: Option<u64>,
}

/// What a host tells about itself, in `meta.json`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Meta {
    pub pid: u32,
    pub provider: String,
    pub native_id: Option<String>,
    pub sock: PathBuf,
    pub cwd: PathBuf,
    pub busy: bool,
    /// Unix seconds.
    pub started: u64,
}

fn provider_name(p: Provider) -> &'static str {
    match p {
        Provider::Claude => "claude",
        Provider::Codex => "codex",
    }
}

fn root() -> PathBuf {
    crate::stepan_verify_home().join("state").join("agents")
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// The binary that runs hosts: this app, or `STEPAN_VERIFY_HOST_EXE` (tests).
fn exe() -> Result<PathBuf> {
    match std::env::var_os("STEPAN_VERIFY_HOST_EXE") {
        Some(p) => Ok(PathBuf::from(p)),
        None => std::env::current_exe().context("путь к stepan_verify"),
    }
}

// ------------------------------------------------------------------ client

/// A window's connection to a host.
pub(crate) struct Conn {
    pub writer: UnixStream,
    /// Taken by the thread that reads the agent's output.
    pub reader: Option<BufReader<UnixStream>>,
    /// The agent was mid-turn when we connected.
    pub busy: bool,
}

impl Drop for Conn {
    /// Hang up for every handle to the socket (the reader thread holds a clone),
    /// so the host knows the window is gone.
    fn drop(&mut self) {
        let _ = self.writer.shutdown(std::net::Shutdown::Both);
    }
}

impl Conn {
    fn connect(sock: &Path) -> Result<Self> {
        let stream = UnixStream::connect(sock).with_context(|| format!("сокет агента {}", sock.display()))?;
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut hello = String::new();
        reader.read_line(&mut hello)?;
        let hello: Value = serde_json::from_str(&hello).context("хост агента не поздоровался")?;
        let busy = hello.pointer("/stepan_host/busy").and_then(Value::as_bool).unwrap_or(false);
        Ok(Self { writer: stream, reader: Some(reader), busy })
    }

    pub fn write_value(&mut self, v: &Value) -> Result<()> {
        let mut line = serde_json::to_string(v)?;
        line.push('\n');
        self.writer.write_all(line.as_bytes()).context("агент недоступен")?;
        self.writer.flush()?;
        Ok(())
    }
}

/// Run `cmd` (program, arguments, environment, folder) under a new host and connect to it.
pub(crate) fn launch(provider: Provider, cmd: &Command) -> Result<Conn> {
    let id = format!("{:x}{:05x}", SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_micros()), std::process::id() & 0xfffff);
    let dir = root().join(&id);
    std::fs::create_dir_all(&dir)?;
    // macOS caps socket paths at 104 bytes: the temp dir, not the (maybe deep) home.
    let sock = std::env::temp_dir().join(format!("stepan-{id}.sock"));
    let spec = Spec {
        provider: provider_name(provider).into(),
        program: cmd.get_program().to_string_lossy().into_owned(),
        args: cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect(),
        sock: sock.clone(),
        grace_secs: std::env::var("STEPAN_VERIFY_HOST_GRACE").ok().and_then(|s| s.parse().ok()),
    };
    std::fs::write(dir.join("spec.json"), serde_json::to_vec_pretty(&spec)?)?;

    let mut host = Command::new(exe()?);
    host.arg(ENTRY).arg(&dir);
    // The agent's environment and folder go to the host, which passes them on.
    for (k, v) in cmd.get_envs() {
        match v {
            Some(v) => host.env(k, v),
            None => host.env_remove(k),
        };
    }
    if let Some(cwd) = cmd.get_current_dir() {
        host.current_dir(cwd);
    }
    let log = std::fs::File::create(dir.join("host.log"))?;
    // Its own process group: quitting the window does not take it down.
    host.stdin(Stdio::null()).stdout(Stdio::null()).stderr(log).process_group(0);
    let mut child = host.spawn().context("не удалось запустить хост агента")?;
    // Reap it if it ends while we are still here.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match Conn::connect(&sock) {
            Ok(conn) => return Ok(conn),
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(30)),
            Err(err) => return Err(err.context(format!("хост агента не ответил, лог: {}", dir.join("host.log").display()))),
        }
    }
}

/// The running host of a session, if there is one: connect to it.
pub(crate) fn attach(provider: Provider, native_id: &str) -> Option<Conn> {
    let meta = find(provider, native_id)?;
    Conn::connect(&meta.sock).ok()
}

/// A live host runs this session.
pub fn running(provider: Provider, native_id: &str) -> bool {
    find(provider, native_id).is_some()
}

fn alive(pid: u32) -> bool {
    // SAFETY: signal 0 only asks whether the process exists.
    pid > 0 && unsafe { libc::kill(pid as libc::pid_t, 0) } == 0
}

fn find(provider: Provider, native_id: &str) -> Option<Meta> {
    let name = provider_name(provider);
    for entry in std::fs::read_dir(root()).ok()?.flatten() {
        let Ok(text) = std::fs::read_to_string(entry.path().join("meta.json")) else { continue };
        let Ok(meta) = serde_json::from_str::<Meta>(&text) else { continue };
        if meta.provider != name || meta.native_id.as_deref() != Some(native_id) {
            continue;
        }
        // A host that died without cleaning up leaves its folder behind. Not a
        // connect: the host would take a probe for a new window and switch to it.
        if alive(meta.pid) && meta.sock.exists() {
            return Some(meta);
        }
        let _ = std::fs::remove_dir_all(entry.path());
    }
    None
}

// -------------------------------------------------------------------- host

struct State {
    provider: Provider,
    dir: PathBuf,
    meta: Meta,
    /// The connected window and its number (a newer one replaces it).
    client: Option<(u64, UnixStream)>,
    next_client: u64,
    /// Claude: prompts sent minus results seen.
    outstanding: u32,
    busy: bool,
    turn: Vec<String>,
    /// Unanswered approval requests: (request id, line).
    pending: Vec<(String, String)>,
    /// Codex: the initialize answer, replayed to windows that connect later.
    init: Option<Value>,
    init_request: Option<Value>,
    initialized: bool,
    last_output: Instant,
}

impl State {
    fn save(&self) {
        if let Ok(text) = serde_json::to_vec_pretty(&self.meta) {
            let _ = std::fs::write(self.dir.join("meta.json"), text);
        }
    }

    fn set_busy(&mut self, busy: bool) {
        if self.busy != busy {
            self.busy = busy;
            self.meta.busy = busy;
            self.save();
        }
        if !busy {
            self.turn.clear();
        }
    }

    fn set_native_id(&mut self, id: &str) {
        if !id.is_empty() && self.meta.native_id.as_deref() != Some(id) {
            self.meta.native_id = Some(id.to_owned());
            self.save();
        }
    }

    fn send_client(&mut self, line: &str) {
        if let Some((_, stream)) = &mut self.client {
            let ok = stream.write_all(line.as_bytes()).and_then(|_| stream.write_all(b"\n")).and_then(|_| stream.flush());
            if ok.is_err() {
                self.client = None;
            }
        }
    }

    /// A line from the agent.
    fn on_output(&mut self, line: &str) {
        self.last_output = Instant::now();
        let v: Value = serde_json::from_str(line).unwrap_or(Value::Null);
        match self.provider {
            Provider::Claude => match v.get("type").and_then(Value::as_str) {
                Some("system") if v.get("subtype").and_then(Value::as_str) == Some("init") => {
                    self.set_native_id(v.get("session_id").and_then(Value::as_str).unwrap_or_default());
                }
                Some("control_request") if v.pointer("/request/subtype").and_then(Value::as_str) == Some("can_use_tool") => {
                    let id = v.get("request_id").and_then(Value::as_str).unwrap_or_default().to_owned();
                    self.pending.push((id, line.to_owned()));
                }
                Some("result") => {
                    self.outstanding = self.outstanding.saturating_sub(1);
                    // The finished turn is in the transcript now.
                    self.turn.clear();
                    if self.outstanding == 0 {
                        self.set_busy(false);
                    }
                }
                _ => self.keep(line),
            },
            Provider::Codex => {
                let method = v.get("method").and_then(Value::as_str);
                let id = v.get("id").filter(|i| !i.is_null());
                match (id, method) {
                    // A request from the server: an approval.
                    (Some(id), Some(_)) => self.pending.push((id.to_string(), line.to_owned())),
                    // An answer to a window's request.
                    (Some(id), None) => {
                        if self.init_request.as_ref() == Some(id) {
                            self.init = v.get("result").cloned();
                        }
                        if let Some(thread) = v.pointer("/result/thread/id").and_then(Value::as_str) {
                            self.set_native_id(thread);
                        }
                    }
                    (None, Some("turn/started")) => {
                        self.set_busy(true);
                        self.turn.clear();
                        self.keep(line);
                    }
                    (None, Some("turn/completed")) => self.set_busy(false),
                    _ => self.keep(line),
                }
            }
        }
    }

    fn keep(&mut self, line: &str) {
        if self.busy && self.turn.len() < MAX_TURN_LINES {
            self.turn.push(line.to_owned());
        }
    }

    /// A line from the window. Returns what to pass to the agent, if anything.
    fn on_input(&mut self, line: &str) -> Option<String> {
        let v: Value = serde_json::from_str(line).unwrap_or(Value::Null);
        match self.provider {
            Provider::Claude => match v.get("type").and_then(Value::as_str) {
                Some("user") => {
                    self.outstanding += 1;
                    self.set_busy(true);
                }
                Some("control_response") => {
                    let id = v.pointer("/response/request_id").and_then(Value::as_str).unwrap_or_default();
                    self.pending.retain(|(p, _)| p != id);
                }
                _ => {}
            },
            Provider::Codex => {
                let method = v.get("method").and_then(Value::as_str);
                let id = v.get("id").filter(|i| !i.is_null()).cloned();
                match (id, method) {
                    // The app-server takes one initialize: a later window gets the first answer.
                    (Some(id), Some("initialize")) => match &self.init {
                        Some(result) => {
                            let reply = json!({"id": id, "result": result}).to_string();
                            self.send_client(&reply);
                            return None;
                        }
                        None => self.init_request = Some(id),
                    },
                    (None, Some("initialized")) => {
                        if self.initialized {
                            return None;
                        }
                        self.initialized = true;
                    }
                    // The window answered an approval.
                    (Some(id), None) => {
                        let key = id.to_string();
                        self.pending.retain(|(p, _)| *p != key);
                    }
                    _ => {}
                }
            }
        }
        Some(line.to_owned())
    }

    /// Hello, the turn so far and the open approvals, for a window that just connected.
    fn greeting(&self) -> Vec<String> {
        let hello = json!({"stepan_host": {"busy": self.busy, "native_id": self.meta.native_id}}).to_string();
        std::iter::once(hello).chain(self.turn.iter().cloned()).chain(self.pending.iter().map(|(_, l)| l.clone())).collect()
    }
}

/// The host: `stepan_verify agent-host <dir>`. Returns when the agent exits.
pub fn serve(dir: &Path) -> Result<()> {
    let spec: Spec = serde_json::from_slice(&std::fs::read(dir.join("spec.json")).context("spec.json")?)?;
    let provider = match spec.provider.as_str() {
        "claude" => Provider::Claude,
        "codex" => Provider::Codex,
        other => bail!("неизвестный агент {other}"),
    };
    let grace = spec.grace_secs.map_or(GRACE, Duration::from_secs);
    let _ = std::fs::remove_file(&spec.sock);
    let listener = UnixListener::bind(&spec.sock).with_context(|| format!("сокет {}", spec.sock.display()))?;

    let mut child: Child = Command::new(&spec.program)
        .args(&spec.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("не удалось запустить `{}`", spec.program))?;
    let stdin: Arc<Mutex<Option<ChildStdin>>> = Arc::new(Mutex::new(child.stdin.take()));
    let stdout = child.stdout.take().context("stdout агента")?;

    let meta = Meta {
        pid: std::process::id(),
        provider: spec.provider.clone(),
        native_id: None,
        sock: spec.sock.clone(),
        cwd: std::env::current_dir().unwrap_or_default(),
        busy: false,
        started: now_secs(),
    };
    let state = Arc::new(Mutex::new(State {
        provider,
        dir: dir.to_owned(),
        meta,
        client: None,
        next_client: 0,
        outstanding: 0,
        busy: false,
        turn: Vec::new(),
        pending: Vec::new(),
        init: None,
        init_request: None,
        initialized: false,
        last_output: Instant::now(),
    }));
    state.lock().unwrap().save();

    // Agent → window.
    {
        let state = state.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let mut s = state.lock().unwrap();
                s.on_output(&line);
                s.send_client(&line);
            }
        });
    }

    // Windows connect; the newest one is the one we talk to.
    {
        let state = state.clone();
        let stdin = stdin.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let Ok(reader) = stream.try_clone() else { continue };
                let n = {
                    let mut s = state.lock().unwrap();
                    s.next_client += 1;
                    let n = s.next_client;
                    s.client = Some((n, stream));
                    for line in s.greeting() {
                        s.send_client(&line);
                    }
                    n
                };
                let state = state.clone();
                let stdin = stdin.clone();
                std::thread::spawn(move || {
                    for line in BufReader::new(reader).lines().map_while(Result::ok) {
                        let forward = state.lock().unwrap().on_input(&line);
                        let Some(line) = forward else { continue };
                        let mut guard = stdin.lock().unwrap();
                        if let Some(w) = guard.as_mut() {
                            if w.write_all(line.as_bytes()).and_then(|_| w.write_all(b"\n")).and_then(|_| w.flush()).is_err() {
                                *guard = None;
                            }
                        }
                    }
                    let mut s = state.lock().unwrap();
                    if s.client.as_ref().is_some_and(|(c, _)| *c == n) {
                        s.client = None;
                    }
                });
            }
        });
    }

    // Let the agent go once nobody needs it.
    let mut alone_idle: Option<Instant> = None;
    let mut closed_at: Option<Instant> = None;
    loop {
        if child.try_wait().ok().flatten().is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
        let (alone, idle) = {
            let s = state.lock().unwrap();
            let quiet = s.last_output.elapsed() > STUCK;
            (s.client.is_none(), s.pending.is_empty() && (!s.busy || quiet))
        };
        if alone && idle {
            let since = *alone_idle.get_or_insert_with(Instant::now);
            if since.elapsed() >= grace && closed_at.is_none() {
                // End of input: the agent finishes and exits by itself.
                stdin.lock().unwrap().take();
                closed_at = Some(Instant::now());
            }
        } else {
            alone_idle = None;
        }
        if closed_at.is_some_and(|t| t.elapsed() > Duration::from_secs(30)) {
            let _ = child.kill();
        }
    }
    let _ = child.wait();
    let _ = std::fs::remove_file(&spec.sock);
    let _ = std::fs::remove_dir_all(dir);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake agent that prints back every line it gets.
    fn echo_host(provider: &str, grace: u64) -> (PathBuf, PathBuf, std::thread::JoinHandle<Result<()>>) {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("stepan-host-test-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sock = std::env::temp_dir().join(format!("sh-{}-{n}.sock", std::process::id()));
        let spec = Spec {
            provider: provider.into(),
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "while IFS= read -r l; do printf '%s\\n' \"$l\"; done".into()],
            sock: sock.clone(),
            grace_secs: Some(grace),
        };
        std::fs::write(dir.join("spec.json"), serde_json::to_vec(&spec).unwrap()).unwrap();
        let d = dir.clone();
        let handle = std::thread::spawn(move || serve(&d));
        let deadline = Instant::now() + Duration::from_secs(5);
        while UnixStream::connect(&sock).is_err() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        (dir, sock, handle)
    }

    fn read_until(conn: &mut Conn, needle: &str) -> Vec<String> {
        let reader = conn.reader.as_mut().unwrap();
        let mut seen = Vec::new();
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap() == 0 {
                panic!("host closed before {needle}: {seen:?}");
            }
            let done = line.contains(needle);
            seen.push(line.trim_end().to_owned());
            if done {
                return seen;
            }
        }
    }

    #[test]
    fn a_window_that_comes_back_gets_the_turn_and_the_open_approval() {
        let (dir, sock, host) = echo_host("claude", 1);
        let mut first = Conn::connect(&sock).unwrap();
        assert!(!first.busy);
        first.write_value(&json!({"type":"system","subtype":"init","session_id":"s-1"})).unwrap();
        first.write_value(&json!({"type":"user","message":{"content":"сделай"}})).unwrap();
        first.write_value(&json!({"type":"assistant","uuid":"a1"})).unwrap();
        first.write_value(&json!({"type":"control_request","request_id":"r1","request":{"subtype":"can_use_tool"}})).unwrap();
        read_until(&mut first, "\"r1\"");
        drop(first);

        // The window closed: the agent is still mid-turn and waits for an answer.
        let meta: Meta = serde_json::from_slice(&std::fs::read(dir.join("meta.json")).unwrap()).unwrap();
        assert_eq!((meta.native_id.as_deref(), meta.busy), (Some("s-1"), true));
        let mut again = Conn::connect(&sock).unwrap();
        assert!(again.busy, "the hello says the agent is mid-turn");
        let replay = read_until(&mut again, "\"r1\"");
        assert!(replay.iter().any(|l| l.contains("\"a1\"")), "the turn so far: {replay:?}");

        // Answered and finished: nothing to replay any more.
        again.write_value(&json!({"type":"control_response","response":{"request_id":"r1"}})).unwrap();
        again.write_value(&json!({"type":"result"})).unwrap();
        read_until(&mut again, "\"result\"");
        drop(again);
        let mut third = Conn::connect(&sock).unwrap();
        assert!(!third.busy);
        third.write_value(&json!({"type":"ping"})).unwrap();
        let lines = read_until(&mut third, "ping");
        assert_eq!(lines.len(), 1, "no stale turn or approval: {lines:?}");
        drop(third);

        // Idle and alone past the grace: the agent and the host end, the registry is gone.
        host.join().unwrap().unwrap();
        assert!(!dir.exists() && !sock.exists());
    }

    #[test]
    fn codex_initialize_is_answered_once_per_server() {
        let (_dir, sock, host) = echo_host("codex", 1);
        let mut first = Conn::connect(&sock).unwrap();
        first.write_value(&json!({"id": 7, "method": "initialize", "params": {}})).unwrap();
        read_until(&mut first, "initialize");
        // The echo agent's «answer»: the same id with a result.
        first.write_value(&json!({"id": 7, "result": {"userAgent": "codex"}})).unwrap();
        read_until(&mut first, "userAgent");
        drop(first);

        let mut second = Conn::connect(&sock).unwrap();
        second.write_value(&json!({"id": 900, "method": "initialize", "params": {}})).unwrap();
        let reply = read_until(&mut second, "900");
        assert!(reply[0].contains("userAgent") && !reply[0].contains("method"), "answered by the host: {reply:?}");
        drop(second);
        host.join().unwrap().unwrap();
    }
}
