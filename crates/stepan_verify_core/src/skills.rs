//! Every skill on this machine and how agents use it.
//!
//! Discovery walks the places Claude Code, Codex and stepan_verify load skills from
//! (following symlinks, so one skill shared through links shows up once with
//! all its locations). Usage comes from the agents' own transcripts:
//!
//! - Claude: an assistant `Skill` tool call (the model chose it) or a user
//!   `<command-name>/x</command-name>` record (you typed `/x`).
//! - Codex: a `{"type":"skill"}` item in a user message (you typed `$x`), or a
//!   command that reads `…/<x>/SKILL.md` — that is how Codex loads a skill it
//!   picked on its own (the skill list in its prompt only carries paths).
//!
//! Transcripts are large, so per-file results are cached by mtime/size in
//! `state/skills-usage.json` and only changed files are re-read.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Home, Provider, SessionSummary};

/// Transcripts older than this are not read (counts use 14 days; history 30).
const SCAN_DAYS: u64 = 30;
/// Claude Code rejects descriptions longer than this.
pub const DESCRIPTION_LIMIT: usize = 1024;
const CACHE_VERSION: u32 = 1;

// ------------------------------------------------------------------ discovery

/// Where a skill folder lives.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Root {
    StepanVerify,
    Claude,
    /// `~/.agents/skills`: read by Codex (and linked into Claude by hand).
    Agents,
    Codex,
    /// Codex's bundled skills (`~/.codex/skills/.system`).
    CodexSystem,
    /// `<project>/.claude/skills` and friends; the dir name tells who reads it.
    Project { project: String, dir: String },
}

impl Root {
    pub fn label(&self) -> String {
        match self {
            Root::StepanVerify => "~/.stepan_verify/skills".into(),
            Root::Claude => "~/.claude/skills".into(),
            Root::Agents => "~/.agents/skills".into(),
            Root::Codex => "~/.codex/skills".into(),
            Root::CodexSystem => "~/.codex/skills/.system".into(),
            Root::Project { project, dir } => format!("{project}/{dir}/skills"),
        }
    }

    fn claude_reads(&self) -> bool {
        matches!(self, Root::Claude) || matches!(self, Root::Project { dir, .. } if dir == ".claude")
    }

    fn codex_reads(&self) -> bool {
        matches!(self, Root::Codex | Root::CodexSystem | Root::Agents)
            || matches!(self, Root::Project { dir, .. } if dir == ".agents" || dir == ".codex")
    }
}

#[derive(Clone, Debug)]
pub struct Location {
    pub root: Root,
    /// The folder as listed in the root (may be a symlink).
    pub path: PathBuf,
    /// Where it really is, when it resolves.
    pub real: Option<PathBuf>,
    pub link: bool,
    pub has_skill_md: bool,
    pub empty: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Skill {
    pub name: String,
    pub description: String,
    /// `disable-model-invocation: true`: agents never pick it themselves.
    pub manual_only: bool,
    pub locations: Vec<Location>,
    pub claude: bool,
    pub codex: bool,
    pub stepan_verify: bool,
    /// Seen in transcripts but not on disk (plugin skills like `plugin:name`).
    pub plugin: bool,
}

impl Skill {
    /// The SKILL.md agents read (first location that has one).
    pub fn skill_md(&self) -> Option<PathBuf> {
        self.locations.iter().find(|l| l.has_skill_md).map(|l| l.real.clone().unwrap_or_else(|| l.path.clone()).join("SKILL.md"))
    }

