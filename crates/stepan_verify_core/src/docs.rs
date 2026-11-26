//! Documents in `~/.stepan_verify/docs`: reports, specs and HTML write-ups made by
//! agents and by you. Metadata comes from YAML frontmatter (markdown) or
//! `<meta name="stepan_verify:…">` tags (HTML); anything missing is guessed from the
//! path. App-owned state: `state/docs-read.json` (what you have read),
//! `state/docs-pins.json` (pinned), `state/docs-given.jsonl` (which
//! document was handed to which agent), `state/doc-comments/` (your comments,
//! see [`crate::doc_comments`]) and `state/doc-versions/` (earlier versions,
//! stored by [`scan`], see [`crate::doc_versions`]).
//!
//! ```markdown
//! ---
//! title: Разбор /events/search
//! project: outpoll-web
//! kind: research           # facts | decision | research | handoff | briefing | note
//! status: open             # decisions: proposed | accepted | superseded
//! question: Убрать fuzzy?  # what the agent asks you; open until status says otherwise
//! author: codex            # claude | codex | ты | авто
//! session: 01a20e11-…      # session that wrote it
//! task: t-0418             # board task it belongs to
//! summary: ILIKE без индекса, варианты ускорения
//! pinned: true
//! ---
//! ```
//!
//! Per project (`docs/<project>/`): `project.md` is the one living facts
//! document, `decisions/` holds decisions, `research/` write-ups, `handoff/`
//! unfinished work. A missing `kind` is guessed from that layout. Agents get
//! the rules and the project's facts and decisions at start ([`agent_brief`]).

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    io::Write as _,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Datelike, Duration, Local, Utc};
use serde::{Deserialize, Serialize};

use crate::Home;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocKind {
    Markdown,
    Html,
    Text,
    Pdf,
    Image,
    Other,
}

impl DocKind {
    pub fn of(path: &Path) -> Self {
        let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        match ext.as_str() {
            "md" | "markdown" | "mdx" => Self::Markdown,
            "html" | "htm" => Self::Html,
            "txt" | "log" | "json" | "yaml" | "yml" | "toml" | "csv" => Self::Text,
            "pdf" => Self::Pdf,
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" => Self::Image,
            _ => Self::Other,
        }
    }

    /// Short mono label for the type chip.
    pub fn label(self) -> &'static str {
        match self {
            Self::Markdown => "md",
            Self::Html => "html",
            Self::Text => "txt",
            Self::Pdf => "pdf",
            Self::Image => "img",
            Self::Other => "file",
        }
    }

    pub fn readable(self) -> bool {
        matches!(self, Self::Markdown | Self::Html | Self::Text)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Author {
    Claude,
    Codex,
    You,
    Auto,
}

impl Author {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "claude" | "claude code" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "ты" | "я" | "me" | "you" | "human" | "user" => Some(Self::You),
            "авто" | "auto" | "automation" | "autopilot" => Some(Self::Auto),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::You => "ты",
            Self::Auto => "авто",
        }
    }
}

/// What a document is for (its `kind`), as opposed to its file format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DocType {
    /// `project.md`: the living facts of a project.
    Facts,
    Decision,
    Research,
    Handoff,
    Briefing,
    Note,
}

impl DocType {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "facts" | "project" | "фактура" | "проект" => Some(Self::Facts),
            "decision" | "adr" | "решение" => Some(Self::Decision),
            "research" | "investigation" | "analysis" | "разбор" | "исследование" => Some(Self::Research),
            "handoff" | "передача" => Some(Self::Handoff),
            "briefing" | "брифинг" => Some(Self::Briefing),
            "note" | "notes" | "заметка" => Some(Self::Note),
            _ => None,
        }
    }

    /// From the folder layout: `<project>/project.md`, `…/decisions/…` and so on.
    pub fn infer(rel: &str) -> Self {
        let parts: Vec<&str> = rel.split('/').collect();
        let file = parts.last().copied().unwrap_or_default().to_lowercase();
        let dirs = &parts[..parts.len().saturating_sub(1)];
        let has = |name: &str| dirs.iter().any(|d| d.eq_ignore_ascii_case(name));
        if parts.len() == 2 && file == "project.md" {
            Self::Facts
        } else if has("decisions") {
            Self::Decision
        } else if has("research") {
            Self::Research
        } else if has("handoff") {
            Self::Handoff
        } else if has("briefings") || file.starts_with("briefing") {
            Self::Briefing
        } else {
            Self::Note
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Facts => "фактура",
            Self::Decision => "решение",
            Self::Research => "разбор",
            Self::Handoff => "передача",
            Self::Briefing => "брифинг",
            Self::Note => "заметка",
        }
    }
}

/// Statuses that close a document's question (and retire a decision).
fn closed(status: &str) -> bool {
    matches!(
        status.trim().to_lowercase().as_str(),
        "answered" | "closed" | "done" | "accepted" | "rejected" | "superseded" | "отвечено" | "закрыт" | "принято" | "отклонено" | "заменено"
    )
}

/// Russian label for a decision status.
pub fn status_label(status: &str) -> Option<&'static str> {
    Some(match status.trim().to_lowercase().as_str() {
        "proposed" | "предложено" => "предложено",
        "accepted" | "принято" => "принято",
        "superseded" | "заменено" => "заменено",
        "rejected" | "отклонено" => "отклонено",
        "open" | "открыт" => "открыт",
        "answered" | "closed" | "отвечено" | "закрыт" => "отвечено",
        _ => return None,
    })
}

