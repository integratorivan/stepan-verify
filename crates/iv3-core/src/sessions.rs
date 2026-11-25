//! Read-only index of agent sessions that already exist on disk, so the
//! dashboard can show and resume sessions started anywhere (CLI, t3code,
//! Codex Desktop), not only the ones iv3 launched.
//!
//! Claude Code: ~/.claude/projects/<escaped-cwd>/<session-id>.jsonl
//! Codex:       ~/.codex/sessions/YYYY/MM/DD/rollout-<ts>-<id>.jsonl
//!              plus ~/.codex/session_index.jsonl for thread names.

use std::{
    collections::HashMap,
    fs::{self, File},
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, Deserialize)]
pub enum Provider {
    Claude,
    Codex,
}

impl Provider {
    pub fn label(self) -> &'static str {
        match self {
            Provider::Claude => "Claude",
            Provider::Codex => "Codex",
        }
    }
}

#[derive(Clone, Debug)]
pub struct SessionSummary {
    pub provider: Provider,
    pub id: String,
    pub title: String,
    pub cwd: Option<PathBuf>,
    pub updated_at: DateTime<Utc>,
    pub file: PathBuf,
}

impl SessionSummary {
    /// Shell command that resumes this session in its own CLI.
    pub fn resume_command(&self) -> Vec<String> {
        match self.provider {
            Provider::Claude => vec!["claude".into(), "--resume".into(), self.id.clone()],
            Provider::Codex => vec!["codex".into(), "resume".into(), self.id.clone()],
        }
    }

    /// Last path segment of the working directory, used as the project name.
    pub fn project(&self) -> String {
        self.cwd
            .as_deref()
            .and_then(Path::file_name)
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "—".into())
    }
}

/// All sessions touched within `max_age`, newest first.
pub fn scan_all(max_age: Duration) -> Vec<SessionSummary> {
    let cutoff = SystemTime::now() - max_age;
    let home = dirs::home_dir().unwrap_or_default();
    let mut out = scan_claude(&home.join(".claude/projects"), cutoff);
    out.extend(scan_codex(&home.join(".codex"), cutoff));
    out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    out
}

fn modified(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|m| m.modified()).ok()
}

// ---------------------------------------------------------------- Claude Code

pub fn scan_claude(projects: &Path, cutoff: SystemTime) -> Vec<SessionSummary> {
    let mut out = Vec::new();
    for dir in fs::read_dir(projects).into_iter().flatten().flatten() {
        for entry in fs::read_dir(dir.path()).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|x| x != "jsonl") {
                continue;
            }
            let Some(mtime) = modified(&path).filter(|t| *t >= cutoff) else { continue };
            if let Some(s) = read_claude_session(&path, mtime) {
                out.push(s);
            }
        }
    }
    out
}

fn read_claude_session(path: &Path, mtime: SystemTime) -> Option<SessionSummary> {
    let reader = BufReader::new(File::open(path).ok()?);
    let mut ai_title = None;
    let mut first_prompt = None;
    let mut cwd = None;
    for line in reader.lines().map_while(Result::ok) {
        // Cheap prefilter: only parse the records we care about.
        let is_title = line.contains("\"type\":\"ai-title\"") || line.contains("\"type\":\"custom-title\"");
        let is_user = first_prompt.is_none() && line.contains("\"type\":\"user\"");
        if !is_title && !is_user {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
        if is_title {
            let t = v.get("customTitle").or_else(|| v.get("aiTitle")).and_then(Value::as_str);
            if let Some(t) = t {
                ai_title = Some(t.to_owned());
            }
        } else if let Some(text) = claude_user_text(&v) {
            cwd = v.get("cwd").and_then(Value::as_str).map(PathBuf::from);
            first_prompt = Some(text);
        }
    }
    // Sessions with no human prompt are hook/sidechain noise.
    let first_prompt = first_prompt?;
    Some(SessionSummary {
        provider: Provider::Claude,
        id: path.file_stem()?.to_string_lossy().into_owned(),
        title: ai_title.unwrap_or_else(|| one_line(&first_prompt)),
        cwd,
        updated_at: mtime.into(),
        file: path.to_owned(),
    })
}

fn claude_user_text(v: &Value) -> Option<String> {
    if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let content = v.get("message")?.get("content")?;
    let text = match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter(|p| p.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(" "),
        _ => return None,
    };
    let text = text.trim();
    // Skip tool results and injected command/system wrappers.
    (!text.is_empty() && !text.starts_with('<')).then(|| text.to_owned())
}

// ---------------------------------------------------------------------- Codex

#[derive(Deserialize)]
struct CodexIndexRow {
    id: String,
    thread_name: Option<String>,
}

pub fn scan_codex(codex_home: &Path, cutoff: SystemTime) -> Vec<SessionSummary> {
    let names: HashMap<String, String> = File::open(codex_home.join("session_index.jsonl"))
        .map(|f| {
            BufReader::new(f)
                .lines()
                .map_while(Result::ok)
                .filter_map(|l| serde_json::from_str::<CodexIndexRow>(&l).ok())
                .filter_map(|r| Some((r.id, r.thread_name?)))
                .collect()
        })
        .unwrap_or_default();

    let mut files = Vec::new();
    collect_rollouts(&codex_home.join("sessions"), cutoff, &mut files);

    files
        .into_iter()
        .filter_map(|(path, mtime)| {
            let meta = first_line_json(&path)?;
            let payload = meta.get("payload")?;
            let id = payload.get("id").and_then(Value::as_str)?.to_owned();
            let cwd = payload.get("cwd").and_then(Value::as_str).map(PathBuf::from);
            let title = names
                .get(&id)
                .cloned()
                .or_else(|| codex_first_prompt(&path))
                .unwrap_or_else(|| "Без названия".into());
            Some(SessionSummary { provider: Provider::Codex, id, title, cwd, updated_at: mtime.into(), file: path })
        })
        .collect()
}

fn collect_rollouts(dir: &Path, cutoff: SystemTime, out: &mut Vec<(PathBuf, SystemTime)>) {
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            // No pruning by directory mtime: a directory's mtime only moves when
            // entries are added, and old threads get resumed in place.
            collect_rollouts(&path, cutoff, out);
        } else if path.extension().is_some_and(|x| x == "jsonl") {
            if let Some(t) = modified(&path).filter(|t| *t >= cutoff) {
                out.push((path, t));
            }
        }
    }
}

/// First real user message in a rollout (skips injected `<environment_context>` etc.).
fn codex_first_prompt(path: &Path) -> Option<String> {
    let reader = BufReader::new(File::open(path).ok()?);
    for line in reader.lines().map_while(Result::ok).take(400) {
        if !line.contains("\"role\":\"user\"") {
            continue;
        }
        let v: Value = serde_json::from_str(&line).ok()?;
        let parts = v.pointer("/payload/content")?.as_array()?;
        for text in parts.iter().filter_map(|p| p.get("text").and_then(Value::as_str)) {
            let text = text.trim();
            if !text.is_empty() && !text.starts_with('<') {
                return Some(one_line(text));
            }
        }
    }
    None
}

fn first_line_json(path: &Path) -> Option<Value> {
    let mut line = String::new();
    BufReader::new(File::open(path).ok()?).read_line(&mut line).ok()?;
    serde_json::from_str(&line).ok()
}

fn one_line(s: &str) -> String {
    let line = s.lines().next().unwrap_or("").trim();
    match line.char_indices().nth(80) {
        Some((i, _)) => format!("{}…", &line[..i]),
        None => line.to_owned(),
    }
}