    /// The real folder to show: first resolved location.
    pub fn folder(&self) -> Option<PathBuf> {
        self.locations.iter().find_map(|l| l.real.clone().or_else(|| Some(l.path.clone())))
    }
}

/// Folders to scan, in the order shown.
fn roots(home: &Home, projects: &[PathBuf]) -> Vec<(Root, PathBuf)> {
    let Some(user) = dirs::home_dir() else { return Vec::new() };
    let mut out = vec![
        (Root::StepanVerify, home.skills()),
        (Root::Claude, user.join(".claude/skills")),
        (Root::Agents, user.join(".agents/skills")),
        (Root::Codex, user.join(".codex/skills")),
        (Root::CodexSystem, user.join(".codex/skills/.system")),
    ];
    let mut seen = HashSet::new();
    for project in projects {
        if !seen.insert(project.clone()) || project == &user {
            continue;
        }
        let name = project.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        for dir in [".claude", ".agents", ".codex"] {
            let path = project.join(dir).join("skills");
            if path.is_dir() {
                out.push((Root::Project { project: name.clone(), dir: dir.into() }, path));
            }
        }
    }
    out
}

/// Project roots worth checking: session cwds, minus scratch and worktree dirs.
pub fn project_roots(sessions: &[SessionSummary]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for s in sessions {
        let Some(cwd) = &s.cwd else { continue };
        // A session in a worktree counts for its main checkout.
        let cwd = &crate::worktree::project_root(cwd);
        let text = cwd.to_string_lossy();
        if text.starts_with("/private/") || text.starts_with("/tmp") || text.contains("/worktrees/") || text.contains("/.t3/") {
            continue;
        }
        if !out.contains(cwd) {
            out.push(cwd.clone());
        }
    }
    out
}

#[derive(Default)]
struct Front {
    name: Option<String>,
    description: Option<String>,
    manual_only: bool,
}

/// YAML frontmatter of a SKILL.md.
fn frontmatter(text: &str) -> Front {
    let Some(rest) = text.strip_prefix("---") else { return Front::default() };
    let Some(end) = rest.find("\n---") else { return Front::default() };
    let yaml = &rest[..end];
    let mut f = Front::default();
    match serde_yaml::from_str::<serde_yaml::Value>(yaml) {
        Ok(v) => {
            let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(|x| x.trim().to_owned()).filter(|x| !x.is_empty());
            f.name = s("name");
            f.description = s("description");
            f.manual_only = v.get("disable-model-invocation").and_then(|x| x.as_bool()).unwrap_or(false);
        }
        Err(_) => {
            // Hand-written frontmatter that is not strict YAML: read the lines.
            for line in yaml.lines() {
                if let Some(d) = line.strip_prefix("description:") {
                    f.description = Some(d.trim().trim_matches('"').to_owned()).filter(|d| !d.is_empty());
                } else if let Some(n) = line.strip_prefix("name:") {
                    f.name = Some(n.trim().to_owned());
                } else if line.trim() == "disable-model-invocation: true" {
                    f.manual_only = true;
                }
            }
        }
    }
    f
}

/// All skills on disk, grouped by folder name.
pub fn discover(home: &Home, projects: &[PathBuf]) -> Vec<Skill> {
    let mut by_name: BTreeMap<String, Skill> = BTreeMap::new();
    for (root, dir) in roots(home, projects) {
        let Ok(entries) = fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let path = entry.path();
            let link = entry.file_type().map(|t| t.is_symlink()).unwrap_or(false);
            let real = fs::canonicalize(&path).ok();
            if real.as_ref().is_some_and(|r| !r.is_dir()) || (!link && !path.is_dir()) {
                continue; // a stray file, not a skill folder
            }
            let skill_md = path.join("SKILL.md");
            let has_skill_md = skill_md.is_file();
            let empty = real.as_ref().is_some_and(|r| fs::read_dir(r).map(|mut d| d.next().is_none()).unwrap_or(false));
            let skill = by_name.entry(name.clone()).or_insert_with(|| Skill { name: name.clone(), ..Default::default() });
            if has_skill_md && skill.description.is_empty() {
                if let Ok(text) = fs::read_to_string(&skill_md) {
                    let f = frontmatter(&text);
                    skill.description = f.description.unwrap_or_default();
                    skill.manual_only = f.manual_only;
                    let _ = f.name;
                }
            }
            if has_skill_md {
                skill.claude |= root.claude_reads();
                skill.codex |= root.codex_reads();
                skill.stepan_verify |= root == Root::StepanVerify;
            }
            skill.locations.push(Location { root: root.clone(), path, real, link, has_skill_md, empty });
        }
    }
    by_name.into_values().collect()
}