/// Frontmatter as written in the file; every field optional.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Front {
    pub title: Option<String>,
    pub project: Option<String>,
    pub author: Option<String>,
    pub session: Option<String>,
    pub task: Option<String>,
    pub summary: Option<String>,
    pub pinned: bool,
    pub kind: Option<String>,
    pub status: Option<String>,
    pub question: Option<String>,
    pub supersedes: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Doc {
    pub path: PathBuf,
    /// Path inside the docs folder, `/`-separated: the stable key for state.
    pub rel: String,
    pub kind: DocKind,
    pub title: String,
    pub project: Option<String>,
    pub author: Option<Author>,
    pub session: Option<String>,
    pub task: Option<String>,
    pub summary: Option<String>,
    pub doc_type: DocType,
    pub status: Option<String>,
    pub question: Option<String>,
    pub pinned: bool,
    pub modified: DateTime<Utc>,
    pub size: u64,
    pub words: usize,
    pub read: bool,
    pub given: usize,
    /// Open comment threads (`doc_comments`).
    pub comments: usize,
}

impl Doc {
    /// "3 мин чтения" estimate.
    pub fn read_minutes(&self) -> usize {
        (self.words / 180).max(1)
    }

    /// The question the agent asks you, while no status closes it.
    pub fn open_question(&self) -> Option<&str> {
        self.question.as_deref().filter(|q| !q.trim().is_empty() && !self.status.as_deref().is_some_and(closed))
    }

    /// A decision still in force (accepted or proposed).
    pub fn live_decision(&self) -> bool {
        self.doc_type == DocType::Decision
            && !self.status.as_deref().is_some_and(|s| matches!(s.trim().to_lowercase().as_str(), "superseded" | "rejected" | "заменено" | "отклонено"))
    }
}

/// Strip YAML frontmatter: (parsed frontmatter, body).
pub fn split_front(text: &str) -> (Option<Front>, &str) {
    let rest = text.strip_prefix('\u{feff}').unwrap_or(text);
    let Some(after) = rest.strip_prefix("---\n").or_else(|| rest.strip_prefix("---\r\n")) else { return (None, text) };
    let Some(end) = after.find("\n---") else { return (None, text) };
    let yaml = &after[..end];
    let body = after[end + 4..].trim_start_matches(['-']).trim_start_matches(['\r', '\n']);
    match serde_yaml::from_str::<Front>(yaml) {
        Ok(front) => (Some(front), body),
        // Agents write `title: Разбор: p95` — not YAML, but meant plainly.
        Err(_) => (Some(loose_front(yaml)), body),
    }
}

/// Frontmatter read line by line: `key: everything after the first colon`,
/// quotes and a trailing `  # comment` dropped.
fn loose_front(yaml: &str) -> Front {
    let mut f = Front::default();
    for line in yaml.lines().filter(|l| !l.starts_with([' ', '\t', '#', '-'])) {
        let Some((key, value)) = line.split_once(':') else { continue };
        let mut value = value.trim();
        if let Some(at) = value.find(" #").filter(|_| !value.starts_with(['"', '\''])) {
            value = value[..at].trim_end();
        }
        let value = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')).or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\''))).unwrap_or(value);
        let value = (!value.is_empty()).then(|| value.to_owned());
        match key.trim() {
            "title" => f.title = value,
            "project" => f.project = value,
            "author" => f.author = value,
            "session" => f.session = value,
            "task" => f.task = value,
            "summary" => f.summary = value,
            "kind" => f.kind = value,
            "status" => f.status = value,
            "question" => f.question = value,
            "supersedes" => f.supersedes = value,
            "pinned" => f.pinned = value.as_deref() == Some("true"),
            _ => {}
        }
    }
    f
}

/// Set (or add) one frontmatter field of a markdown document in place,
/// keeping everything else as written. A file without frontmatter gets one.
pub fn set_front_field(path: &Path, key: &str, value: &str) -> std::io::Result<()> {
    let text = fs::read_to_string(path)?;
    let line = format!("{key}: {value}");
    let rest = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let out = match rest.strip_prefix("---\n").and_then(|after| after.find("\n---").map(|end| (after, end))) {
        Some((after, end)) => {
            let yaml = &after[..end];
            let mut found = false;
            let mut lines: Vec<String> = yaml
                .lines()
                .map(|l| {
                    if l.split_once(':').is_some_and(|(k, _)| k.trim() == key) && !l.starts_with([' ', '\t']) {
                        found = true;
                        line.clone()
                    } else {
                        l.to_owned()
                    }
                })
                .collect();
            if !found {
                lines.push(line);
            }
            format!("---\n{}{}", lines.join("\n"), &after[end..])
        }
        None => format!("---\n{line}\n---\n\n{rest}"),
    };
    fs::write(path, out)
}

