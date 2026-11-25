//! Agent memory: what agents must keep in mind about the user and each project —
//! the user's corrections, don'ts, preferences, and how a project works.
//!
//! Not documents. Documents (`docs`) are artifacts for the user: decisions,
//! research, handoffs. Memory is a log for agents: short imperative lines that
//! go into every session's start context.
//!
//! ```text
//! ~/.stepan_verify/memory/
//!   log.jsonl            append-only: add / edit / remove, one JSON object per line
//!   USER.md              snapshot: entries for every project (rewritten after each change)
//!   projects/<name>.md   snapshot: one project's entries
//! ```
//!
//! The log is the source of truth; the current entries are its fold. The
//! snapshots exist for reading outside Stepan (a terminal Claude session can
//! `@`-include them) and are never read back.

use std::{
    collections::HashSet,
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::Home;

/// How much of the shared and of a project's memory goes into a session's start
/// context. Past it, the newest entries win and the rest stay searchable.
const USER_LIMIT: usize = 4000;
const PROJECT_LIMIT: usize = 6000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// Something the user asked agents not to do.
    Dont,
    /// How the user wants things done: a preference, a correction, a convention.
    Rule,
    /// How the project works: commands, paths, invariants, pitfalls.
    Fact,
}

impl Kind {
    pub const ALL: [Kind; 3] = [Kind::Dont, Kind::Rule, Kind::Fact];

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "dont" | "don't" | "no" | "never" | "нельзя" | "не" | "запрет" => Some(Self::Dont),
            "rule" | "do" | "pref" | "preference" | "correction" | "правило" | "делай" => Some(Self::Rule),
            "fact" | "facts" | "project" | "факт" | "фактура" => Some(Self::Fact),
            _ => None,
        }
    }

    /// Section heading in briefs, snapshots and the app.
    pub fn title(self) -> &'static str {
        match self {
            Self::Dont => "Не делай",
            Self::Rule => "Делай так",
            Self::Fact => "Фактура",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::Dont => "dont",
            Self::Rule => "rule",
            Self::Fact => "fact",
        }
    }
}

/// One thing to remember.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub id: String,
    pub kind: Kind,
    /// One imperative line: «Не запускать pkill iv3: в окне идут сессии пользователя».
    pub text: String,
    /// None: applies everywhere; Some: one project (the docs folder name).
    pub project: Option<String>,
    /// The user's own words this came from, verbatim.
    pub quote: String,
    pub why: String,
    /// Who wrote it first: "claude", "codex", "авто" (the distiller), "ты".
    pub by: String,
    pub session: Option<String>,
    pub created: DateTime<Utc>,
    pub updated: DateTime<Utc>,
}

/// A line of the log.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Record {
    pub at: DateTime<Utc>,
    pub by: String,
    #[serde(flatten)]
    pub op: Op,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum Op {
    Add {
        id: String,
        kind: Kind,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<String>,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        quote: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        why: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
    },
    Edit {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<Kind>,
        /// Some(None) moves the entry to every project.
        #[serde(default, skip_serializing_if = "Option::is_none", with = "scope")]
        project: Option<Option<String>>,
    },
    Remove {
        id: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        reason: String,
    },
}

impl Op {
    pub fn id(&self) -> &str {
        match self {
            Op::Add { id, .. } | Op::Edit { id, .. } | Op::Remove { id, .. } => id,
        }
    }
}

/// `project` in an edit: absent = unchanged, "" = every project, "name" = that project.
mod scope {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &Option<Option<String>>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(Some(p)) => s.serialize_str(p),
            _ => s.serialize_str(""),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Option<String>>, D::Error> {
        let s = String::deserialize(d)?;
        Ok(Some(Some(s).filter(|s| !s.is_empty())))
    }
}