// ------------------------------------------------------------------ usage

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Method {
    /// The agent picked the skill itself.
    Auto,
    /// You asked for it: `/x` in Claude, `$x` in Codex.
    Command,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Use {
    pub skill: String,
    pub agent: Provider,
    pub session: String,
    pub at: DateTime<Utc>,
    pub method: Method,
}

/// Candidate uses in one Claude transcript (unfiltered: any `/command`).
pub fn claude_uses(path: &Path) -> Vec<Use> {
    let session = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let Ok(file) = fs::File::open(path) else { return Vec::new() };
    let mut out = Vec::new();
    let mut seen_tools = HashSet::new();
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let tool = line.contains("\"name\":\"Skill\"");
        let command = line.contains("<command-name>/");
        if !tool && !command {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
        let at = v.get("timestamp").and_then(Value::as_str).and_then(|t| t.parse::<DateTime<Utc>>().ok());
        let Some(at) = at else { continue };
        let message = v.get("message");
        match v.get("type").and_then(Value::as_str) {
            Some("assistant") if tool => {
                for item in message.and_then(|m| m.get("content")).and_then(Value::as_array).into_iter().flatten() {
                    if item.get("type").and_then(Value::as_str) != Some("tool_use") || item.get("name").and_then(Value::as_str) != Some("Skill") {
                        continue;
                    }
                    let id = item.get("id").and_then(Value::as_str).unwrap_or_default().to_owned();
                    let Some(skill) = item.get("input").and_then(|i| i.get("skill")).and_then(Value::as_str) else { continue };
                    if seen_tools.insert(id) {
                        out.push(Use { skill: skill.trim_start_matches('/').to_owned(), agent: Provider::Claude, session: session.clone(), at, method: Method::Auto });
                    }
                }
            }
            Some("user") if command => {
                // Only a real typed command: the record's text starts with the
                // command markup (quoted markup inside messages does not count).
                let text = match message.and_then(|m| m.get("content")) {
                    Some(Value::String(s)) => s.clone(),
                    Some(Value::Array(items)) => items.iter().filter_map(|i| i.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join(""),
                    _ => continue,
                };
                let head = text.trim_start();
                if !(head.starts_with("<command-message>") || head.starts_with("<command-name>")) {
                    continue;
                }
                if let Some(name) = between(head, "<command-name>/", "</command-name>") {
                    out.push(Use { skill: name.to_owned(), agent: Provider::Claude, session: session.clone(), at, method: Method::Command });
                }
            }
            _ => {}
        }
    }
    out
}

/// Candidate uses in one Codex rollout (unfiltered: any `…/x/SKILL.md` read).
pub fn codex_uses(path: &Path) -> Vec<Use> {
    let Ok(file) = fs::File::open(path) else { return Vec::new() };
    let mut session = String::new();
    let mut out = Vec::new();
    // One use per skill per turn: Codex often re-reads a SKILL.md in pieces.
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for (i, line) in BufReader::new(file).lines().map_while(Result::ok).enumerate() {
        if i == 0 || session.is_empty() && line.contains("\"session_meta\"") {
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                if v.get("type").and_then(Value::as_str) == Some("session_meta") {
                    let p = v.get("payload");
                    session = p.and_then(|p| p.get("id").or_else(|| p.get("session_id"))).and_then(Value::as_str).unwrap_or_default().to_owned();
                }
            }
            continue;
        }
        let explicit = line.contains("\"type\":\"skill\"");
        let read = line.contains("/SKILL.md") && line.contains("\"CommandExecution\"");
        if !(explicit || read) || !line.contains("\"item_completed\"") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
        let Some(at) = v.get("timestamp").and_then(Value::as_str).and_then(|t| t.parse::<DateTime<Utc>>().ok()) else { continue };
        let Some(p) = v.get("payload") else { continue };
        let turn = p.get("turn_id").and_then(Value::as_str).unwrap_or_default().to_owned();
        let Some(item) = p.get("item") else { continue };
        let mut push = |skill: String, method: Method, out: &mut Vec<Use>| {
            if seen.insert((turn.clone(), skill.clone())) {
                out.push(Use { skill, agent: Provider::Codex, session: session.clone(), at, method });
            }
        };
        match item.get("type").and_then(Value::as_str) {
            Some("UserMessage") => {
                for c in item.get("content").and_then(Value::as_array).into_iter().flatten() {
                    if c.get("type").and_then(Value::as_str) == Some("skill") {
                        if let Some(name) = c.get("name").and_then(Value::as_str) {
                            push(name.to_owned(), Method::Command, &mut out);
                        }
                    }
                }
            }
            Some("CommandExecution") => {
                let cmd = match item.get("command") {
                    Some(Value::Array(parts)) => parts.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" "),
                    Some(Value::String(s)) => s.clone(),
                    _ => continue,
                };
                for name in skill_reads(&cmd) {
                    push(name, Method::Auto, &mut out);
                }
            }
            _ => {}
        }
    }
    out
}