/// HTML metadata: `<title>` and `<meta name="stepan_verify:project" content="…">` tags.
pub fn html_front(text: &str) -> Front {
    let head = &text[..text.len().min(16 * 1024)];
    let meta = |name: &str| -> Option<String> {
        // `iv3:` is the old prefix, still found in older documents.
        let at = ["stepan_verify", "iv3"].iter().find_map(|prefix| head.find(&format!("name=\"{prefix}:{name}\"")))?;
        let tag_start = head[..at].rfind('<')?;
        let tag_end = at + head[at..].find('>')?;
        let tag = &head[tag_start..tag_end];
        let c = tag.find("content=\"")? + 9;
        let len = tag[c..].find('"')?;
        Some(unescape(&tag[c..c + len]))
    };
    let title = meta("title").or_else(|| {
        let lower = head.to_lowercase();
        let s = lower.find("<title>")? + 7;
        let e = lower[s..].find("</title>")?;
        Some(unescape(head[s..s + e].trim()))
    });
    Front {
        title: title.filter(|t| !t.is_empty()),
        project: meta("project"),
        author: meta("author"),
        session: meta("session"),
        task: meta("task"),
        summary: meta("summary"),
        pinned: meta("pinned").is_some_and(|v| v == "true"),
        kind: meta("kind"),
        status: meta("status"),
        question: meta("question"),
        supersedes: meta("supersedes"),
    }
}

fn unescape(s: &str) -> String {
    s.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'")
}

/// First `# heading` of a markdown body.
fn first_heading(body: &str) -> Option<String> {
    body.lines().find_map(|l| l.strip_prefix("# ").map(|t| t.trim().to_owned())).filter(|t| !t.is_empty())
}

/// "events-search_p95" → "Events search p95".
fn humanize(stem: &str) -> String {
    let words = stem.replace(['-', '_'], " ");
    let mut chars = words.trim().chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

/// A markdown section: the text under one heading (or before the first one).
#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    /// (level, text) of the heading opening the section; None for the preamble.
    pub heading: Option<(u8, String)>,
    pub text: String,
}

/// Split a markdown body into sections at `#`, `##` and `###` headings
/// (ignoring `#` inside fenced code). The table of contents is the list of
/// section headings; the reader renders one block per section to jump.
pub fn sections(body: &str) -> Vec<Section> {
    let mut out = vec![Section { heading: None, text: String::new() }];
    let mut fence = false;
    for line in body.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fence = !fence;
        }
        let heading = (!fence)
            .then(|| {
                let level = trimmed.chars().take_while(|c| *c == '#').count();
                ((1..=3).contains(&level) && trimmed[level..].starts_with(' '))
                    .then(|| (level as u8, trimmed[level..].trim().trim_end_matches('#').trim().to_owned()))
            })
            .flatten();
        match heading {
            Some(h) => out.push(Section { heading: Some(h), text: format!("{line}\n") }),
            None => {
                let last = out.last_mut().expect("preamble section");
                last.text.push_str(line);
                last.text.push('\n');
            }
        }
    }
    if out.len() > 1 && out[0].text.trim().is_empty() {
        out.remove(0);
    }
    out
}

/// Headings for the table of contents: (section index, level, text). An H1
/// opening the document is its title — the reader's header already shows it.
pub fn toc(sections: &[Section], _title: &str) -> Vec<(usize, u8, String)> {
    sections
        .iter()
        .enumerate()
        .filter_map(|(i, s)| s.heading.as_ref().map(|(l, t)| (i, *l, t.clone())))
        .filter(|(i, l, _)| !(*i == 0 && *l == 1))
        .collect()
}

/// Section text to render: an opening H1 is dropped (the header shows the title).
pub fn section_body(sections: &[Section], i: usize) -> &str {
    let s = &sections[i];
    match (&s.heading, i) {
        (Some((1, _)), 0) => s.text.split_once('\n').map_or("", |(_, rest)| rest),
        _ => &s.text,
    }
}

// ------------------------------------------------------------------ state

fn read_state_path(home: &Home) -> PathBuf {
    home.state().join("docs-read.json")
}

fn pins_path(home: &Home) -> PathBuf {
    home.state().join("docs-pins.json")
}

fn given_path(home: &Home) -> PathBuf {
    home.state().join("docs-given.jsonl")
}