/// What a caller adds; id and times are filled in.
#[derive(Clone, Debug, Default)]
pub struct NewEntry {
    pub kind: Option<Kind>,
    pub text: String,
    pub project: Option<String>,
    pub quote: String,
    pub why: String,
    pub session: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Change {
    pub text: Option<String>,
    pub kind: Option<Kind>,
    pub project: Option<Option<String>>,
}

/// The fold of the log: live entries in the order they were added, plus the log itself.
#[derive(Clone, Debug, Default)]
pub struct Memory {
    pub entries: Vec<Entry>,
    pub log: Vec<Record>,
}

pub fn dir(home: &Home) -> PathBuf {
    home.memory()
}

fn log_path(home: &Home) -> PathBuf {
    dir(home).join("log.jsonl")
}

impl Memory {
    pub fn load(home: &Home) -> Self {
        let records = fs::File::open(log_path(home))
            .map(|f| BufReader::new(f).lines().map_while(Result::ok).filter_map(|l| serde_json::from_str::<Record>(&l).ok()).collect())
            .unwrap_or_default();
        Self::fold(records)
    }

    pub fn fold(log: Vec<Record>) -> Self {
        let mut entries: Vec<Entry> = Vec::new();
        for r in &log {
            match &r.op {
                Op::Add { id, kind, text, project, quote, why, session } => {
                    if entries.iter().any(|e| &e.id == id) {
                        continue;
                    }
                    entries.push(Entry {
                        id: id.clone(),
                        kind: *kind,
                        text: text.clone(),
                        project: project.clone(),
                        quote: quote.clone(),
                        why: why.clone(),
                        by: r.by.clone(),
                        session: session.clone(),
                        created: r.at,
                        updated: r.at,
                    });
                }
                Op::Edit { id, text, kind, project } => {
                    if let Some(e) = entries.iter_mut().find(|e| &e.id == id) {
                        if let Some(t) = text {
                            e.text = t.clone();
                        }
                        if let Some(k) = kind {
                            e.kind = *k;
                        }
                        if let Some(p) = project {
                            e.project = p.clone();
                        }
                        e.updated = r.at;
                    }
                }
                Op::Remove { id, .. } => entries.retain(|e| &e.id != id),
            }
        }
        Self { entries, log }
    }

    pub fn get(&self, id: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.id == id)
    }

    /// Entries that apply in `project`: the shared ones and that project's.
    pub fn applying(&self, project: Option<&str>) -> Vec<&Entry> {
        self.entries.iter().filter(|e| e.project.is_none() || e.project.as_deref() == project).collect()
    }

    /// Projects that have entries, sorted.
    pub fn projects(&self) -> Vec<String> {
        let mut out: Vec<String> = self.entries.iter().filter_map(|e| e.project.clone()).collect::<HashSet<_>>().into_iter().collect();
        out.sort();
        out
    }

    /// Keyword search over text, quote and reason; every word must match.
    /// Ranked by how many times the words occur, newest first on ties.
    pub fn search(&self, query: &str, project: Option<&str>) -> Vec<&Entry> {
        let words: Vec<String> = query.split_whitespace().map(|w| w.to_lowercase()).collect();
        let mut hits: Vec<(usize, &Entry)> = self
            .entries
            .iter()
            .filter(|e| project.is_none_or(|p| e.project.is_none() || e.project.as_deref() == Some(p)))
            .filter_map(|e| {
                let hay = format!("{} {} {} {}", e.text, e.quote, e.why, e.project.as_deref().unwrap_or("")).to_lowercase();
                let counts: Vec<usize> = words.iter().map(|w| hay.matches(w.as_str()).count()).collect();
                counts.iter().all(|&c| c > 0).then(|| (counts.iter().sum(), e))
            })
            .collect();
        hits.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.updated.cmp(&a.1.updated)));
        hits.into_iter().map(|(_, e)| e).collect()
    }

    /// The memory section of a session's start context: shared entries, then
    /// the project's, by kind; each part capped, newest first past the cap.
    pub fn brief(&self, project: Option<&str>) -> String {
        let mut out = String::from(BRIEF_HEAD);
        let shared: Vec<&Entry> = self.entries.iter().filter(|e| e.project.is_none()).collect();
        out.push_str("\n## Для всех проектов\n\n");
        push_capped(&mut out, &shared, USER_LIMIT);
        if let Some(p) = project {
            let mine: Vec<&Entry> = self.entries.iter().filter(|e| e.project.as_deref() == Some(p)).collect();
            out.push_str(&format!("## Проект {p}\n\n"));
            push_capped(&mut out, &mine, PROJECT_LIMIT);
        }
        out.push_str(&RULES.replace("{project}", project.unwrap_or("<проект>")));
        out
    }
}

