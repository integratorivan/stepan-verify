//! Drives Claude Code and Codex as child processes and normalizes what they
//! do into one transcript model the UI renders.
//!
//! Claude: `claude -p --input-format stream-json --output-format stream-json`,
//!         permission prompts answered over stdio (`can_use_tool`).
//! Codex:  `codex app-server`, newline-delimited JSON-RPC.

mod claude;
mod codex;
mod follow;
mod model;
pub mod peek;

use std::{path::PathBuf, sync::mpsc};

pub use model::{AgentEvent, Approval, Attachment, AttachmentKind, FileDiff, Item, ItemKind, ItemStatus, is_image_path};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    Claude,
    Codex,
}

/// How much the agent may do without asking.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// Ask before every command and edit.
    Ask,
    /// File edits go through, commands ask.
    #[default]
    AutoEdits,
    /// Never ask.
    FullAccess,
}

#[derive(Clone, Debug)]
pub struct SessionSpec {
    pub provider: Provider,
    /// Existing session to resume (Claude session id / Codex thread id).
    pub native_id: Option<String>,
    /// Claude transcript file, read for history when resuming.
    pub transcript: Option<PathBuf>,
    pub cwd: PathBuf,
    pub mode: Mode,
    /// Where inline images (pasted screenshots, tool screenshots) are written.
    pub image_cache: Option<PathBuf>,
    /// Extra environment for the agent process (e.g. `STEPAN_VERIFY_TASK`).
    pub env: Vec<(String, String)>,
    /// Model override (`sonnet`, `gpt-5`…); the CLI default otherwise.
    pub model: Option<String>,
    /// Added to the agent's own system prompt: Claude `--append-system-prompt`,
    /// Codex `developerInstructions` (a resumed Codex thread keeps the ones it
    /// started with).
    pub instructions: Option<String>,
}

/// A message to the agent: text plus attached images (local files).
#[derive(Clone, Debug, Default)]
pub struct Outgoing {
    pub text: String,
    pub images: Vec<PathBuf>,
}

pub(crate) enum Command {
    Send(Outgoing),
    Interrupt,
    Approve { id: String, allow: bool },
    Shutdown,
}

/// Handle to a running session. Dropping it shuts the agent process down.
pub struct AgentHandle {
    tx: mpsc::Sender<Command>,
}

impl AgentHandle {
    pub fn send(&self, text: impl Into<String>) {
        self.send_message(Outgoing { text: text.into(), images: Vec::new() });
    }
    pub fn send_message(&self, message: Outgoing) {
        let _ = self.tx.send(Command::Send(message));
    }
    pub fn interrupt(&self) {
        let _ = self.tx.send(Command::Interrupt);
    }
    pub fn approve(&self, id: impl Into<String>, allow: bool) {
        let _ = self.tx.send(Command::Approve { id: id.into(), allow });
    }
}

impl Drop for AgentHandle {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Shutdown);
    }
}

/// Start (or resume) a session. History arrives first as `AgentEvent::History`.
pub fn open(spec: SessionSpec) -> (AgentHandle, async_channel::Receiver<AgentEvent>) {
    launch(spec, None)
}

/// Resume a session that may still be running elsewhere: its transcript
/// `file` is replayed and then followed until the first message sent from
/// here, which takes the session over.
pub fn open_following(spec: SessionSpec, file: PathBuf) -> (AgentHandle, async_channel::Receiver<AgentEvent>) {
    launch(spec, Some(file))
}

