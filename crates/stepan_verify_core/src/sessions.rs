//! Read-only index of agent sessions that already exist on disk, so the
//! dashboard can show and resume sessions started anywhere (CLI, t3code,
//! Codex Desktop), not only the ones stepan_verify launched.
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

    /// Last path segment of the working directory, used as the project name
    /// (of the main checkout when the session ran in a linked worktree).
    pub fn project(&self) -> String {
        self.cwd.as_deref().and_then(crate::worktree::project_name).unwrap_or_else(|| "—".into())
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
    let names = codex_names(codex_home);
    let mut files = Vec::new();
    collect_rollouts(&codex_home.join("sessions"), cutoff, &mut files);
    codex_summaries_named(&names, files)
}

/// Thread names from ~/.codex/session_index.jsonl.
fn codex_names(codex_home: &Path) -> HashMap<String, String> {
    File::open(codex_home.join("session_index.jsonl"))
        .map(|f| {
            BufReader::new(f)
                .lines()
                .map_while(Result::ok)
                .filter_map(|l| serde_json::from_str::<CodexIndexRow>(&l).ok())
                .filter_map(|r| Some((r.id, r.thread_name?)))
                .collect()
        })
        .unwrap_or_default()
}

fn codex_summaries_named(names: &HashMap<String, String>, files: Vec<(PathBuf, SystemTime)>) -> Vec<SessionSummary> {
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

// ------------------------------------------------------------ indexed scan
//
// The plain scan reads every transcript of the last two weeks (gigabytes) on
// each start. The index in the store remembers, per file, its size, mtime and
// how far it was read; transcripts only grow, so a later scan reads just the
// new tail, and an unchanged file costs one stat.

use crate::store::{FileRow, Store, mtime_ns};

/// Sessions as the last scan saw them, without touching a transcript: what the
/// window shows the moment it opens.
pub fn cached(store: &Store, max_age: Duration) -> Vec<SessionSummary> {
    let cutoff = mtime_ns(SystemTime::now() - max_age);
    let mut out: Vec<SessionSummary> = store
        .files()
        .into_iter()
        .filter(|(_, row)| row.mtime_ns >= cutoff)
        .filter_map(|(path, row)| summary_of(path, &row))
        .collect();
    out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    out
}

/// Scan reading only what changed since the last scan, and save the index.
pub fn scan_indexed(store: &Store, max_age: Duration) -> Vec<SessionSummary> {
    let home = dirs::home_dir().unwrap_or_default();
    scan_indexed_in(store, &home.join(".claude/projects"), &home.join(".codex"), max_age)
}

fn scan_indexed_in(store: &Store, claude_projects: &Path, codex_home: &Path, max_age: Duration) -> Vec<SessionSummary> {
    let cutoff = SystemTime::now() - max_age;
    let prev = store.files();
    let mut rows: HashMap<PathBuf, FileRow> = HashMap::new();
    let stat = |path: &Path| fs::metadata(path).ok().and_then(|m| Some((m.modified().ok()?, m.len() as i64)));

    for dir in fs::read_dir(claude_projects).into_iter().flatten().flatten() {
        for entry in fs::read_dir(dir.path()).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|x| x != "jsonl") {
                continue;
            }
            let Some((mtime, size)) = stat(&path).filter(|(t, _)| *t >= cutoff) else { continue };
            let mtime = mtime_ns(mtime);
            let old = prev.get(&path).filter(|r| r.provider == Some(Provider::Claude));
            let mut row = match old {
                Some(r) if r.size == size && r.mtime_ns == mtime => r.clone(),
                // Grown: read on from where the last scan stopped.
                Some(r) if size >= r.size => r.clone(),
                _ => FileRow {
                    provider: Some(Provider::Claude),
                    id: path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
                    ..Default::default()
                },
            };
            if row.offset < size {
                read_claude_tail(&path, &mut row);
            }
            row.size = size;
            row.mtime_ns = mtime;
            rows.insert(path, row);
        }
    }

    let names = codex_names(codex_home);
    let mut files = Vec::new();
    collect_rollouts(&codex_home.join("sessions"), cutoff, &mut files);
    for (path, mtime) in files {
        let size = fs::metadata(&path).map_or(0, |m| m.len() as i64);
        let mtime = mtime_ns(mtime);
        let mut row = match prev.get(&path).filter(|r| r.provider == Some(Provider::Codex)) {
            Some(r) if r.size == size && r.mtime_ns == mtime => r.clone(),
            // Cheap to read again: the first line and a few hundred more at most.
            Some(r) if r.first_prompt.is_some() => FileRow { size, mtime_ns: mtime, ..r.clone() },
            _ => {
                let meta = first_line_json(&path);
                let payload = meta.as_ref().and_then(|m| m.get("payload"));
                FileRow {
                    provider: Some(Provider::Codex),
                    mtime_ns: mtime,
                    size,
                    offset: size,
                    id: payload.and_then(|p| p.get("id")).and_then(Value::as_str).unwrap_or_default().to_owned(),
                    title: None,
                    first_prompt: codex_first_prompt(&path),
                    cwd: payload.and_then(|p| p.get("cwd")).and_then(Value::as_str).map(PathBuf::from),
                }
            }
        };
        // Thread names change any time; take the current one.
        row.title = names.get(&row.id).cloned();
        rows.insert(path, row);
    }

    if let Err(err) = store.replace_files(&rows) {
        eprintln!("stepan_verify: session index: {err:#}");
    }
    let mut out: Vec<SessionSummary> = rows.into_iter().filter_map(|(path, row)| summary_of(path, &row)).collect();
    out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    out
}