const BRIEF_HEAD: &str = "# Память Stepan\n\n\
Это то, что пользователь уже говорил агентам, и то, что они выяснили о проекте. Соблюдай без напоминаний: \
«не делай» — его запреты, «делай так» — как ему нужно, «фактура» — как устроен проект. \
Если запись мешает задаче или устарела — скажи об этом, а не нарушай молча.\n";

const RULES: &str = "## Как пополнять память\n\n\
Когда пользователь поправляет тебя, что-то запрещает, говорит, как ему нравится, или ты выяснил, как устроен проект \
(запуск, тесты, где что лежит, грабли), — сразу запиши одну строку, без отдельной просьбы:\n\n\
```sh\n\
stepan_verify memory add \"Не запускать pkill: в окне идут сессии пользователя\" --kind dont --quote \"никогда не убивай iv3\"\n\
stepan_verify memory add \"Тексты интерфейса — по-русски\" --kind rule --global\n\
stepan_verify memory add \"Тесты: cargo test -p app; IV3_HOME=/tmp/x для изоляции\" --kind fact\n\
```\n\n\
- `--kind dont` — запрет, `rule` — предпочтение или поправка, `fact` — как устроен проект.\n\
- Одна запись — одна мысль, повелительно, без дат. `--quote` — слова пользователя дословно, если они были.\n\
- По умолчанию запись про проект {project}; `--global` — если это про пользователя вообще (стиль, язык, привычки).\n\
- Сначала проверь, нет ли такой: `stepan_verify memory search <слово>`. Устарело или пользователь передумал — \
`stepan_verify memory edit <id> \"новый текст\"` или `stepan_verify memory rm <id>`.\n\
- Не пиши в память план работы, пересказ правок и то, что уже есть в коде, README или CLAUDE.md. \
Большие осмысленные вещи — решения, разборы — это документы, не память.\n";

fn push_capped(out: &mut String, entries: &[&Entry], limit: usize) {
    if entries.is_empty() {
        out.push_str("Пока пусто.\n");
        return;
    }
    let mut newest: Vec<&Entry> = entries.to_vec();
    newest.sort_by_key(|e| std::cmp::Reverse(e.updated));
    let mut used = 0;
    let mut kept: Vec<&Entry> = Vec::new();
    for e in newest {
        let len = e.text.chars().count() + 16;
        if used + len > limit && !kept.is_empty() {
            continue;
        }
        used += len;
        kept.push(e);
    }
    for kind in Kind::ALL {
        let mut part: Vec<&&Entry> = kept.iter().filter(|e| e.kind == kind).collect();
        if part.is_empty() {
            continue;
        }
        part.sort_by_key(|e| e.created);
        out.push_str(&format!("### {}\n\n", kind.title()));
        for e in part {
            out.push_str(&format!("- {} `{}`\n", e.text, e.id));
        }
        out.push('\n');
    }
    let rest = entries.len() - kept.len();
    if rest > 0 {
        out.push_str(&format!("…и ещё {rest} — `stepan_verify memory search <слово>`.\n"));
    }
}

// ------------------------------------------------------------------ writes

fn normalize(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").trim_end_matches(['.', '!', ';']).to_lowercase()
}

fn new_id(at: DateTime<Utc>, text: &str) -> String {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut h = DefaultHasher::new();
    text.hash(&mut h);
    at.timestamp_nanos_opt().unwrap_or_default().hash(&mut h);
    format!("m{:06x}", h.finish() & 0xff_ffff)
}

