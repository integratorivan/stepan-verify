//! Watch a session that runs elsewhere (a terminal, Codex Desktop): replay its
//! transcript file and keep reading what the other process appends. The first
//! message sent from here hands the session to the regular agent, which
//! resumes it with its own process.

use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::mpsc::{self, RecvTimeoutError},
    time::Duration,
};

use anyhow::{Context, Result};

use crate::{
    Command, Provider, SessionSpec,
    claude::Feed,
    model::{AgentEvent, Emitter, ImageStore, Item, ItemStatus},
    peek::CodexFeed,
};

const POLL: Duration = Duration::from_millis(700);
/// No writes for this long: the other process is gone or stuck, stop showing it as working.
const QUIET: Duration = Duration::from_secs(10 * 60);

enum Lines {
    Claude(Feed),
    Codex(CodexFeed),
}

impl Lines {
    fn push(&mut self, line: &str) -> Vec<Item> {
        match self {
            Lines::Claude(f) => f.push(line),
            Lines::Codex(f) => f.push(line).into_iter().collect(),
        }
    }

    fn running(&self) -> bool {
        match self {
            Lines::Claude(f) => f.running(),
            Lines::Codex(f) => f.running == Some(true),
        }
    }

    fn settle(&mut self) {
        match self {
            Lines::Claude(f) => f.settle(),
            Lines::Codex(f) => f.running = Some(false),
        }
    }

    fn items(&self) -> Vec<Item> {
        match self {
            Lines::Claude(f) => f.items.clone(),
            Lines::Codex(f) => f.items.clone(),
        }
    }
}

/// Complete lines appended to a file since the last read.
struct Tail {
    path: PathBuf,
    offset: u64,
    partial: Vec<u8>,
}

impl Tail {
    fn read(&mut self) -> Result<Vec<String>> {
        let mut file = File::open(&self.path).with_context(|| format!("{}", self.path.display()))?;
        let len = file.metadata()?.len();
        if len < self.offset {
            // Rewritten from scratch: start over.
            self.offset = 0;
            self.partial.clear();
        }
        if len == self.offset {
            return Ok(Vec::new());
        }
        file.seek(SeekFrom::Start(self.offset))?;
        let mut buf = Vec::new();
        file.take(len - self.offset).read_to_end(&mut buf)?;
        self.offset += buf.len() as u64;
        self.partial.extend_from_slice(&buf);
        let Some(end) = self.partial.iter().rposition(|b| *b == b'\n') else { return Ok(Vec::new()) };
        let rest = self.partial.split_off(end + 1);
        let done = std::mem::replace(&mut self.partial, rest);
        Ok(String::from_utf8_lossy(&done).lines().map(str::to_owned).collect())
    }
}

pub(crate) fn run(spec: SessionSpec, file: &Path, rx: mpsc::Receiver<Command>, events: &Emitter) -> Result<()> {
    let store = ImageStore { dir: spec.image_cache.clone() };
    let mut lines = match spec.provider {
        Provider::Claude => Lines::Claude(Feed::new(store)),
        Provider::Codex => Lines::Codex(CodexFeed::default()),
    };
    let mut tail = Tail { path: file.to_owned(), offset: 0, partial: Vec::new() };
    for line in tail.read()? {
        lines.push(&line);
    }
    let written = || std::fs::metadata(file).and_then(|m| m.modified()).ok();
    if written().is_some_and(|t| t.elapsed().unwrap_or_default() > QUIET) {
        lines.settle();
    }
    let mut running = lines.running();
    let mut history = lines.items();
    if !running {
        // Tools whose result never landed (interrupted turn) are not running anymore.
        for item in &mut history {
            if item.status == ItemStatus::Running {
                item.status = ItemStatus::Done;
            }
        }
    }
    if running {
        // Publish the turn before History can make the session look idle.
        events.emit(AgentEvent::TurnStarted);
    }
    events.emit(AgentEvent::History(history));

    loop {
        match rx.recv_timeout(POLL) {
            Ok(Command::Send(message)) => return take_over(spec, Command::Send(message), rx, events),
            Ok(Command::Shutdown) | Err(RecvTimeoutError::Disconnected) => return Ok(()),
            // Approvals and interrupts belong to the process that runs the session.
            Ok(Command::Approve { .. } | Command::Interrupt) => {}
            Err(RecvTimeoutError::Timeout) => {
                if running && written().is_some_and(|t| t.elapsed().unwrap_or_default() > QUIET) {
                    lines.settle();
                    running = false;
                    events.emit(AgentEvent::TurnCompleted { error: None });
                }
                for line in tail.read().unwrap_or_default() {
                    for item in lines.push(&line) {
                        events.emit(AgentEvent::Upsert(item));
                    }
                    let now = lines.running();
                    if now != running {
                        running = now;
                        events.emit(if now { AgentEvent::TurnStarted } else { AgentEvent::TurnCompleted { error: None } });
                    }
                }
            }
        }
    }
}