/// rel path → modification time (unix secs) you last read.
pub fn load_read(home: &Home) -> HashMap<String, i64> {
    fs::read_to_string(read_state_path(home)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

/// Remember that the current version of `doc` is read.
pub fn mark_read(home: &Home, doc: &Doc) {
    let mut read = load_read(home);
    if read.get(&doc.rel) == Some(&doc.modified.timestamp()) {
        return;
    }
    read.insert(doc.rel.clone(), doc.modified.timestamp());
    let _ = fs::create_dir_all(home.state());
    if let Ok(text) = serde_json::to_string_pretty(&read) {
        let _ = fs::write(read_state_path(home), text);
    }
}

pub fn load_pins(home: &Home) -> HashSet<String> {
    fs::read_to_string(pins_path(home)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

/// Pin or unpin; returns the new state.
pub fn toggle_pin(home: &Home, rel: &str) -> bool {
    let mut pins = load_pins(home);
    let pinned = if pins.remove(rel) {
        false
    } else {
        pins.insert(rel.to_owned());
        true
    };
    let _ = fs::create_dir_all(home.state());
    let mut list: Vec<&String> = pins.iter().collect();
    list.sort();
    if let Ok(text) = serde_json::to_string_pretty(&list) {
        let _ = fs::write(pins_path(home), text);
    }
    pinned
}

/// One hand-off of a document to an agent.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Given {
    pub at: DateTime<Utc>,
    pub doc: String,
    /// `session:<native id>`, `new:claude`, `new:codex`, `task:<id>`, `copy`.
    pub target: String,
    /// Human name of the target ("Оптимизация /events/search").
    #[serde(default)]
    pub title: String,
    /// claude / codex when the target is an agent.
    #[serde(default)]
    pub agent: Option<String>,
}

pub fn log_given(home: &Home, given: &Given) {
    let _ = fs::create_dir_all(home.state());
    let Ok(line) = serde_json::to_string(given) else { return };
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(given_path(home)) {
        let _ = writeln!(f, "{line}");
    }
}

pub fn load_given(home: &Home) -> Vec<Given> {
    fs::read_to_string(given_path(home))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

// ------------------------------------------------------------------ scan

/// The folder's own README (root `README.md`) explains the folder; it is not
/// a document of the shelf.
fn is_folder_readme(rel: &str) -> bool {
    rel.eq_ignore_ascii_case("README.md")
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 6 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        if path.is_dir() {
            walk(&path, out, depth + 1);
        } else {
            out.push(path);
        }
    }
}

/// Cheap fingerprint of the folder (files, sizes, mtimes) and of the
/// comments on its documents, to notice changes.
pub fn fingerprint(home: &Home) -> u64 {
    let mut files = Vec::new();
    walk(&home.docs(), &mut files, 0);
    walk(&crate::doc_comments::root(home), &mut files, 0);
    files.iter().fold(files.len() as u64, |acc, p| {
        let m = fs::metadata(p).ok();
        let t = m.as_ref().and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis() as u64);
        let s = m.map_or(0, |m| m.len());
        acc.wrapping_mul(31).wrapping_add(t ^ s.rotate_left(17))
    })
}

/// All documents, newest first, with read/pin/given state applied.
pub fn scan(home: &Home) -> Vec<Doc> {
    let root = home.docs();
    let mut files = Vec::new();
    walk(&root, &mut files, 0);
    let read = load_read(home);
    let pins = load_pins(home);
    let mut given: HashMap<String, usize> = HashMap::new();
    for g in load_given(home) {
        *given.entry(g.doc).or_default() += 1;
    }
    let mut docs: Vec<Doc> = files
        .into_iter()
        .filter_map(|path| {
            let rel = path.strip_prefix(&root).ok()?.to_string_lossy().replace('\\', "/");
            if is_folder_readme(&rel) {
                return None;
            }
            let meta = fs::metadata(&path).ok()?;
            let modified: DateTime<Utc> = meta.modified().ok()?.into();
            let kind = DocKind::of(&path);
            let text = if kind.readable() && meta.len() < 4 * 1024 * 1024 { fs::read_to_string(&path).ok() } else { None };
            if let Some(t) = &text {
                crate::doc_versions::capture(home, &rel, t.as_bytes());
            }
            let (front, body) = match (&text, kind) {
                (Some(t), DocKind::Markdown | DocKind::Text) => {
                    let (f, b) = split_front(t);
                    (f.unwrap_or_default(), b.to_owned())
                }
                (Some(t), DocKind::Html) => (html_front(t), String::new()),
                _ => (Front::default(), String::new()),
            };
            let words = match (&text, kind) {
                (Some(t), DocKind::Html) => strip_tags(t).split_whitespace().count(),
                _ => body.split_whitespace().count(),
            };
            let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let title = front.title.clone().or_else(|| first_heading(&body)).unwrap_or_else(|| humanize(&stem));
            // docs/<project>/file.md: the folder names the project.
            let folder = rel.split_once('/').map(|(d, _)| d.to_owned());
            let project = front.project.clone().filter(|p| !p.trim().is_empty()).or(folder);
            let doc_type = front.kind.as_deref().and_then(DocType::parse).unwrap_or_else(|| DocType::infer(&rel));
            Some(Doc {
                read: read.get(&rel).is_some_and(|&t| t >= modified.timestamp()),
                pinned: front.pinned || pins.contains(&rel),
                given: given.get(&rel).copied().unwrap_or(0),
                comments: crate::doc_comments::open_count(home, &rel),
                path,
                rel,
                kind,
                title,
                project,
                author: front.author.as_deref().and_then(Author::parse),
                session: front.session.filter(|s| !s.is_empty()),
                task: front.task.filter(|s| !s.is_empty()),
                summary: front.summary,
                doc_type,
                status: front.status.filter(|s| !s.trim().is_empty()),
                question: front.question.filter(|s| !s.trim().is_empty()),
                modified,
                size: meta.len(),
                words,
            })
        })
        .collect();
    docs.sort_by(|a, b| b.modified.cmp(&a.modified));
    docs
}

fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let mut inside = false;
    for c in html.chars() {
        match c {
            '<' => inside = true,
            '>' => {
                inside = false;
                out.push(' ');
            }
            _ if !inside => out.push(c),
            _ => {}
        }
    }
    out
}

// ------------------------------------------------------------------ agent brief

/// How much of `project.md` goes into an agent's start context.
const FACTS_LIMIT: usize = 5000;
/// Decisions listed in the start context (newest first).
const DECISIONS_LIMIT: usize = 25;

/// The rules for writing documents, in every agent session Stepan starts.
/// `{docs}` is the docs folder, `{dir}` where this project's documents go,
/// `{author}` / `{session}` fill the frontmatter.
const RULES: &str = r#"# Документы Stepan

У пользователя есть полка документов Stepan — папка `{docs}` (она же `$STEPAN_VERIFY_DOCS`). Документы этого проекта лежат в `{dir}/`, пути ниже — от этой папки. Полку читают пользователь и следующие агенты, поэтому ты пишешь туда сам, без отдельной просьбы, когда это оправдано:

- **Разбор** — `research/<slug>.md`. Ты что-то исследовал: замеры, причина бага, сравнение вариантов, устройство чужого кода. Пиши, если результат пригодится позже и его нельзя восстановить из кода или git.
- **Решение** — `decisions/<ГГГГ-ММ-ДД>-<slug>.md`. В работе выбрали подход, библиотеку, схему данных или отказались от варианта. Что выбрали, какие варианты отвергли и почему, когда стоит пересмотреть. Решение — это выбор, а не перечень сделанного.
- **Вопрос пользователю** — развилка, которую должен решить он. Запиши варианты и свою рекомендацию в разбор, вопрос — в поле `question`, и задай его в чате. Когда ответ получен, поставь `status: answered` и запиши решение.
- **Передача** — `handoff/<slug>.md`, только если работа остаётся незаконченной: что сделано, что осталось, где остановился.

Как запускать и тестировать проект, где что лежит, грабли, поправки и запреты пользователя — это не документы, а память (`stepan_verify memory add`, см. «Память Stepan» выше). Не пиши: отчёт о проделанной работе, пересказ диффа, то, что уже есть в коде, README, CLAUDE.md или AGENTS.md. Один точный документ лучше пяти. Нечего записать — ничего не пиши. Если пользователь ведёт тебя по шагам, пиши документ по итогу, а не после каждого шага.

Формат — markdown с YAML-фронтматтером:

```markdown
---
title: Разбор /events/search — +40 ms к p95
kind: research          # research | decision | handoff
status: open            # решения: proposed | accepted | superseded; вопросы: open | answered
question: Убрать fuzzy-поиск или оставить с индексом? Я бы оставил: …
author: {author}
session: {session}
summary: Одна строка о сути
---
```

Пиши по-русски, коротко, с конкретикой: цифры, пути, команды, ссылки на файлы. Если новое решение отменяет старое — поставь старому `status: superseded`, а в новом укажи `supersedes: <файл>`. Создавай папки, если их нет.

Пользователь оставляет комментарии к документам полки — к цитате или ко всему документу. Прочитать: `stepan_verify doc comments <файл>`, ответить в треде: `stepan_verify doc reply <файл> <id> "текст"`. Закрывай тред (`stepan_verify doc resolve <файл> <id>`), только если пользователь попросил или комментарии пришли с пометкой, что они закрыты; не закрывай только потому, что ответил или поправил документ.
"#;

/// The text an agent session starts with: the rules plus what the shelf
/// already knows about the project (facts, live decisions, open questions).
/// `project` is the docs folder name (the cwd's last segment); None for a
/// session outside any project.
pub fn agent_brief(home: &Home, project: Option<&str>, author: &str, session: Option<&str>) -> String {
    let memory = crate::memory::Memory::load(home).brief(project);
    format!("{memory}\n{}", brief_from(&scan(home), &home.docs(), project, author, session))
}

fn brief_from(docs: &[Doc], root: &Path, project: Option<&str>, author: &str, session: Option<&str>) -> String {
    let dir = match project {
        Some(p) => root.join(p),
        None => root.to_owned(),
    };
    let session = session.map_or_else(|| "<id сессии, если знаешь: $CLAUDE_CODE_SESSION_ID или $CODEX_THREAD_ID>".to_owned(), str::to_owned);
    let mut out = RULES
        .replace("{docs}", &root.display().to_string())
        .replace("{dir}", &dir.display().to_string())
        .replace("{author}", author)
        .replace("{session}", &session);
    let Some(project) = project else { return out };
    let mine: Vec<&Doc> = docs.iter().filter(|d| d.project.as_deref() == Some(project)).collect();
    out.push_str(&format!("\n## Что полка уже знает о проекте {project}\n\n"));
    if mine.is_empty() {
        out.push_str("Пока ничего.\n");
        return out;
    }
    if let Some(facts) = mine.iter().find(|d| d.doc_type == DocType::Facts) {
        let text = fs::read_to_string(&facts.path).unwrap_or_default();
        let (_, body) = split_front(&text);
        let body = body.trim();
        out.push_str(&format!("### Фактура (`{}`)\n\n", facts.path.display()));
        if body.chars().count() > FACTS_LIMIT {
            let cut: String = body.chars().take(FACTS_LIMIT).collect();
            out.push_str(&format!("{cut}\n\n…обрезано, полный текст — в файле.\n\n"));
        } else {
            out.push_str(&format!("{body}\n\n"));
        }
    }
    let decisions: Vec<&&Doc> = mine.iter().filter(|d| d.live_decision()).take(DECISIONS_LIMIT).collect();
    if !decisions.is_empty() {
        out.push_str("### Решения в силе\n\nНе переобсуждай их без новой причины; нужное открой и прочитай целиком.\n\n");
        for d in decisions {
            out.push_str(&format!("- {}{} — `{}`\n", d.title, d.summary.as_deref().map(|s| format!(": {s}")).unwrap_or_default(), d.path.display()));
        }
        out.push('\n');
    }
    let questions: Vec<(&&Doc, &str)> = mine.iter().filter_map(|d| d.open_question().map(|q| (d, q))).collect();
    if !questions.is_empty() {
        out.push_str("### Открытые вопросы к пользователю\n\n");
        for (d, q) in questions {
            out.push_str(&format!("- {q} — `{}`\n", d.path.display()));
        }
        out.push('\n');
    }
    let commented: Vec<&&Doc> = mine.iter().filter(|d| d.comments > 0).collect();
    if !commented.is_empty() {
        out.push_str("### Документы с открытыми комментариями пользователя\n\nПрочитай: `stepan_verify doc comments <файл>`.\n\n");
        for d in commented {
            out.push_str(&format!("- {} — {} {} — `{}`\n", d.title, d.comments, threads_word(d.comments), d.path.display()));
        }
        out.push('\n');
    }
    let others: Vec<&&Doc> =
        mine.iter().filter(|d| matches!(d.doc_type, DocType::Research | DocType::Handoff) && d.open_question().is_none()).take(10).collect();
    if !others.is_empty() {
        out.push_str("### Последние разборы и передачи\n\n");
        for d in others {
            out.push_str(&format!("- {} ({}) — `{}`\n", d.title, d.doc_type.label(), d.path.display()));
        }
        out.push('\n');
    }
    out
}

/// "1 тред", "3 треда", "5 тредов".
fn threads_word(n: usize) -> &'static str {
    match (n % 10, n % 100) {
        (1, r) if r != 11 => "тред",
        (2..=4, r) if !(12..=14).contains(&r) => "треда",
        _ => "тредов",
    }
}