fn append(home: &Home, record: &Record) -> Result<()> {
    fs::create_dir_all(dir(home))?;
    let mut file = OpenOptions::new().create(true).append(true).open(log_path(home))?;
    let mut line = serde_json::to_string(record)?;
    line.push('\n');
    file.write_all(line.as_bytes())?;
    Ok(())
}

pub fn add(home: &Home, new: NewEntry, by: &str) -> Result<Entry> {
    let text = new.text.trim().to_owned();
    if text.is_empty() {
        bail!("пустая запись");
    }
    let memory = Memory::load(home);
    if let Some(same) = memory.entries.iter().find(|e| e.project == new.project && normalize(&e.text) == normalize(&text)) {
        bail!("такая запись уже есть: {} {}", same.id, same.text);
    }
    let at = Utc::now();
    let id = new_id(at, &text);
    let record = Record {
        at,
        by: by.to_owned(),
        op: Op::Add {
            id: id.clone(),
            kind: new.kind.unwrap_or(Kind::Rule),
            text,
            project: new.project,
            quote: new.quote.trim().to_owned(),
            why: new.why.trim().to_owned(),
            session: new.session,
        },
    };
    append(home, &record)?;
    let memory = Memory::load(home);
    write_snapshots(home, &memory);
    memory.get(&id).cloned().context("запись не сохранилась")
}

pub fn edit(home: &Home, id: &str, change: Change, by: &str) -> Result<Entry> {
    let memory = Memory::load(home);
    let Some(entry) = memory.get(id) else { bail!("нет записи {id}") };
    let text = change.text.map(|t| t.trim().to_owned()).filter(|t| !t.is_empty() && *t != entry.text);
    let kind = change.kind.filter(|k| *k != entry.kind);
    let project = change.project.filter(|p| *p != entry.project);
    if text.is_none() && kind.is_none() && project.is_none() {
        return Ok(entry.clone());
    }
    append(home, &Record { at: Utc::now(), by: by.to_owned(), op: Op::Edit { id: id.to_owned(), text, kind, project } })?;
    let memory = Memory::load(home);
    write_snapshots(home, &memory);
    memory.get(id).cloned().context("запись пропала")
}

pub fn remove(home: &Home, id: &str, reason: &str, by: &str) -> Result<Entry> {
    let memory = Memory::load(home);
    let Some(entry) = memory.get(id).cloned() else { bail!("нет записи {id}") };
    append(home, &Record { at: Utc::now(), by: by.to_owned(), op: Op::Remove { id: id.to_owned(), reason: reason.trim().to_owned() } })?;
    write_snapshots(home, &Memory::load(home));
    Ok(entry)
}

/// USER.md and projects/<name>.md, from the current entries. Snapshots of
/// projects that lost every entry are removed.
pub fn write_snapshots(home: &Home, memory: &Memory) {
    let root = dir(home);
    let _ = fs::create_dir_all(root.join("projects"));
    let shared: Vec<&Entry> = memory.entries.iter().filter(|e| e.project.is_none()).collect();
    let _ = fs::write(root.join("USER.md"), snapshot("Память Stepan: для всех проектов", &shared));
    let projects = memory.projects();
    for p in &projects {
        let mine: Vec<&Entry> = memory.entries.iter().filter(|e| e.project.as_deref() == Some(p)).collect();
        let _ = fs::write(root.join("projects").join(format!("{p}.md")), snapshot(&format!("Память Stepan: {p}"), &mine));
    }
    for entry in fs::read_dir(root.join("projects")).into_iter().flatten().flatten() {
        let path = entry.path();
        let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        if path.extension().is_some_and(|e| e == "md") && !projects.contains(&stem) {
            let _ = fs::remove_file(path);
        }
    }
}