/// Skill names whose SKILL.md a shell command reads: `…/skills/<name>/SKILL.md`.
/// Memory folders (`…/memories/skills/…`) are notes, not skills.
fn skill_reads(cmd: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = cmd;
    while let Some(i) = rest.find("/SKILL.md") {
        let before = &rest[..i];
        let start = before.rfind(|c: char| c == '/' || c.is_whitespace() || c == '\'' || c == '"').map_or(0, |s| s + 1);
        let name = &before[start..];
        let path_start = before.rfind(|c: char| c.is_whitespace() || c == '\'' || c == '"').map_or(0, |s| s + 1);
        let path = &before[path_start..];
        if !name.is_empty() && path.contains("skills/") && !path.contains("/memories/") && !names.iter().any(|n| n == name) {
            names.push(name.to_owned());
        }
        rest = &rest[i + "/SKILL.md".len()..];
    }
    names
}

fn between<'a>(s: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let a = s.find(start)? + start.len();
    let b = s[a..].find(end)? + a;
    Some(s[a..b].trim())
}

// ------------------------------------------------------------------ cache

#[derive(Default, Serialize, Deserialize)]
struct Cache {
    version: u32,
    files: HashMap<String, CachedFile>,
}

#[derive(Serialize, Deserialize)]
struct CachedFile {
    mtime: u64,
    size: u64,
    uses: Vec<Use>,
}

fn transcripts(cutoff: SystemTime) -> Vec<(Provider, PathBuf)> {
    let Some(user) = dirs::home_dir() else { return Vec::new() };
    let mut out = Vec::new();
    let fresh = |p: &Path| fs::metadata(p).and_then(|m| m.modified()).map(|t| t >= cutoff).unwrap_or(false);
    for project in fs::read_dir(user.join(".claude/projects")).into_iter().flatten().flatten() {
        for f in fs::read_dir(project.path()).into_iter().flatten().flatten() {
            let p = f.path();
            if p.extension().is_some_and(|e| e == "jsonl") && fresh(&p) {
                out.push((Provider::Claude, p));
            }
        }
    }
    let mut stack = vec![user.join(".codex/sessions")];
    while let Some(dir) = stack.pop() {
        for e in fs::read_dir(&dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("rollout-")) && fresh(&p) {
                out.push((Provider::Codex, p));
            }
        }
    }
    out
}

/// All candidate uses from transcripts touched in the last 30 days, reusing the
/// cache for unchanged files.
fn usage(cache_path: &Path) -> Vec<Use> {
    let mut cache: Cache = fs::read_to_string(cache_path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .filter(|c: &Cache| c.version == CACHE_VERSION)
        .unwrap_or_default();
    let cutoff = SystemTime::now() - Duration::from_secs(SCAN_DAYS * 86_400);
    let mut files = HashMap::new();
    let mut changed = false;
    for (agent, path) in transcripts(cutoff) {
        let Ok(meta) = fs::metadata(&path) else { continue };
        let mtime = meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
        let key = path.to_string_lossy().into_owned();
        let entry = match cache.files.remove(&key) {
            Some(c) if c.mtime == mtime && c.size == meta.len() => c,
            _ => {
                changed = true;
                let uses = match agent {
                    Provider::Claude => claude_uses(&path),
                    Provider::Codex => codex_uses(&path),
                };
                CachedFile { mtime, size: meta.len(), uses }
            }
        };
        files.insert(key, entry);
    }
    changed |= !cache.files.is_empty(); // files dropped out of the window
    let all: Vec<Use> = files.values().flat_map(|f| f.uses.iter().cloned()).collect();
    if changed {
        let cache = Cache { version: CACHE_VERSION, files };
        if let Some(dir) = cache_path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        if let Ok(text) = serde_json::to_string(&cache) {
            let tmp = cache_path.with_extension("json.tmp");
            if fs::write(&tmp, text).is_ok() {
                let _ = fs::rename(&tmp, cache_path);
            }
        }
    }
    all
}

// ------------------------------------------------------------------ problems

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    High,
    Medium,
    Low,
}

#[derive(Clone, Debug)]
pub struct Problem {
    pub severity: Severity,
    pub skill: String,
    pub title: String,
    pub detail: String,
    /// What to do about it (shown, not run).
    pub fix: String,
    /// File or folder to open.
    pub path: Option<PathBuf>,
}