// ------------------------------------------------------------------ groups

/// Label of the group for documents without a project.
pub const NO_PROJECT: &str = "Общие";

/// Pinned first, then projects by their newest document, then documents
/// without a project. Inside a group: newest first.
pub fn group_by_project(docs: &[Doc]) -> Vec<(String, Vec<usize>)> {
    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
    let pinned: Vec<usize> = (0..docs.len()).filter(|&i| docs[i].pinned).collect();
    if !pinned.is_empty() {
        groups.push(("Закреплённые".into(), pinned));
    }
    let mut by: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut loose = Vec::new();
    for (i, d) in docs.iter().enumerate() {
        match &d.project {
            Some(p) => by.entry(p.clone()).or_default().push(i),
            None => loose.push(i),
        }
    }
    let mut projects: Vec<(String, Vec<usize>)> = by.into_iter().collect();
    // The project's facts open its group: it is the document you return to.
    for (_, list) in &mut projects {
        list.sort_by_key(|&i| docs[i].doc_type != DocType::Facts);
    }
    let newest = |list: &[usize]| list.iter().map(|&i| docs[i].modified).max();
    projects.sort_by_key(|g| std::cmp::Reverse(newest(&g.1)));
    groups.extend(projects);
    if !loose.is_empty() {
        groups.push((NO_PROJECT.into(), loose));
    }
    groups
}