fn snapshot(title: &str, entries: &[&Entry]) -> String {
    let mut out = format!("<!-- Снимок {}: не правь руками, источник — log.jsonl (stepan_verify memory …) -->\n# {title}\n\n", "памяти");
    for kind in Kind::ALL {
        let part: Vec<&&Entry> = entries.iter().filter(|e| e.kind == kind).collect();
        if part.is_empty() {
            continue;
        }
        out.push_str(&format!("## {}\n\n", kind.title()));
        for e in part {
            out.push_str(&format!("- {}\n", e.text));
        }
        out.push('\n');
    }
    out
}

// ------------------------------------------------------------------ project

/// The project an agent works in, as memory and docs name it: the folder name
/// of the git root above `cwd` (or `cwd` itself); None for the home folder.
pub fn project_of(cwd: &Path) -> Option<String> {
    let root = cwd.ancestors().find(|d| d.join(".git").exists()).unwrap_or(cwd);
    if dirs::home_dir().is_some_and(|h| h == root) {
        return None;
    }
    root.file_name().map(|n| n.to_string_lossy().into_owned())
}

// ------------------------------------------------------------------ cli

pub const USAGE: &str = "\
stepan_verify memory — память агентов (~/.stepan_verify/memory)

  stepan_verify memory add TEXT [--kind dont|rule|fact] [--quote СЛОВА] [--why ПОЧЕМУ] [--global | --project P]
  stepan_verify memory edit ID [TEXT] [--kind K] [--global | --project P]
  stepan_verify memory rm ID [--reason ПОЧЕМУ]
  stepan_verify memory search СЛОВА [--project P | --all]
  stepan_verify memory list [--project P | --global | --all] [--json]
  stepan_verify memory brief [--project P]     что агент получает на старте
  stepan_verify memory log [N]                 последние изменения

Проект по умолчанию — $STEPAN_VERIFY_PROJECT, иначе папка git-корня текущей директории.
Автор — $STEPAN_VERIFY_AGENT (иначе «ты»), сессия — $CLAUDE_CODE_SESSION_ID / $CODEX_THREAD_ID.
";