fn launch(spec: SessionSpec, follow: Option<PathBuf>) -> (AgentHandle, async_channel::Receiver<AgentEvent>) {
    let (tx, rx) = mpsc::channel();
    let (events_tx, events_rx) = async_channel::unbounded();
    let events = model::Emitter(events_tx);
    std::thread::Builder::new()
        .name(format!("stepan_verify-{:?}", spec.provider).to_lowercase())
        .spawn(move || {
            let result = match (&follow, spec.provider) {
                (Some(file), _) => follow::run(spec, file, rx, &events),
                (None, Provider::Claude) => claude::run(spec, rx, &events),
                (None, Provider::Codex) => codex::run(spec, rx, &events),
            };
            if let Err(err) = result {
                events.emit(AgentEvent::Error(format!("{err:#}")));
            }
            events.emit(AgentEvent::Exited);
        })
        .expect("spawn agent thread");
    (AgentHandle { tx }, events_rx)
}

/// Agents must not inherit the launching terminal's control variables (Rex,
/// tmux, …): their hooks would act on the user's terminal session.
pub(crate) fn scrub_env(cmd: &mut std::process::Command) {
    for (key, _) in std::env::vars_os() {
        let key = key.to_string_lossy();
        if key.starts_with("REX_") || key == "TMUX" || key == "TMUX_PANE" || key == "CLAUDECODE" {
            cmd.env_remove(key.as_ref());
        }
    }
}

/// The stepan_verify home (`STEPAN_VERIFY_HOME`, else `~/.stepan_verify`).
fn stepan_verify_home() -> PathBuf {
    std::env::var_os("STEPAN_VERIFY_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".stepan_verify"))
}

/// Where agents put documents for the stepan_verify «Документы» shelf. Every agent
/// process gets it as `STEPAN_VERIFY_DOCS`; the app picks new files up on its own.
pub(crate) fn docs_dir() -> PathBuf {
    stepan_verify_home().join("docs")
}

/// `STEPAN_VERIFY_DOCS` for the agent process unless the spec sets its own, and
/// `STEPAN_VERIFY_FLOWS` — the project's user-flow map, the shared product
/// vocabulary — when the project has one.
pub(crate) fn docs_env(cmd: &mut std::process::Command, spec: &SessionSpec) {
    if !spec.env.iter().any(|(k, _)| k == "STEPAN_VERIFY_DOCS") {
        cmd.env("STEPAN_VERIFY_DOCS", docs_dir());
    }
    // The flow-language folder; an older single-file map until Stepan migrates it.
    let flows = [spec.cwd.join(FLOWS_DIR), spec.cwd.join(FLOWS_FILE)].into_iter().find(|p| p.exists());
    if let Some(flows) = flows.filter(|_| !spec.env.iter().any(|(k, _)| k == "STEPAN_VERIFY_FLOWS")) {
        cmd.env("STEPAN_VERIFY_FLOWS", flows);
    }
    // `stepan_verify memory …` from any session: the app's own binary on PATH,
    // and the project its memory entries belong to (the cwd's folder name, as docs name it).
    let set = |key: &str| spec.env.iter().any(|(k, _)| k == key);
    if !set("PATH") {
        if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.to_owned())) {
            let path = std::env::var("PATH").unwrap_or_default();
            cmd.env("PATH", format!("{}:{path}", dir.display()));
        }
    }
    if !set("STEPAN_VERIFY_PROJECT") {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        if home.as_deref() != Some(spec.cwd.as_path()) {
            if let Some(name) = spec.cwd.file_name() {
                cmd.env("STEPAN_VERIFY_PROJECT", name);
            }
        }
    }
}

/// The project's flow map folder (flow language), relative to its root
/// (same as `stepan_verify_core::flows::DIR`).
pub const FLOWS_DIR: &str = ".stepan_verify/flows";
/// The older single-file map (`stepan_verify_core::flows::FILE`).
pub const FLOWS_FILE: &str = ".stepan_verify/flows.yaml";

/// Sessions working on a board task (`STEPAN_VERIFY_TASK` in their env) may run
/// `stepan_verify task …` without asking and write the board in the stepan_verify home.
pub(crate) fn board_home(spec: &SessionSpec) -> Option<PathBuf> {
    spec.env.iter().any(|(k, _)| k == "STEPAN_VERIFY_TASK").then(stepan_verify_home)
}
