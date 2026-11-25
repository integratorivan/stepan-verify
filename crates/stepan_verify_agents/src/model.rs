#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ItemKind {
    User,
    Assistant,
    Reasoning,
    /// Shell command, file edit, MCP/tool call, web search…
    Tool,
    /// Something went wrong or a notice from the agent runtime.
    Notice,
    /// The agent's checklist (TodoWrite / update_plan / Codex plan). Body lines
    /// are `✓ step`, `▸ step` (current) or `○ step` (pending).
    Plan,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ItemStatus {
    Running,
    Done,
    Failed,
}

use std::{
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AttachmentKind {
    Image,
    File,
    Folder,
}

/// Something shown next to a message or tool call: a screenshot, a pasted
/// image, a mentioned file, a file the agent sent you.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Attachment {
    pub kind: AttachmentKind,
    pub path: PathBuf,
}

impl Attachment {
    /// Image if the extension says so, else a file.
    pub fn for_path(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let kind = if is_image_path(&path) { AttachmentKind::Image } else { AttachmentKind::File };
        Self { kind, path }
    }
    pub fn folder(path: impl Into<PathBuf>) -> Self {
        Self { kind: AttachmentKind::Folder, path: path.into() }
    }
    pub fn label(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| self.path.display().to_string())
    }
}

pub fn is_image_path(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref(),
        Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tiff" | "heic")
    )
}

/// A `/show-me` output: an absolute `show-me-*.html|htm|png|svg` path. Stepan
/// attaches these to the transcript and opens the HTML in its preview panel.
pub fn is_show_me(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
    path.is_absolute() && name.starts_with("show-me-") && matches!(ext.as_deref(), Some("html" | "htm" | "png" | "svg"))
}

/// `/show-me` files named in a shell command or a tool input, in order, once each.
pub fn show_me_attachments(text: &str) -> Vec<Attachment> {
    let mut out: Vec<Attachment> = Vec::new();
    let split = |c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '`' | '=' | '(' | ')' | ';' | '|' | '&' | '<' | '>' | ',');
    for token in text.split(split) {
        let path = Path::new(token);
        if is_show_me(path) && !out.iter().any(|a| a.path == path) {
            out.push(Attachment::for_path(path));
        }
    }
    out
}

/// Writes inline (base64) images to disk once, so the UI only handles paths.
#[derive(Clone, Debug, Default)]
pub(crate) struct ImageStore {
    pub dir: Option<PathBuf>,
}

impl ImageStore {
    pub fn store_base64(&self, media_type: &str, data: &str) -> Option<Attachment> {
        use base64::Engine as _;
        let dir = self.dir.as_ref()?;
        let bytes = base64::engine::general_purpose::STANDARD.decode(data.trim()).ok()?;
        let ext = match media_type {
            "image/jpeg" | "image/jpg" => "jpg",
            "image/gif" => "gif",
            "image/webp" => "webp",
            _ => "png",
        };
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        bytes.hash(&mut hasher);
        let path = dir.join(format!("{:016x}.{ext}", hasher.finish()));
        if !path.exists() {
            std::fs::create_dir_all(dir).ok()?;
            std::fs::write(&path, &bytes).ok()?;
        }
        Some(Attachment { kind: AttachmentKind::Image, path })
    }

    /// `data:image/png;base64,…` URLs.
    pub fn store_data_url(&self, url: &str) -> Option<Attachment> {
        let rest = url.strip_prefix("data:")?;
        let (meta, data) = rest.split_once(',')?;
        let media_type = meta.split(';').next().unwrap_or("image/png");
        self.store_base64(media_type, data)
    }
}

/// A unified-diff hunk set for one file.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FileDiff {
    pub path: String,
    /// Hunk lines prefixed with ' ', '+', '-' or '@@'.
    pub patch: String,
}

impl FileDiff {
    /// (added, removed) line counts.
    pub fn stats(&self) -> (usize, usize) {
        self.patch.lines().fold((0, 0), |(a, r), l| {
            if l.starts_with('+') && !l.starts_with("+++") {
                (a + 1, r)
            } else if l.starts_with('-') && !l.starts_with("---") {
                (a, r + 1)
            } else {
                (a, r)
            }
        })
    }
}

/// One row of the transcript.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Item {
    pub id: String,
    pub kind: ItemKind,
    /// Tool name / command line; empty for messages.
    pub title: String,
    /// Message markdown, or tool output / diff.
    pub body: String,
    pub status: ItemStatus,
    /// File edits made by this tool call.
    pub diffs: Vec<FileDiff>,
    /// Images and files shown with this item.
    pub attachments: Vec<Attachment>,
}

impl Item {
    pub fn message(id: impl Into<String>, kind: ItemKind, body: impl Into<String>) -> Self {
        Self { id: id.into(), kind, title: String::new(), body: body.into(), status: ItemStatus::Done, diffs: Vec::new(), attachments: Vec::new() }
    }
    pub fn with_attachments(mut self, attachments: Vec<Attachment>) -> Self {
        self.attachments = attachments;
        self
    }
    pub fn with_diffs(mut self, diffs: Vec<FileDiff>) -> Self {
        self.diffs = diffs;
        self
    }
    pub fn tool(id: impl Into<String>, title: impl Into<String>, body: impl Into<String>, status: ItemStatus) -> Self {
        Self { id: id.into(), kind: ItemKind::Tool, title: title.into(), body: body.into(), status, diffs: Vec::new(), attachments: Vec::new() }
    }
}

#[derive(Clone, Debug)]
pub struct Approval {
    pub id: String,
    pub title: String,
    pub detail: String,
}

#[derive(Clone, Debug)]
pub enum AgentEvent {
    /// Native session id is known (new sessions learn it after start).
    SessionId(String),
    /// Prior transcript of a resumed session.
    History(Vec<Item>),
    TurnStarted,
    /// Insert, or replace the item with the same id.
    Upsert(Item),
    /// Streaming text appended to an item's body.
    Append { id: String, text: String },
    ApprovalRequested(Approval),
    ApprovalResolved(String),
    TurnCompleted { error: Option<String> },
    Error(String),
    Exited,
}

pub(crate) struct Emitter(pub async_channel::Sender<AgentEvent>);

impl Emitter {
    pub fn emit(&self, event: AgentEvent) {
        let _ = self.0.send_blocking(event);
    }
}

/// Unified hunks (3 lines of context) between two texts.
pub(crate) fn unified(old: &str, new: &str) -> String {
    similar::TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(3)
        .to_string()
}