/// Runs `stepan_verify memory <args>`; returns the text to print.
pub fn run(home: &Home, raw: &[String]) -> Result<String> {
    let Some((cmd, rest)) = raw.split_first() else { return Ok(USAGE.to_owned()) };
    let (positional, flags) = split_args(rest);
    let flag = |name: &str| flags.iter().find(|(f, _)| f == name).and_then(|(_, v)| v.clone());
    let has = |name: &str| flags.iter().any(|(f, _)| f == name);
    let by = flag("--by").or_else(|| std::env::var("STEPAN_VERIFY_AGENT").ok()).unwrap_or_else(|| "ты".into());
    let current = || -> Option<String> {
        flag("--project")
            .or_else(|| match std::env::var("STEPAN_VERIFY_PROJECT") {
                // Set but empty: a session outside any project, its entries are shared.
                Ok(p) => Some(p).filter(|p| !p.is_empty()),
                Err(_) => std::env::current_dir().ok().and_then(|d| project_of(&d)),
            })
    };
    let kind = flag("--kind").map(|k| Kind::parse(&k).with_context(|| format!("--kind: dont, rule или fact, а не «{k}»"))).transpose()?;
    let line = |e: &Entry| format!("{} [{}] {}{}", e.id, e.kind.key(), e.text, e.project.as_deref().map(|p| format!("  ({p})")).unwrap_or_default());

    match cmd.as_str() {
        "add" => {
            let text = positional.join(" ");
            let project = if has("--global") { None } else { current() };
            let session = flag("--session")
                .or_else(|| std::env::var("CLAUDE_CODE_SESSION_ID").ok())
                .or_else(|| std::env::var("CODEX_THREAD_ID").ok());
            let e = add(home, NewEntry { kind, text, project, quote: flag("--quote").unwrap_or_default(), why: flag("--why").unwrap_or_default(), session }, &by)?;
            Ok(format!("запомнил: {}", line(&e)))
        }
        "edit" => {
            let Some((id, text)) = positional.split_first() else { bail!("нужен id") };
            let project = if has("--global") { Some(None) } else { flag("--project").map(Some) };
            let text = Some(text.join(" ")).filter(|t| !t.is_empty());
            let e = edit(home, id, Change { text, kind, project }, &by)?;
            Ok(format!("поправил: {}", line(&e)))
        }
        "rm" | "remove" | "forget" => {
            let Some(id) = positional.first() else { bail!("нужен id") };
            let e = remove(home, id, &flag("--reason").unwrap_or_default(), &by)?;
            Ok(format!("забыл: {}", line(&e)))
        }
        "search" | "find" => {
            let memory = Memory::load(home);
            let project = if has("--all") { None } else { current() };
            let found = memory.search(&positional.join(" "), project.as_deref());
            if found.is_empty() {
                return Ok("ничего не нашёл".into());
            }
            Ok(found.iter().map(|e| line(e)).collect::<Vec<_>>().join("\n"))
        }
        "list" | "ls" => {
            let memory = Memory::load(home);
            let list: Vec<&Entry> = if has("--all") {
                memory.entries.iter().collect()
            } else if has("--global") {
                memory.entries.iter().filter(|e| e.project.is_none()).collect()
            } else {
                memory.applying(current().as_deref())
            };
            if has("--json") {
                let json: Vec<serde_json::Value> = list
                    .iter()
                    .map(|e| {
                        serde_json::json!({"id": e.id, "kind": e.kind, "text": e.text, "project": e.project, "quote": e.quote, "why": e.why,
                            "by": e.by, "session": e.session, "created": e.created, "updated": e.updated})
                    })
                    .collect();
                return Ok(serde_json::to_string_pretty(&json)?);
            }
            if list.is_empty() {
                return Ok("память пуста".into());
            }
            Ok(list.iter().map(|e| line(e)).collect::<Vec<_>>().join("\n"))
        }
        "brief" => Ok(Memory::load(home).brief(current().as_deref())),
        "log" => {
            let n: usize = positional.first().and_then(|n| n.parse().ok()).unwrap_or(20);
            let memory = Memory::load(home);
            let start = memory.log.len().saturating_sub(n);
            Ok(memory.log[start..].iter().map(|r| format!("{} {} {}", r.at.format("%Y-%m-%d %H:%M"), r.by, serde_json::to_string(&r.op).unwrap_or_default())).collect::<Vec<_>>().join("\n"))
        }
        "help" | "--help" | "-h" => Ok(USAGE.to_owned()),
        other => bail!("не знаю команду «{other}»\n\n{USAGE}"),
    }
}