/// Сегодня / Вчера / Эта неделя / Раньше, in local time.
pub fn group_by_time(docs: &[Doc], now: DateTime<Local>) -> Vec<(String, Vec<usize>)> {
    let today = now.date_naive();
    let week_start = today - Duration::days(now.weekday().num_days_from_monday() as i64);
    let mut buckets: [(&str, Vec<usize>); 4] = [("Сегодня", vec![]), ("Вчера", vec![]), ("Эта неделя", vec![]), ("Раньше", vec![])];
    for (i, d) in docs.iter().enumerate() {
        let day = d.modified.with_timezone(&Local).date_naive();
        let b = if day >= today {
            0
        } else if day == today - Duration::days(1) {
            1
        } else if day >= week_start {
            2
        } else {
            3
        };
        buckets[b].1.push(i);
    }
    buckets.into_iter().filter(|(_, v)| !v.is_empty()).map(|(l, v)| (l.to_owned(), v)).collect()
}

/// Compact time for a shelf row: "11:15", "вчера", "пт", "12 нояб".
pub fn short_time(t: DateTime<Utc>, now: DateTime<Local>) -> String {
    const DAYS: [&str; 7] = ["пн", "вт", "ср", "чт", "пт", "сб", "вс"];
    const MONTHS: [&str; 12] = ["янв", "фев", "мар", "апр", "мая", "июн", "июл", "авг", "сен", "окт", "нояб", "дек"];
    let local = t.with_timezone(&Local);
    let (day, today) = (local.date_naive(), now.date_naive());
    if day == today {
        local.format("%H:%M").to_string()
    } else if day == today - Duration::days(1) {
        "вчера".into()
    } else if (today - day).num_days() < 7 {
        DAYS[local.weekday().num_days_from_monday() as usize].into()
    } else {
        format!("{} {}", local.day(), MONTHS[local.month0() as usize])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn doc(rel: &str, project: Option<&str>, modified: DateTime<Utc>, pinned: bool) -> Doc {
        Doc {
            path: PathBuf::from(rel),
            rel: rel.into(),
            kind: DocKind::Markdown,
            title: rel.into(),
            project: project.map(str::to_owned),
            author: None,
            session: None,
            task: None,
            summary: None,
            doc_type: DocType::infer(rel),
            status: None,
            question: None,
            pinned,
            modified,
            size: 0,
            words: 0,
            read: false,
            given: 0,
            comments: 0,
        }
    }

    #[test]
    fn frontmatter_and_fallbacks() {
        let text = "---\ntitle: Разбор p95\nproject: outpoll-web\nauthor: codex\nsession: abc\npinned: true\n---\n# Другое\n\nТекст";
        let (front, body) = split_front(text);
        let front = front.unwrap();
        assert_eq!(front.title.as_deref(), Some("Разбор p95"));
        assert_eq!(front.project.as_deref(), Some("outpoll-web"));
        assert_eq!(Author::parse(front.author.as_deref().unwrap()), Some(Author::Codex));
        assert!(front.pinned);
        assert!(body.starts_with("# Другое"));

        // Colons inside values are not YAML but still meant plainly.
        let (front, _) = split_front("---\ntitle: Разбор: p95 вырос\nkind: research   # комментарий\nquestion: \"Убрать: fuzzy?\"\nstatus: open\n---\nтекст");
        let front = front.unwrap();
        assert_eq!(front.title.as_deref(), Some("Разбор: p95 вырос"));
        assert_eq!(front.kind.as_deref(), Some("research"));
        assert_eq!(front.question.as_deref(), Some("Убрать: fuzzy?"));
        assert_eq!(front.status.as_deref(), Some("open"));

        let (none, body) = split_front("# Просто заголовок\nтекст");
        assert!(none.is_none());
        assert_eq!(first_heading(body).as_deref(), Some("Просто заголовок"));
        assert_eq!(humanize("events-search_p95"), "Events search p95");
        assert_eq!(Author::parse("ты"), Some(Author::You));
        assert_eq!(Author::parse("autopilot"), Some(Author::Auto));
    }

    #[test]
    fn html_meta_tags() {
        let html = r#"<html><head><title>Карта &amp; архитектура</title><meta name="stepan_verify:project" content="outpoll"><meta name="stepan_verify:author" content="claude"></head>"#;
        let f = html_front(html);
        assert_eq!(f.title.as_deref(), Some("Карта & архитектура"));
        let old = html_front(r#"<meta name="iv3:project" content="legacy">"#);
        assert_eq!(old.project.as_deref(), Some("legacy"));
        assert_eq!(f.project.as_deref(), Some("outpoll"));
        assert_eq!(f.author.as_deref(), Some("claude"));
    }

    #[test]
    fn sections_and_toc() {
        let body = "# Разбор\nвступление\n## Замеры\n| a |\n```sh\n# не заголовок\n```\n## Причина\nтекст\n### Детали\n";
        let s = sections(body);
        let heads: Vec<_> = s.iter().map(|x| x.heading.clone().map(|h| h.1)).collect();
        assert_eq!(heads, [Some("Разбор".into()), Some("Замеры".into()), Some("Причина".into()), Some("Детали".into())]);
        assert!(s[1].text.contains("# не заголовок"));
        let t = toc(&s, "Разбор");
        assert_eq!(t.iter().map(|x| x.2.as_str()).collect::<Vec<_>>(), ["Замеры", "Причина", "Детали"]);
        assert_eq!(section_body(&s, 0).trim(), "вступление");
        assert!(section_body(&s, 1).starts_with("## Замеры"));
        assert_eq!(t[2].1, 3);
        // Preamble without heading stays as section 0.
        let s = sections("текст\n## A\nb");
        assert!(s[0].heading.is_none());
        assert_eq!(toc(&s, "x")[0].0, 1);
    }

    #[test]
    fn groups() {
        let now = Local.with_ymd_and_hms(2026, 11, 26, 15, 0, 0).unwrap(); // Thursday
        let at = |d: u32, h: u32| Local.with_ymd_and_hms(2026, 11, d, h, 0, 0).unwrap().with_timezone(&Utc);
        let docs = vec![
            doc("a.md", Some("stepan_verify"), at(26, 10), false),
            doc("b.md", Some("outpoll"), at(25, 10), true),
            doc("c.md", None, at(23, 10), false),
            doc("d.md", Some("stepan_verify"), at(10, 10), false),
        ];
        let by_project = group_by_project(&docs);
        let names: Vec<&str> = by_project.iter().map(|g| g.0.as_str()).collect();
        assert_eq!(names, ["Закреплённые", "stepan_verify", "outpoll", NO_PROJECT]);
        assert_eq!(by_project[1].1, [0, 3]);
        let by_time = group_by_time(&docs, now);
        let names: Vec<&str> = by_time.iter().map(|g| g.0.as_str()).collect();
        assert_eq!(names, ["Сегодня", "Вчера", "Эта неделя", "Раньше"]);
        assert_eq!(short_time(at(26, 10), now), "10:00");
        assert_eq!(short_time(at(25, 10), now), "вчера");
        assert_eq!(short_time(at(23, 10), now), "пн");
        assert_eq!(short_time(at(10, 10), now), "10 нояб");
    }

    #[test]
    fn scan_state_roundtrip() {
        let root = std::env::temp_dir().join(format!("stepan_verify-docs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let home = Home { root: root.clone() };
        fs::create_dir_all(home.docs().join("outpoll-web")).unwrap();
        fs::write(home.docs().join("README.md"), "# folder readme").unwrap();
        fs::write(home.docs().join("outpoll-web/p95.md"), "---\nauthor: codex\n---\n# Разбор p95\n\nодин два три").unwrap();
        fs::write(home.docs().join("map.html"), "<title>Карта</title><body>слово</body>").unwrap();
        let docs = scan(&home);
        assert_eq!(docs.len(), 2, "folder README is not a shelf document");
        assert_eq!(crate::doc_versions::list(&home, "outpoll-web/p95.md").len(), 1, "the first version is stored");
        scan(&home);
        assert_eq!(crate::doc_versions::list(&home, "outpoll-web/p95.md").len(), 1, "unchanged: no new version");
        let p95 = docs.iter().find(|d| d.rel == "outpoll-web/p95.md").unwrap();
        assert_eq!(p95.title, "Разбор p95");
        assert_eq!(p95.project.as_deref(), Some("outpoll-web"));
        assert_eq!(p95.author, Some(Author::Codex));
        assert!(!p95.read);
        mark_read(&home, p95);
        log_given(&home, &Given { at: Utc::now(), doc: p95.rel.clone(), target: "copy".into(), title: String::new(), agent: None });
        assert!(toggle_pin(&home, "map.html"));
        let docs = scan(&home);
        let p95 = docs.iter().find(|d| d.rel == "outpoll-web/p95.md").unwrap();
        assert!(p95.read);
        assert_eq!(p95.given, 1);
        assert!(docs.iter().find(|d| d.rel == "map.html").unwrap().pinned);
        assert!(!toggle_pin(&home, "map.html"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn front_field_edit() {
        let path = std::env::temp_dir().join(format!("stepan_verify-front-{}.md", std::process::id()));
        fs::write(&path, "---\ntitle: A\nstatus: open\nquestion: Да?\n---\n# A\n\nстатус: не трогать\n").unwrap();
        set_front_field(&path, "status", "answered").unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text, "---\ntitle: A\nstatus: answered\nquestion: Да?\n---\n# A\n\nстатус: не трогать\n");
        set_front_field(&path, "pinned", "true").unwrap();
        let (front, _) = split_front(&fs::read_to_string(&path).unwrap());
        assert!(front.unwrap().pinned);
        fs::write(&path, "# Без фронтматтера\n").unwrap();
        set_front_field(&path, "status", "accepted").unwrap();
        let text = fs::read_to_string(&path).unwrap();
        let (front, body) = split_front(&text);
        assert_eq!(front.unwrap().status.as_deref(), Some("accepted"));
        assert_eq!(body, "# Без фронтматтера\n");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn types_and_questions() {
        assert_eq!(DocType::infer("stepan_verify/project.md"), DocType::Facts);
        assert_eq!(DocType::infer("project.md"), DocType::Note);
        assert_eq!(DocType::infer("stepan_verify/decisions/2026-11-25-x.md"), DocType::Decision);
        assert_eq!(DocType::infer("stepan_verify/research/p95.md"), DocType::Research);
        assert_eq!(DocType::infer("briefing-24.md"), DocType::Briefing);
        assert_eq!(DocType::parse("Решение"), Some(DocType::Decision));
        let mut d = doc("stepan_verify/research/p95.md", Some("stepan_verify"), Utc::now(), false);
        d.question = Some("Убрать fuzzy?".into());
        assert_eq!(d.open_question(), Some("Убрать fuzzy?"));
        d.status = Some("answered".into());
        assert_eq!(d.open_question(), None);
        let mut dec = doc("stepan_verify/decisions/a.md", Some("stepan_verify"), Utc::now(), false);
        assert!(dec.live_decision());
        dec.status = Some("superseded".into());
        assert!(!dec.live_decision());
    }

    #[test]
    fn brief_lists_project_knowledge() {
        let root = std::env::temp_dir().join(format!("stepan_verify-brief-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let home = Home { root: root.clone() };
        let dir = home.docs().join("stepan_verify");
        fs::create_dir_all(dir.join("decisions")).unwrap();
        fs::create_dir_all(dir.join("research")).unwrap();
        fs::write(dir.join("project.md"), "---\ntitle: stepan_verify\n---\n## Запуск\n\ncargo run, STEPAN_VERIFY_HOME для тестов").unwrap();
        fs::write(dir.join("decisions/a.md"), "---\ntitle: WKWebView для HTML\nstatus: accepted\nsummary: нативно, без Electron\n---\n").unwrap();
        fs::write(dir.join("decisions/b.md"), "---\ntitle: Старое решение\nstatus: superseded\n---\n").unwrap();
        fs::write(dir.join("research/q.md"), "---\ntitle: Поиск\nquestion: Убрать fuzzy?\n---\n").unwrap();
        fs::write(home.docs().join("other.md"), "# Чужое").unwrap();
        crate::doc_comments::add(&home, "stepan_verify/research/q.md", crate::doc_comments::NewComment { text: "Уточни".into(), author: "ты".into(), ..Default::default() }).unwrap();

        let brief = agent_brief(&home, Some("stepan_verify"), "claude", Some("abc"));
        assert!(brief.contains("session: abc") && brief.contains("author: claude"));
        assert!(brief.starts_with("# Память Stepan") && brief.contains("stepan_verify memory add"), "memory comes first");
        assert!(brief.contains(&format!("лежат в `{}/`", dir.display())));
        assert!(brief.contains("STEPAN_VERIFY_HOME для тестов"), "facts body is included");
        assert!(brief.contains("WKWebView для HTML: нативно, без Electron"));
        assert!(!brief.contains("Старое решение"), "superseded decisions are left out");
        assert!(brief.contains("Убрать fuzzy?"));
        assert!(!brief.contains("Чужое"));
        assert!(brief.contains("stepan_verify doc reply") && brief.contains("Поиск — 1 тред"), "commented documents are listed");

        let empty = agent_brief(&home, Some("new-project"), "codex", None);
        assert!(empty.contains("Пока ничего") && empty.contains("$CODEX_THREAD_ID"));
        let loose = agent_brief(&home, None, "codex", None);
        assert!(!loose.contains("Что полка уже знает"));
        let _ = fs::remove_dir_all(root);
    }
}