pub fn problems(skills: &[Skill]) -> Vec<Problem> {
    let mut out = Vec::new();
    for s in skills.iter().filter(|s| !s.plugin) {
        for l in &s.locations {
            let shown = tilde(&l.path);
            if l.link && l.real.is_none() {
                out.push(Problem {
                    severity: Severity::High,
                    skill: s.name.clone(),
                    title: "Битая ссылка".into(),
                    detail: format!("{shown} ведёт в никуда — {} этот скилл не видит.", reader(&l.root)),
                    fix: format!("Удали ссылку или перенаправь: rm {shown}"),
                    path: None,
                });
            } else if l.empty {
                out.push(Problem {
                    severity: Severity::Medium,
                    skill: s.name.clone(),
                    title: "Пустая папка".into(),
                    detail: format!("{shown} пустая."),
                    fix: format!("Удали папку или добавь SKILL.md: rmdir {shown}"),
                    path: Some(l.path.clone()),
                });
            } else if !l.has_skill_md {
                out.push(Problem {
                    severity: Severity::High,
                    skill: s.name.clone(),
                    title: "Нет SKILL.md — агенты не видят".into(),
                    detail: format!("В {shown} нет SKILL.md: {} пропускает такую папку.", reader(&l.root)),
                    fix: "Добавь SKILL.md с frontmatter name и description или перенеси папку из skills/.".into(),
                    path: Some(l.path.clone()),
                });
            }
        }

        // Several real folders for one name: copies, or worse, diverging versions.
        let mut reals: Vec<PathBuf> = s.locations.iter().filter(|l| l.has_skill_md).filter_map(|l| l.real.clone()).collect();
        reals.sort();
        reals.dedup();
        if reals.len() > 1 {
            let texts: Vec<Option<String>> = reals.iter().map(|r| fs::read_to_string(r.join("SKILL.md")).ok()).collect();
            let same = texts.windows(2).all(|w| w[0] == w[1]);
            let list = reals.iter().map(|r| tilde(r)).collect::<Vec<_>>().join(", ");
            out.push(Problem {
                severity: if same { Severity::Low } else { Severity::Medium },
                skill: s.name.clone(),
                title: if same { format!("Копии в {} местах", reals.len()) } else { "Версии расходятся".into() },
                detail: if same {
                    format!("Одинаковые копии: {list}. Правка в одной не попадёт в другую.")
                } else {
                    format!("Разные SKILL.md: {list}. Claude и Codex могут читать разные версии.")
                },
                fix: "Оставь одну папку (обычно ~/.agents/skills) и сделай на неё ссылки из ~/.claude/skills и ~/.codex/skills: ln -s ../../.agents/skills/<имя> …".into(),
                path: reals.first().map(|r| r.join("SKILL.md")),
            });
        }

        let has_md = s.locations.iter().any(|l| l.has_skill_md);
        let len = s.description.chars().count();
        if has_md && s.description.is_empty() {
            out.push(Problem {
                severity: Severity::Medium,
                skill: s.name.clone(),
                title: "Нет description".into(),
                detail: "Без описания агент не поймёт, когда звать скилл сам.".into(),
                fix: "Добавь description во frontmatter SKILL.md: что делает и когда использовать.".into(),
                path: s.skill_md(),
            });
        } else if len > DESCRIPTION_LIMIT {
            out.push(Problem {
                severity: Severity::High,
                skill: s.name.clone(),
                title: format!("Описание длиннее лимита: {len} из {DESCRIPTION_LIMIT}"),
                detail: "Claude Code отклоняет такой скилл.".into(),
                fix: "Сократи description, подробности перенеси в тело SKILL.md.".into(),
                path: s.skill_md(),
            });
        } else if len > 900 {
            out.push(Problem {
                severity: Severity::Low,
                skill: s.name.clone(),
                title: format!("Описание близко к лимиту: {len} из {DESCRIPTION_LIMIT}"),
                detail: "Ещё немного — и Claude Code перестанет загружать скилл.".into(),
                fix: "Сократи description, подробности перенеси в тело SKILL.md.".into(),
                path: s.skill_md(),
            });
        }

        if has_md && !s.claude && !s.codex && !s.stepan_verify {
            out.push(Problem {
                severity: Severity::Medium,
                skill: s.name.clone(),
                title: "Не виден ни одному агенту".into(),
                detail: "Папка лежит там, откуда скиллы никто не читает.".into(),
                fix: "Перенеси в ~/.agents/skills и сделай ссылку из ~/.claude/skills.".into(),
                path: s.folder(),
            });
        }
    }
    out.sort_by(|a, b| a.severity.cmp(&b.severity).then(a.skill.cmp(&b.skill)));
    out
}

