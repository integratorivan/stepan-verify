//! Drives Claude Code and Codex as child processes and normalizes what they
//! do into one transcript model the UI renders.
//!
//! Claude: `claude -p --input-format stream-json --output-format stream-json`,
//!         permission prompts answered over stdio (`can_use_tool`).
//! Codex:  `codex app-server`, newline-delimited JSON-RPC.

mod claude;
mod codex;
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
    let (tx, rx) = mpsc::channel();
    let (events_tx, events_rx) = async_channel::unbounded();
    let events = model::Emitter(events_tx);
    std::thread::Builder::new()
        .name(format!("stepan_verify-{:?}", spec.provider).to_lowercase())
        .spawn(move || {
            let result = match spec.provider {
                Provider::Claude => claude::run(spec, rx, &events),
                Provider::Codex => codex::run(spec, rx, &events),
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

/// `STEPAN_VERIFY_DOCS` for the agent process unless the spec sets its own.
pub(crate) fn docs_env(cmd: &mut std::process::Command, spec: &SessionSpec) {
    if !spec.env.iter().any(|(k, _)| k == "STEPAN_VERIFY_DOCS") {
        cmd.env("STEPAN_VERIFY_DOCS", docs_dir());
    }
}

/// Sessions working on a board task (`STEPAN_VERIFY_TASK` in their env) may run
/// `stepan_verify task …` without asking and write the board in the stepan_verify home.
pub(crate) fn board_home(spec: &SessionSpec) -> Option<PathBuf> {
    spec.env.iter().any(|(k, _)| k == "STEPAN_VERIFY_TASK").then(stepan_verify_home)
}