fn split_args(raw: &[String]) -> (Vec<String>, Vec<(String, Option<String>)>) {
    const VALUED: [&str; 7] = ["--kind", "--quote", "--why", "--project", "--by", "--session", "--reason"];
    let mut positional = Vec::new();
    let mut flags = Vec::new();
    let mut it = raw.iter();
    while let Some(a) = it.next() {
        if a.starts_with("--") {
            let value = VALUED.contains(&a.as_str()).then(|| it.next().cloned()).flatten();
            flags.push((a.clone(), value));
        } else {
            positional.push(a.clone());
        }
    }
    (positional, flags)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> Home {
        let root = std::env::temp_dir().join(format!("stepan-memory-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = fs::remove_dir_all(&root);
        Home { root }
    }

    fn new(text: &str, kind: Kind, project: Option<&str>) -> NewEntry {
        NewEntry { kind: Some(kind), text: text.into(), project: project.map(Into::into), ..Default::default() }
    }

    #[test]
    fn log_folds_into_entries_and_snapshots() {
        let home = home();
        let a = add(&home, new("Не запускать pkill iv3", Kind::Dont, Some("iv3")), "claude").unwrap();
        let b = add(&home, new("Тексты интерфейса — по-русски", Kind::Rule, None), "ты").unwrap();
        assert!(add(&home, new("не запускать  pkill iv3.", Kind::Dont, Some("iv3")), "codex").is_err(), "duplicates are rejected");
        edit(&home, &b.id, Change { text: Some("Тексты интерфейса и PR — по-русски".into()), ..Default::default() }, "ты").unwrap();
        let c = add(&home, new("Тесты: cargo test", Kind::Fact, Some("iv3")), "авто").unwrap();
        remove(&home, &c.id, "устарело", "ты").unwrap();

        let m = Memory::load(&home);
        assert_eq!(m.entries.len(), 2);
        assert_eq!(m.log.len(), 5);
        assert_eq!(m.get(&b.id).unwrap().text, "Тексты интерфейса и PR — по-русски");
        assert_eq!(m.get(&a.id).unwrap().by, "claude");
        assert_eq!(m.applying(Some("other")).len(), 1);

        let user = fs::read_to_string(dir(&home).join("USER.md")).unwrap();
        assert!(user.contains("## Делай так") && user.contains("и PR"));
        let proj = fs::read_to_string(dir(&home).join("projects/iv3.md")).unwrap();
        assert!(proj.contains("## Не делай") && !proj.contains("cargo test"));

        // Moving the only project entry to "everywhere" drops the project snapshot.
        edit(&home, &a.id, Change { project: Some(None), ..Default::default() }, "ты").unwrap();
        assert!(!dir(&home).join("projects/iv3.md").exists());
        assert!(Memory::load(&home).get(&a.id).unwrap().project.is_none());
        fs::remove_dir_all(&home.root).unwrap();
    }

    #[test]
    fn brief_groups_by_kind_and_caps() {
        let home = home();
        add(&home, new("Не делать force push", Kind::Dont, None), "ты").unwrap();
        add(&home, new("Запуск: cargo run", Kind::Fact, Some("iv3")), "авто").unwrap();
        let brief = Memory::load(&home).brief(Some("iv3"));
        assert!(brief.contains("## Для всех проектов") && brief.contains("### Не делай") && brief.contains("## Проект iv3") && brief.contains("### Фактура"));
        assert!(brief.contains("stepan_verify memory add"));
        let elsewhere = Memory::load(&home).brief(Some("other"));
        assert!(!elsewhere.contains("cargo run") && elsewhere.contains("Пока пусто"));

        let many: Vec<Entry> = (0..400)
            .map(|i| Entry {
                id: format!("m{i}"),
                kind: Kind::Rule,
                text: format!("правило номер {i} достаточно длинное"),
                project: None,
                quote: String::new(),
                why: String::new(),
                by: "ты".into(),
                session: None,
                created: Utc::now(),
                updated: Utc::now(),
            })
            .collect();
        let refs: Vec<&Entry> = many.iter().collect();
        let mut out = String::new();
        push_capped(&mut out, &refs, USER_LIMIT);
        assert!(out.chars().count() < USER_LIMIT + 400 && out.contains("…и ещё"));
        fs::remove_dir_all(&home.root).unwrap();
    }

    #[test]
    fn search_needs_every_word() {
        let home = home();
        add(&home, NewEntry { quote: "никогда не убивай iv3".into(), ..new("Не запускать pkill", Kind::Dont, Some("iv3")) }, "ты").unwrap();
        add(&home, new("Шрифты: SF Pro Rounded", Kind::Rule, Some("iv3")), "ты").unwrap();
        let m = Memory::load(&home);
        assert_eq!(m.search("убивай pkill", None).len(), 1);
        assert_eq!(m.search("шрифты", Some("other")).len(), 0);
        assert_eq!(m.search("iv3", Some("iv3")).len(), 2);
        fs::remove_dir_all(&home.root).unwrap();
    }

    #[test]
    fn edit_scope_roundtrips_through_json() {
        let r = Record { at: Utc::now(), by: "ты".into(), op: Op::Edit { id: "m1".into(), text: None, kind: None, project: Some(None) } };
        let back: Record = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(back, r);
        let r2: Record = serde_json::from_str(r#"{"at":"2026-11-25T10:00:00Z","by":"ты","op":"edit","id":"m1","text":"x"}"#).unwrap();
        assert!(matches!(r2.op, Op::Edit { project: None, .. }));
    }
}