/// Read a Claude transcript from `row.offset` to its last complete line,
/// picking up the first human prompt (and its cwd) and the latest title.
fn read_claude_tail(path: &Path, row: &mut FileRow) {
    use std::io::{Seek, SeekFrom};
    let Ok(mut file) = File::open(path) else { return };
    if file.seek(SeekFrom::Start(row.offset as u64)).is_err() {
        return;
    }
    let mut reader = BufReader::with_capacity(1 << 20, file);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let Ok(n) = reader.read_until(b'\n', &mut buf) else { break };
        // A line still being written is read next time, whole.
        if n == 0 || buf.last() != Some(&b'\n') {
            break;
        }
        row.offset += n as i64;
        let Ok(line) = std::str::from_utf8(&buf) else { continue };
        let is_title = line.contains("\"type\":\"ai-title\"") || line.contains("\"type\":\"custom-title\"");
        let is_user = row.first_prompt.is_none() && line.contains("\"type\":\"user\"");
        if !is_title && !is_user {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if is_title {
            if let Some(t) = v.get("customTitle").or_else(|| v.get("aiTitle")).and_then(Value::as_str) {
                row.title = Some(t.to_owned());
            }
        } else if let Some(text) = claude_user_text(&v) {
            row.cwd = v.get("cwd").and_then(Value::as_str).map(PathBuf::from);
            row.first_prompt = Some(one_line(&text));
        }
    }
}

fn summary_of(file: PathBuf, row: &FileRow) -> Option<SessionSummary> {
    let provider = row.provider?;
    if row.id.is_empty() {
        return None;
    }
    let title = match provider {
        // Sessions with no human prompt are hook/sidechain noise.
        Provider::Claude => row.title.clone().or_else(|| row.first_prompt.clone())?.clone(),
        Provider::Codex => row.title.clone().or_else(|| row.first_prompt.clone()).unwrap_or_else(|| "Без названия".into()),
    };
    if provider == Provider::Claude && row.first_prompt.is_none() {
        return None;
    }
    let secs = row.mtime_ns.div_euclid(1_000_000_000);
    let nanos = row.mtime_ns.rem_euclid(1_000_000_000) as u32;
    Some(SessionSummary {
        provider,
        id: row.id.clone(),
        title,
        cwd: row.cwd.clone(),
        updated_at: DateTime::from_timestamp(secs, nanos).unwrap_or_default(),
        file,
    })
}

/// Where Claude keeps the transcript of session `id` (any project folder).
pub fn claude_transcript(id: &str) -> Option<PathBuf> {
    let projects = dirs::home_dir()?.join(".claude/projects");
    fs::read_dir(projects).ok()?.flatten().map(|d| d.path().join(format!("{id}.jsonl"))).find(|p| p.is_file())
}

#[cfg(test)]
mod indexed_tests {
    use super::*;
    use std::io::Write as _;

    #[test]
    fn reads_only_the_new_tail_and_keeps_partial_lines_for_later() {
        let root = std::env::temp_dir().join(format!("stepan-index-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let project = root.join("claude/-x");
        fs::create_dir_all(&project).unwrap();
        let store = Store::open_at(&root.join("stepan.db")).unwrap();
        let file = project.join("s1.jsonl");
        let user = r#"{"type":"user","cwd":"/p/app","message":{"role":"user","content":"почини поиск"}}"#;
        fs::write(&file, format!("{user}\n")).unwrap();
        let day = Duration::from_secs(86_400);
        let scan = |store: &Store| scan_indexed_in(store, &root.join("claude"), &root.join("codex"), day);

        let first = scan(&store);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].title, "почини поиск");
        assert_eq!(first[0].cwd.as_deref(), Some(Path::new("/p/app")));

        // A title arrives, the next line is half written.
        let mut f = fs::OpenOptions::new().append(true).open(&file).unwrap();
        write!(f, "{}\n{}", r#"{"type":"ai-title","aiTitle":"Поиск событий"}"#, r#"{"type":"ai-ti"#).unwrap();
        drop(f);
        let second = scan(&store);
        assert_eq!(second[0].title, "Поиск событий");
        let row = &store.files()[&file];
        assert_eq!(row.offset as usize, user.len() + 1 + r#"{"type":"ai-title","aiTitle":"Поиск событий"}"#.len() + 1);

        // The half line completes; it is read whole now.
        let mut f = fs::OpenOptions::new().append(true).open(&file).unwrap();
        writeln!(f, "{}", r#"tle","aiTitle":"Поиск v2"}"#).unwrap();
        drop(f);
        assert_eq!(scan(&store)[0].title, "Поиск v2");
        assert_eq!(cached(&store, day)[0].title, "Поиск v2");

        // A deleted file leaves the index.
        fs::remove_file(&file).unwrap();
        assert!(scan(&store).is_empty());
        assert!(cached(&store, day).is_empty());
        let _ = fs::remove_dir_all(root);
    }
}