fn reader(root: &Root) -> &'static str {
    match (root.claude_reads(), root.codex_reads()) {
        (true, true) => "ни Claude, ни Codex",
        (true, false) => "Claude",
        (false, true) => "Codex",
        _ => "агент",
    }
}

pub fn tilde(path: &Path) -> String {
    let s = path.display().to_string();
    match dirs::home_dir().map(|h| h.display().to_string()) {
        Some(h) if s.starts_with(&h) => format!("~{}", &s[h.len()..]),
        _ => s,
    }
}

// ------------------------------------------------------------------ report

pub struct Report {
    /// Disk skills plus plugin skills seen only in transcripts, by name.
    pub skills: Vec<Skill>,
    /// Uses of known skills, newest first.
    pub uses: Vec<Use>,
    pub problems: Vec<Problem>,
    /// Session titles and folders for the uses.
    pub sessions: HashMap<String, SessionSummary>,
    pub scanned_at: DateTime<Utc>,
}

impl Report {
    pub fn uses_of<'a>(&'a self, skill: &'a str) -> impl Iterator<Item = &'a Use> + 'a {
        self.uses.iter().filter(move |u| u.skill == skill)
    }
}

/// Full scan; slow on first run (reads every transcript of the last 30 days),
/// fast afterwards (cache). Run it off the UI thread.
pub fn scan(home: &Home) -> Report {
    let sessions = crate::sessions::scan_all(Duration::from_secs(SCAN_DAYS * 86_400));
    let mut skills = discover(home, &project_roots(&sessions));
    let known: HashSet<String> = skills.iter().map(|s| s.name.clone()).collect();
    let mut uses: Vec<Use> = usage(&home.state().join("skills-usage.json"))
        .into_iter()
        .filter(|u| {
            // Commands are only skills when a skill by that name exists; tool
            // calls always are (plugin skills have no local folder).
            known.contains(&u.skill) || (u.agent == Provider::Claude && u.method == Method::Auto)
        })
        .collect();
    uses.sort_by(|a, b| b.at.cmp(&a.at));
    for u in &uses {
        if !known.contains(&u.skill) && !skills.iter().any(|s| s.name == u.skill) {
            skills.push(Skill { name: u.skill.clone(), claude: true, plugin: true, ..Default::default() });
        }
    }
    let problems = problems(&skills);
    let sessions = sessions.into_iter().map(|s| (s.id.clone(), s)).collect();
    Report { skills, uses, problems, sessions, scanned_at: Utc::now() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("stepan_verify-skills-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn claude_tool_calls_and_typed_commands() {
        let d = tmp("claude");
        let f = d.join("abc.jsonl");
        let lines = [
            r#"{"type":"assistant","timestamp":"2026-11-24T10:00:00Z","message":{"content":[{"type":"tool_use","id":"t1","name":"Skill","input":{"skill":"dataviz"}}]}}"#,
            r#"{"type":"user","timestamp":"2026-11-24T10:01:00Z","message":{"content":"<command-message>pr-before-after</command-message>\n<command-name>/pr-before-after</command-name>"}}"#,
            // Quoted markup inside a normal message is not a command.
            r#"{"type":"user","timestamp":"2026-11-24T10:02:00Z","message":{"content":"look: <command-name>/x</command-name>"}}"#,
        ];
        fs::write(&f, lines.join("\n")).unwrap();
        let uses = claude_uses(&f);
        assert_eq!(uses.len(), 2);
        assert_eq!((uses[0].skill.as_str(), uses[0].method), ("dataviz", Method::Auto));
        assert_eq!((uses[1].skill.as_str(), uses[1].method), ("pr-before-after", Method::Command));
        assert_eq!(uses[1].session, "abc");
    }

    #[test]
    fn codex_mentions_and_skill_reads() {
        let d = tmp("codex");
        let f = d.join("rollout-x.jsonl");
        let lines = [
            r#"{"timestamp":"2026-11-24T06:55:46Z","type":"session_meta","payload":{"id":"s1"}}"#,
            r#"{"timestamp":"2026-11-24T06:56:00Z","type":"event_msg","payload":{"type":"item_completed","turn_id":"t1","item":{"type":"UserMessage","content":[{"type":"text","text":"$anti-slop-audit"},{"type":"skill","name":"anti-slop-audit","path":"/u/.agents/skills/anti-slop-audit/SKILL.md"}]}}}"#,
            r#"{"timestamp":"2026-11-24T06:57:00Z","type":"event_msg","payload":{"type":"item_completed","turn_id":"t2","item":{"type":"CommandExecution","command":["/bin/zsh","-lc","sed -n '1,200p' /u/.claude/skills/pr-before-after/SKILL.md"]}}}"#,
            // Same skill read again in the same turn: one use.
            r#"{"timestamp":"2026-11-24T06:57:05Z","type":"event_msg","payload":{"type":"item_completed","turn_id":"t2","item":{"type":"CommandExecution","command":["/bin/zsh","-lc","sed -n '200,400p' /u/.claude/skills/pr-before-after/SKILL.md"]}}}"#,
            // Memory notes are not skills.
            r#"{"timestamp":"2026-11-24T06:58:00Z","type":"event_msg","payload":{"type":"item_completed","turn_id":"t3","item":{"type":"CommandExecution","command":["/bin/zsh","-lc","cat /u/.codex/memories/skills/foo/SKILL.md"]}}}"#,
        ];
        fs::write(&f, lines.join("\n")).unwrap();
        let uses = codex_uses(&f);
        let got: Vec<(&str, Method)> = uses.iter().map(|u| (u.skill.as_str(), u.method)).collect();
        assert_eq!(got, [("anti-slop-audit", Method::Command), ("pr-before-after", Method::Auto)]);
        assert!(uses.iter().all(|u| u.session == "s1" && u.agent == Provider::Codex));
    }

    #[test]
    fn skill_reads_from_shell_lines() {
        assert_eq!(skill_reads("cat ~/.agents/skills/unslop/SKILL.md && head x"), ["unslop"]);
        assert_eq!(skill_reads("rg foo /a/skills/a/SKILL.md '/b/skills/b/SKILL.md'"), ["a", "b"]);
        assert!(skill_reads("cat /u/.codex/memories/skills/x/SKILL.md").is_empty());
        assert!(skill_reads("cat README.md").is_empty());
    }

    #[test]
    fn frontmatter_fields() {
        let f = frontmatter("---\nname: x\ndescription: \"Does x\"\ndisable-model-invocation: true\n---\nbody");
        assert_eq!(f.description.as_deref(), Some("Does x"));
        assert!(f.manual_only);
    }

    #[test]
    fn problems_found() {
        let d = tmp("problems");
        let a = d.join("a/dup");
        let b = d.join("b/dup");
        for (dir, text) in [(&a, "---\ndescription: one\n---"), (&b, "---\ndescription: two\n---")] {
            fs::create_dir_all(dir).unwrap();
            fs::write(dir.join("SKILL.md"), text).unwrap();
        }
        let empty = d.join("a/empty");
        fs::create_dir_all(&empty).unwrap();
        let loc = |path: &Path, has: bool, empty: bool| Location {
            root: Root::Agents,
            path: path.to_owned(),
            real: Some(path.to_owned()),
            link: false,
            has_skill_md: has,
            empty,
        };
        let skills = vec![
            Skill { name: "dup".into(), description: "one".into(), codex: true, locations: vec![loc(&a, true, false), loc(&b, true, false)], ..Default::default() },
            Skill { name: "empty".into(), locations: vec![loc(&empty, false, true)], ..Default::default() },
            Skill { name: "long".into(), description: "x".repeat(950), codex: true, locations: vec![loc(&a, true, false)], ..Default::default() },
        ];
        let titles: Vec<String> = problems(&skills).into_iter().map(|p| format!("{}: {}", p.skill, p.title)).collect();
        assert!(titles.contains(&"dup: Версии расходятся".to_owned()), "{titles:?}");
        assert!(titles.contains(&"empty: Пустая папка".to_owned()), "{titles:?}");
        assert!(titles.iter().any(|t| t.starts_with("long: Описание близко к лимиту")), "{titles:?}");
    }
}