/// Hand the session to the regular agent: it reloads history and resumes.
fn take_over(spec: SessionSpec, first: Command, rx: mpsc::Receiver<Command>, events: &Emitter) -> Result<()> {
    let (tx, inner) = mpsc::channel();
    let _ = tx.send(first);
    std::thread::spawn(move || {
        while let Ok(cmd) = rx.recv() {
            if tx.send(cmd).is_err() {
                break;
            }
        }
    });
    match spec.provider {
        Provider::Claude => crate::claude::run(spec, inner, events),
        Provider::Codex => crate::codex::run(spec, inner, events),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;
    use crate::{AgentEvent, Mode, ItemKind};

    fn spec() -> SessionSpec {
        SessionSpec {
            provider: Provider::Claude,
            native_id: Some("s".into()),
            transcript: None,
            cwd: std::env::temp_dir(),
            mode: Mode::Ask,
            image_cache: None,
            env: Vec::new(),
            model: None,
            instructions: None,
        }
    }

    fn next(rx: &async_channel::Receiver<AgentEvent>) -> AgentEvent {
        for _ in 0..50 {
            if let Ok(e) = rx.try_recv() {
                return e;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("no event");
    }

    #[test]
    fn follows_appended_turn() {
        let dir = std::env::temp_dir().join(format!("stepan_verify-follow-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.jsonl");
        std::fs::write(&path, "{\"type\":\"user\",\"uuid\":\"u1\",\"message\":{\"role\":\"user\",\"content\":\"привет\"}}\n{\"type\":\"system\",\"subtype\":\"turn_duration\"}\n").unwrap();

        let (_handle, rx) = crate::open_following(spec(), path.clone());
        let AgentEvent::History(items) = next(&rx) else { panic!("history first") };
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, ItemKind::User);

        // A new turn written by "the terminal", the second record in two writes.
        let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(f, "{{\"type\":\"user\",\"uuid\":\"u2\",\"message\":{{\"role\":\"user\",\"content\":\"сделай\"}}}}").unwrap();
        write!(f, "{{\"type\":\"assistant\",\"uuid\":\"a1\",\"message\":{{\"id\":\"m1\",\"content\":[{{\"type\":\"text\",\"text\":\"готово\"}}],").unwrap();
        f.flush().unwrap();
        std::thread::sleep(POLL * 2);
        writeln!(f, "\"stop_reason\":\"end_turn\"}}}}").unwrap();

        let mut seen = Vec::new();
        while !matches!(seen.last(), Some(AgentEvent::TurnCompleted { .. })) {
            seen.push(next(&rx));
        }
        let kinds: Vec<String> = seen
            .iter()
            .map(|e| match e {
                AgentEvent::Upsert(i) => format!("{:?}:{}", i.kind, i.body),
                AgentEvent::TurnStarted => "start".into(),
                AgentEvent::TurnCompleted { .. } => "end".into(),
                _ => "other".into(),
            })
            .collect();
        assert_eq!(kinds, ["User:сделай", "start", "Assistant:готово", "end"]);

        // A background command keeps the agent working after its turn ends.
        writeln!(f, "{{\"type\":\"user\",\"uuid\":\"u3\",\"message\":{{\"role\":\"user\",\"content\":\"ещё\"}}}}").unwrap();
        writeln!(f, "{{\"type\":\"assistant\",\"uuid\":\"a3\",\"message\":{{\"id\":\"m3\",\"content\":[{{\"type\":\"tool_use\",\"id\":\"bg1\",\"name\":\"Bash\",\"input\":{{\"command\":\"sleep 10\",\"run_in_background\":true}}}}]}}}}").unwrap();
        writeln!(f, "{{\"type\":\"assistant\",\"uuid\":\"a4\",\"message\":{{\"id\":\"m4\",\"content\":[{{\"type\":\"text\",\"text\":\"жду\"}}],\"stop_reason\":\"end_turn\"}}}}").unwrap();
        f.flush().unwrap();
        std::thread::sleep(POLL * 3);
        let mut before = Vec::new();
        while let Ok(e) = rx.try_recv() {
            before.push(e);
        }
        assert!(before.iter().any(|e| matches!(e, AgentEvent::Upsert(i) if i.body == "жду")));
        assert!(!before.iter().any(|e| matches!(e, AgentEvent::TurnCompleted { .. })), "ended while the background command runs");
        writeln!(f, "{{\"type\":\"queue-operation\",\"content\":\"<task-notification>\\n<tool-use-id>bg1</tool-use-id>\\n<status>completed</status></task-notification>\"}}").unwrap();
        assert!(matches!(next(&rx), AgentEvent::TurnCompleted { .. }));

        // Woken by a background task: no human prompt, but the agent writes again.
        writeln!(f, "{{\"type\":\"user\",\"uuid\":\"n1\",\"message\":{{\"role\":\"user\",\"content\":\"<task-notification>done</task-notification>\"}}}}").unwrap();
        writeln!(f, "{{\"type\":\"assistant\",\"uuid\":\"a2\",\"message\":{{\"id\":\"m2\",\"content\":[{{\"type\":\"tool_use\",\"id\":\"t1\",\"name\":\"Bash\",\"input\":{{\"command\":\"ls\"}}}}],\"stop_reason\":\"tool_use\"}}}}").unwrap();
        let mut seen = Vec::new();
        while !matches!(seen.last(), Some(AgentEvent::TurnStarted)) {
            seen.push(next(&rx));
        }

        // Opening during the turn must never briefly expose an idle session.
        let (_reopened, rx) = crate::open_following(spec(), path.clone());
        assert!(matches!(next(&rx), AgentEvent::TurnStarted));
        assert!(matches!(next(&rx), AgentEvent::History(_)));
        std::fs::remove_dir_all(&dir).ok();
    }
}
