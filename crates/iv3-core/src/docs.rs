//! Documents in `~/.iv3/docs`: reports, specs and HTML write-ups made by
//! agents and by you. Metadata comes from YAML frontmatter (markdown) or
//! `<meta name="iv3:…">` tags (HTML); anything missing is guessed from the
//! path. App-owned state: `state/docs-read.json` (what you have read),
//! `state/docs-pins.json` (pinned) and `state/docs-given.jsonl` (which
//! document was handed to which agent).
//!
//! ```markdown
//! ---
//! title: Разбор /events/search
//! project: outpoll-web
//! author: codex            # claude | codex | ты | авто
//! session: 01a20e11-…      # session that wrote it
//! task: t-0418             # board task it belongs to
//! summary: ILIKE без индекса, варианты ускорения
//! pinned: true
//! ---
//! ```

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
    pub pinned: bool,
    pub modified: DateTime<Utc>,
    pub size: u64,
    pub words: usize,
    pub read: bool,
    pub given: usize,
}

impl Doc {
    /// "3 мин чтения" estimate.
    pub fn read_minutes(&self) -> usize {
        (self.words / 180).max(1)
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
        Err(_) => (None, body),
    }
}

/// HTML metadata: `<title>` and `<meta name="iv3:project" content="…">` tags.
pub fn html_front(text: &str) -> Front {
    let head = &text[..text.len().min(16 * 1024)];
    let meta = |name: &str| -> Option<String> {
        let needle = format!("name=\"iv3:{name}\"");
        let at = head.find(&needle)?;
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

/// Cheap fingerprint of the folder (files, sizes, mtimes) to notice changes.
pub fn fingerprint(home: &Home) -> u64 {
    let mut files = Vec::new();
    walk(&home.docs(), &mut files, 0);
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
            Some(Doc {
                read: read.get(&rel).is_some_and(|&t| t >= modified.timestamp()),
                pinned: front.pinned || pins.contains(&rel),
                given: given.get(&rel).copied().unwrap_or(0),
                path,
                rel,
                kind,
                title,
                project,
                author: front.author.as_deref().and_then(Author::parse),
                session: front.session.filter(|s| !s.is_empty()),
                task: front.task.filter(|s| !s.is_empty()),
                summary: front.summary,
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
    // `docs` is newest first, so the first index is the newest document.
    projects.sort_by(|a, b| docs[b.1[0]].modified.cmp(&docs[a.1[0]].modified));
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
            pinned,
            modified,
            size: 0,
            words: 0,
            read: false,
            given: 0,
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

        let (none, body) = split_front("# Просто заголовок\nтекст");
        assert!(none.is_none());
        assert_eq!(first_heading(body).as_deref(), Some("Просто заголовок"));
        assert_eq!(humanize("events-search_p95"), "Events search p95");
        assert_eq!(Author::parse("ты"), Some(Author::You));
        assert_eq!(Author::parse("autopilot"), Some(Author::Auto));
    }

    #[test]
    fn html_meta_tags() {
        let html = r#"<html><head><title>Карта &amp; архитектура</title><meta name="iv3:project" content="outpoll"><meta name="iv3:author" content="claude"></head>"#;
        let f = html_front(html);
        assert_eq!(f.title.as_deref(), Some("Карта & архитектура"));
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
            doc("a.md", Some("iv3"), at(26, 10), false),
            doc("b.md", Some("outpoll"), at(25, 10), true),
            doc("c.md", None, at(23, 10), false),
            doc("d.md", Some("iv3"), at(10, 10), false),
        ];
        let by_project = group_by_project(&docs);
        let names: Vec<&str> = by_project.iter().map(|g| g.0.as_str()).collect();
        assert_eq!(names, ["Закреплённые", "iv3", "outpoll", NO_PROJECT]);
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
        let root = std::env::temp_dir().join(format!("iv3-docs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let home = Home { root: root.clone() };
        fs::create_dir_all(home.docs().join("outpoll-web")).unwrap();
        fs::write(home.docs().join("README.md"), "# folder readme").unwrap();
        fs::write(home.docs().join("outpoll-web/p95.md"), "---\nauthor: codex\n---\n# Разбор p95\n\nодин два три").unwrap();
        fs::write(home.docs().join("map.html"), "<title>Карта</title><body>слово</body>").unwrap();
        let docs = scan(&home);
        assert_eq!(docs.len(), 2, "folder README is not a shelf document");
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
}
