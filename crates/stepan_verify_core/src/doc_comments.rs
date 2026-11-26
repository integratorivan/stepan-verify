//! Comments on shelf documents, anchored to a quote rather than a line number
//! so they survive rewrites. The markdown itself is never touched: each
//! document's comments live beside it in app state,
//! `state/doc-comments/<rel>.jsonl` (`rel` is the path inside the docs folder).
//!
//! The file is an append-only log: an `add` line carries a whole comment,
//! later lines (`resolve`, `reopen`, `delete`, `sent`) change its state. The
//! app and agents (`stepan_verify doc …`) may write at the same time, and an
//! appended line never clobbers someone else's.
//!
//! A comment remembers its quote and the text of the blocks around it
//! (`prev` / `next`). To find it again in the current text: the quote
//! exactly, then with whitespace collapsed, then with markdown marks
//! dropped (a selection is copied from rendered text), then the place
//! between its old neighbours. Not found — the comment is stale, but kept.

use std::{
    fs,
    io::Write as _,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    Home,
    docs::{self, Author},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// «замечание»
    #[default]
    Comment,
    /// «одобряю»
    Approval,
    /// «убрать это»
    Deletion,
}

impl Kind {
    pub fn key(self) -> &'static str {
        match self {
            Self::Comment => "comment",
            Self::Approval => "approval",
            Self::Deletion => "deletion",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Comment => "замечание",
            Self::Approval => "одобряю",
            Self::Deletion => "убрать это",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Comment {
    pub id: String,
    /// The commented text: a selection or a whole block. Empty — the whole document.
    #[serde(default)]
    pub quote: String,
    /// Text of the blocks before and after the quote's block, to find it again.
    #[serde(default)]
    pub prev: String,
    #[serde(default)]
    pub next: String,
    pub text: String,
    #[serde(default)]
    pub kind: Kind,
    /// ты / claude / codex.
    pub author: String,
    /// The thread's root comment; None for a root.
    #[serde(default)]
    pub reply_to: Option<String>,
    #[serde(default)]
    pub resolved: bool,
    pub created_at: DateTime<Utc>,
    /// Session the comment was delivered to (`new:claude` for one started for it).
    #[serde(default)]
    pub sent_to: Option<String>,
}

impl Comment {
    pub fn by_you(&self) -> bool {
        self.author == Author::You.label()
    }
}

/// One line of the log.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Op {
    Add(Comment),
    Resolve { id: String, by: String, at: DateTime<Utc> },
    Reopen { id: String, by: String, at: DateTime<Utc> },
    Delete { id: String, by: String, at: DateTime<Utc> },
    Sent { id: String, session: String, at: DateTime<Utc> },
}

/// A root comment and its replies, oldest first.
#[derive(Clone, Debug, PartialEq)]
pub struct Thread {
    pub root: Comment,
    pub replies: Vec<Comment>,
}

impl Thread {
    pub fn messages(&self) -> impl Iterator<Item = &Comment> {
        std::iter::once(&self.root).chain(&self.replies)
    }

    /// Your messages not yet delivered to an agent.
    pub fn unsent(&self) -> Vec<&Comment> {
        self.messages().filter(|c| c.by_you() && c.sent_to.is_none()).collect()
    }
}

// ------------------------------------------------------------------ store

pub fn log_path(home: &Home, rel: &str) -> PathBuf {
    home.state().join("doc-comments").join(format!("{rel}.jsonl"))
}

/// The folder all comment logs live in (watched for changes).
pub fn root(home: &Home) -> PathBuf {
    home.state().join("doc-comments")
}

/// Current comments of a document, deleted ones gone, in creation order.
pub fn load(home: &Home, rel: &str) -> Vec<Comment> {
    fold(&fs::read_to_string(log_path(home, rel)).unwrap_or_default())
}

fn fold(log: &str) -> Vec<Comment> {
    let mut out: Vec<Comment> = Vec::new();
    for op in log.lines().filter_map(|l| serde_json::from_str::<Op>(l).ok()) {
        match op {
            Op::Add(c) => {
                if !out.iter().any(|o| o.id == c.id) {
                    out.push(c);
                }
            }
            Op::Resolve { id, .. } => out.iter_mut().filter(|c| c.id == id).for_each(|c| c.resolved = true),
            Op::Reopen { id, .. } => out.iter_mut().filter(|c| c.id == id).for_each(|c| c.resolved = false),
            // Deleting a root takes its replies with it.
            Op::Delete { id, .. } => out.retain(|c| c.id != id && c.reply_to.as_deref() != Some(id.as_str())),
            Op::Sent { id, session, .. } => out.iter_mut().filter(|c| c.id == id).for_each(|c| c.sent_to = Some(session.clone())),
        }
    }
    out
}

/// Threads in creation order; a thread is resolved when its root is.
pub fn threads(comments: &[Comment]) -> Vec<Thread> {
    comments
        .iter()
        .filter(|c| c.reply_to.is_none())
        .map(|root| Thread { root: root.clone(), replies: comments.iter().filter(|c| c.reply_to.as_deref() == Some(root.id.as_str())).cloned().collect() })
        .collect()
}

/// Open threads of a document (the shelf's counter).
pub fn open_count(home: &Home, rel: &str) -> usize {
    let path = log_path(home, rel);
    if !path.exists() {
        return 0;
    }
    load(home, rel).iter().filter(|c| c.reply_to.is_none() && !c.resolved).count()
}

fn append(home: &Home, rel: &str, ops: &[Op]) -> Result<()> {
    let path = log_path(home, rel);
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut text = String::new();
    for op in ops {
        text.push_str(&serde_json::to_string(op)?);
        text.push('\n');
    }
    let mut f = fs::OpenOptions::new().create(true).append(true).open(&path).with_context(|| path.display().to_string())?;
    f.write_all(text.as_bytes())?;
    Ok(())
}

/// What a new root comment says and where it points.
#[derive(Clone, Debug, Default)]
pub struct NewComment {
    pub quote: String,
    pub prev: String,
    pub next: String,
    pub text: String,
    pub kind: Kind,
    pub author: String,
}

impl NewComment {
    /// On block `ix` of `blocks`; `quote` narrows it to a fragment (else the whole block).
    pub fn on_block(blocks: &[String], ix: usize, quote: Option<&str>) -> Self {
        let around = |i: Option<usize>| i.and_then(|i| blocks.get(i)).cloned().unwrap_or_default();
        Self {
            quote: quote.map(str::trim).filter(|q| !q.is_empty()).map_or_else(|| blocks.get(ix).cloned().unwrap_or_default(), str::to_owned),
            prev: around(ix.checked_sub(1)),
            next: around(Some(ix + 1)),
            ..Self::default()
        }
    }
}

fn new_id(existing: &[Comment], salt: &str) -> String {
    let now = Utc::now().timestamp_nanos_opt().unwrap_or_default();
    (0u64..)
        .map(|n| format!("c{:03x}", fnv(format!("{now}{salt}{n}").as_bytes()) & 0xfff))
        .find(|id| !existing.iter().any(|c| &c.id == id))
        .expect("a free id")
}

/// FNV-1a: a stable hash (std's may change between releases).
pub(crate) fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

pub fn add(home: &Home, rel: &str, new: NewComment) -> Result<Comment> {
    if new.text.trim().is_empty() && new.kind == Kind::Comment {
        bail!("пустой комментарий");
    }
    let existing = load(home, rel);
    let c = Comment {
        id: new_id(&existing, &new.text),
        quote: new.quote,
        prev: new.prev,
        next: new.next,
        text: new.text.trim().to_owned(),
        kind: new.kind,
        author: new.author,
        reply_to: None,
        resolved: false,
        created_at: Utc::now(),
        sent_to: None,
    };
    append(home, rel, &[Op::Add(c.clone())])?;
    Ok(c)
}

/// Answer in the thread of `id` (a root or any of its replies).
pub fn reply(home: &Home, rel: &str, id: &str, text: &str, author: &str) -> Result<Comment> {
    if text.trim().is_empty() {
        bail!("пустой ответ");
    }
    let existing = load(home, rel);
    let target = existing.iter().find(|c| c.id == id).with_context(|| format!("нет комментария {id}"))?;
    let root = target.reply_to.clone().unwrap_or_else(|| target.id.clone());
    let c = Comment {
        id: new_id(&existing, text),
        quote: String::new(),
        prev: String::new(),
        next: String::new(),
        text: text.trim().to_owned(),
        kind: Kind::Comment,
        author: author.to_owned(),
        reply_to: Some(root),
        resolved: false,
        created_at: Utc::now(),
        sent_to: None,
    };
    append(home, rel, &[Op::Add(c.clone())])?;
    Ok(c)
}

/// The thread roots of `ids` (each id may be a reply), checked to exist.
fn roots(existing: &[Comment], ids: &[String]) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for id in ids {
        let c = existing.iter().find(|c| &c.id == id).with_context(|| format!("нет комментария {id}"))?;
        let root = c.reply_to.clone().unwrap_or_else(|| c.id.clone());
        if !out.contains(&root) {
            out.push(root);
        }
    }
    Ok(out)
}

/// Close (or reopen) the threads of `ids`; returns the roots touched.
pub fn set_resolved(home: &Home, rel: &str, ids: &[String], resolved: bool, by: &str) -> Result<Vec<String>> {
    let roots = roots(&load(home, rel), ids)?;
    let at = Utc::now();
    let ops: Vec<Op> = roots
        .iter()
        .map(|id| if resolved { Op::Resolve { id: id.clone(), by: by.to_owned(), at } } else { Op::Reopen { id: id.clone(), by: by.to_owned(), at } })
        .collect();
    append(home, rel, &ops)?;
    Ok(roots)
}

/// Delete one comment; a root goes with its replies.
pub fn delete(home: &Home, rel: &str, id: &str, by: &str) -> Result<()> {
    if !load(home, rel).iter().any(|c| c.id == id) {
        bail!("нет комментария {id}");
    }
    append(home, rel, &[Op::Delete { id: id.to_owned(), by: by.to_owned(), at: Utc::now() }])
}

/// Remember that `ids` went to `session`.
pub fn mark_sent(home: &Home, rel: &str, ids: &[String], session: &str) -> Result<()> {
    let at = Utc::now();
    let ops: Vec<Op> = ids.iter().map(|id| Op::Sent { id: id.clone(), session: session.to_owned(), at }).collect();
    append(home, rel, &ops)
}

// ------------------------------------------------------------------ blocks and anchors

/// Split a markdown body into blocks a comment can point at: headings,
/// paragraphs, list items (with their nested lines), fenced code, tables
/// and quotes as one chunk each.
pub fn blocks(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    let mut fence: Option<&str> = None;
    let mut after_blank = false;
    let flush = |cur: &mut Vec<&str>, out: &mut Vec<String>| {
        if cur.iter().any(|l| !l.trim().is_empty()) {
            out.push(cur.join("\n"));
        }
        cur.clear();
    };
    for line in body.lines() {
        let trimmed = line.trim_start();
        if let Some(marker) = fence {
            cur.push(line);
            if trimmed.starts_with(marker) {
                fence = None;
                flush(&mut cur, &mut out);
            }
            continue;
        }
        if line.trim().is_empty() {
            flush(&mut cur, &mut out);
            after_blank = true;
            continue;
        }
        let indent = line.len() - trimmed.len();
        // A loose list item's indented paragraph stays with the item.
        if after_blank && indent >= 2 && cur.is_empty() && out.last().is_some_and(|b| list_item(b.trim_start())) {
            let last = out.pop().expect("checked");
            out.push(format!("{last}\n\n{line}"));
            after_blank = false;
            continue;
        }
        // …and so do the lines that follow it.
        if out.last().is_some_and(|b| b.contains("\n\n") && list_item(b.trim_start())) && cur.is_empty() && indent >= 2 && !after_blank {
            let last = out.last_mut().expect("checked");
            last.push('\n');
            last.push_str(line);
            continue;
        }
        after_blank = false;
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            flush(&mut cur, &mut out);
            fence = Some(&trimmed[..3]);
            cur.push(line);
            continue;
        }
        if heading(trimmed) {
            flush(&mut cur, &mut out);
            out.push(line.to_owned());
            continue;
        }
        if indent == 0 && list_item(trimmed) {
            flush(&mut cur, &mut out);
        }
        cur.push(line);
    }
    flush(&mut cur, &mut out);
    out
}

fn heading(line: &str) -> bool {
    let level = line.chars().take_while(|c| *c == '#').count();
    (1..=6).contains(&level) && line[level..].starts_with(' ')
}

/// `- x`, `* x`, `+ x`, `1. x`, `1) x`.
pub fn list_item(line: &str) -> bool {
    if line.starts_with("- ") || line.starts_with("* ") || line.starts_with("+ ") {
        return true;
    }
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    digits > 0 && (line[digits..].starts_with(". ") || line[digits..].starts_with(") "))
}

/// Whitespace collapsed to single spaces.
fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Text as rendered, roughly: markdown marks, list markers and heading
/// hashes dropped, whitespace collapsed. A selection copied from the reader
/// matches the source this way.
fn plain(s: &str) -> String {
    let lines: Vec<String> = s
        .lines()
        .map(|l| {
            let l = l.trim_start().trim_start_matches(['#', '>']).trim_start();
            let l = if list_item(l) { l.split_once(' ').map_or("", |(_, rest)| rest) } else { l };
            l.chars().filter(|c| !matches!(c, '*' | '_' | '`' | '~' | '|')).collect()
        })
        .collect();
    squash(&lines.join(" "))
}

/// Where a comment sits in the current text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Anchor {
    /// About the whole document.
    Doc,
    /// On block `ix`; `moved`: found only by its neighbours — the quoted text changed.
    Block { ix: usize, moved: bool },
    /// Not found any more («устарел»).
    Stale,
}

/// Find `c`'s quote among `blocks`.
pub fn anchor(blocks: &[String], c: &Comment) -> Anchor {
    if c.quote.trim().is_empty() {
        return Anchor::Doc;
    }
    let ways: [&dyn Fn(&str) -> String; 3] = [&|s: &str| s.to_owned(), &squash, &plain];
    for norm in ways {
        let quote = norm(&c.quote);
        if quote.is_empty() {
            continue;
        }
        let hits: Vec<usize> = (0..blocks.len()).filter(|&i| norm(&blocks[i]).contains(&quote)).collect();
        if let Some(&first) = hits.first() {
            // Several places say it: the one between the old neighbours wins.
            let ix = hits.iter().copied().max_by_key(|&i| (neighbour(blocks, i, -1, &c.prev) as u8 + neighbour(blocks, i, 1, &c.next) as u8, std::cmp::Reverse(i))).unwrap_or(first);
            return Anchor::Block { ix, moved: false };
        }
    }
    let (prev, next) = (!c.prev.trim().is_empty(), !c.next.trim().is_empty());
    let between = (0..blocks.len()).find(|&i| prev && next && neighbour(blocks, i, -1, &c.prev) && neighbour(blocks, i, 1, &c.next));
    let after = || (1..blocks.len()).find(|&i| prev && neighbour(blocks, i, -1, &c.prev));
    let before = || (0..blocks.len().saturating_sub(1)).find(|&i| next && neighbour(blocks, i, 1, &c.next));
    match between.or_else(after).or_else(before) {
        Some(ix) => Anchor::Block { ix, moved: true },
        None => Anchor::Stale,
    }
}

/// Block `i + step` reads `text` (whitespace aside).
fn neighbour(blocks: &[String], i: usize, step: isize, text: &str) -> bool {
    if text.trim().is_empty() {
        return false;
    }
    i.checked_add_signed(step).and_then(|j| blocks.get(j)).is_some_and(|b| squash(b) == squash(text))
}

/// The block holding a selection copied from the reader (its first line
/// when it spans blocks).
pub fn locate(blocks: &[String], selection: &str) -> Option<usize> {
    let probe = Comment { id: String::new(), quote: selection.to_owned(), prev: String::new(), next: String::new(), text: String::new(), kind: Kind::Comment, author: String::new(), reply_to: None, resolved: false, created_at: Utc::now(), sent_to: None };
    if let Anchor::Block { ix, .. } = anchor(blocks, &probe) {
        return Some(ix);
    }
    let first = selection.lines().find(|l| !l.trim().is_empty())?;
    match anchor(blocks, &Comment { quote: first.to_owned(), ..probe }) {
        Anchor::Block { ix, .. } => Some(ix),
        _ => None,
    }
}

/// Blocks of a document as the CLI and agents see them: the markdown body
/// after frontmatter; other formats have none (comments are document-wide).
pub fn doc_blocks(path: &Path) -> Vec<String> {
    if docs::DocKind::of(path) != docs::DocKind::Markdown {
        return Vec::new();
    }
    let text = fs::read_to_string(path).unwrap_or_default();
    blocks(docs::split_front(&text).1)
}

// ------------------------------------------------------------------ for agents

/// Threads as agents read them:
///
/// ```text
/// <doc_comments for="/abs/path.md">
/// <comment id="c3f1" kind="comment">
///   | block before
/// > | quoted text
///   | block after
/// ты: what to change
/// claude: the answer
/// </comment>
/// </doc_comments>
/// ```
pub fn to_xml(path: &Path, threads: &[&Thread], blocks: &[String], note: Option<&str>) -> String {
    let mut out = format!("<doc_comments for=\"{}\">\n", path.display());
    for t in threads {
        let c = &t.root;
        let mut attrs = format!("id=\"{}\" kind=\"{}\"", c.id, c.kind.key());
        match anchor(blocks, c) {
            Anchor::Doc => attrs.push_str(" about=\"document\""),
            Anchor::Stale => attrs.push_str(" stale=\"true\""),
            Anchor::Block { moved: true, .. } => attrs.push_str(" changed=\"true\""),
            Anchor::Block { .. } => {}
        }
        if c.resolved {
            attrs.push_str(" resolved=\"true\"");
        }
        out.push_str(&format!("<comment {attrs}>\n"));
        if !c.quote.trim().is_empty() {
            context_lines(&mut out, "  | ", &c.prev, 3, true);
            context_lines(&mut out, "> | ", &c.quote, 30, false);
            context_lines(&mut out, "  | ", &c.next, 3, false);
        }
        for m in t.messages() {
            let text = if m.text.is_empty() { m.kind.label().to_owned() } else { m.text.clone() };
            out.push_str(&format!("{}: {}\n", m.author, text.trim()));
        }
        out.push_str("</comment>\n");
    }
    if let Some(note) = note {
        out.push_str(&format!("<note>{note}</note>\n"));
    }
    out.push_str("</doc_comments>");
    out
}

/// `text` as prefixed lines, at most `max` of them (the last ones for a
/// block before the quote), each cut at 200 characters.
fn context_lines(out: &mut String, prefix: &str, text: &str, max: usize, tail: bool) {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let shown: &[&str] = if lines.len() <= max {
        &lines
    } else if tail {
        &lines[lines.len() - max..]
    } else {
        &lines[..max]
    };
    if tail && lines.len() > max {
        out.push_str(&format!("{prefix}…\n"));
    }
    for l in shown {
        let cut: String = l.chars().take(200).collect();
        let more = if l.chars().count() > 200 { "…" } else { "" };
        out.push_str(&format!("{prefix}{cut}{more}\n"));
    }
    if !tail && lines.len() > max {
        out.push_str(&format!("{prefix}…\n"));
    }
}

/// The message that delivers `threads` into a session.
pub fn message(title: &str, path: &Path, threads: &[&Thread], blocks: &[String], resolved: bool) -> String {
    let note = resolved.then_some("Пользователь уже отметил эти комментарии закрытыми: внеси правки, отвечать в тредах не обязательно.");
    let lead = format!(
        "Комментарии к документу «{title}». Поправь документ по ним; ответить в треде — `stepan_verify doc reply {} <id> \"текст\"`.",
        path.display()
    );
    format!("{lead}\n\n{}", to_xml(path, threads, blocks, note))
}

// ------------------------------------------------------------------ cli

pub const USAGE: &str = "\
stepan_verify doc — комментарии пользователя к документам полки

  stepan_verify doc comments FILE [--all]        открытые треды (--all — и закрытые)
  stepan_verify doc reply FILE ID TEXT [--by NAME]
  stepan_verify doc resolve FILE ID... [--by NAME]

FILE — абсолютный путь или путь от папки документов. NAME — из $STEPAN_VERIFY_AGENT,
иначе claude / codex по окружению сессии, иначе «ты».
";

#[derive(Debug, PartialEq)]
enum Command {
    Comments { file: String, all: bool },
    Reply { file: String, id: String, text: String },
    Resolve { file: String, ids: Vec<String> },
    Help,
}

/// (command, --by).
fn parse(raw: &[String]) -> Result<(Command, Option<String>)> {
    let Some((cmd, rest)) = raw.split_first() else { return Ok((Command::Help, None)) };
    let mut positional = Vec::new();
    let (mut by, mut all) = (None, false);
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--by" => by = Some(it.next().context("--by: нужно имя")?.clone()),
            "--all" => all = true,
            flag if flag.starts_with("--") => bail!("неизвестный флаг {flag}"),
            _ => positional.push(a.clone()),
        }
    }
    let command = match (cmd.as_str(), positional.as_slice()) {
        ("comments" | "list" | "ls", [file]) => Command::Comments { file: file.clone(), all },
        ("reply", [file, id, text @ ..]) if !text.is_empty() => Command::Reply { file: file.clone(), id: id.clone(), text: text.join(" ") },
        ("resolve", [file, ids @ ..]) if !ids.is_empty() => Command::Resolve { file: file.clone(), ids: ids.to_vec() },
        ("help" | "--help" | "-h", _) => Command::Help,
        ("comments" | "list" | "ls" | "reply" | "resolve", _) => bail!("не хватает аргументов\n\n{USAGE}"),
        (other, _) => bail!("неизвестная команда `{other}`\n\n{USAGE}"),
    };
    Ok((command, by))
}

/// Who writes: `--by`, else `$STEPAN_VERIFY_AGENT`, else the agent whose
/// session this is, else you. Known names are normalised (ты / claude / codex).
fn author(by: Option<String>, env: impl Fn(&str) -> Option<String>) -> String {
    let raw = by
        .or_else(|| env("STEPAN_VERIFY_AGENT"))
        .or_else(|| (env("CLAUDECODE").is_some() || env("CLAUDE_CODE_SESSION_ID").is_some()).then(|| "claude".to_owned()))
        .or_else(|| env("CODEX_THREAD_ID").map(|_| "codex".to_owned()))
        .unwrap_or_else(|| Author::You.label().to_owned());
    Author::parse(&raw).map_or(raw, |a| a.label().to_owned())
}

/// `file` as (rel inside the docs folder, absolute path).
fn resolve_file(home: &Home, file: &str) -> Result<(String, PathBuf)> {
    let root = home.docs();
    let canon_root = root.canonicalize().unwrap_or_else(|_| root.clone());
    let given = PathBuf::from(file);
    let candidates = if given.is_absolute() {
        vec![given]
    } else {
        let mut c = vec![root.join(&given)];
        if let Ok(cwd) = std::env::current_dir() {
            c.push(cwd.join(&given));
        }
        c
    };
    for path in candidates {
        if !path.is_file() {
            continue;
        }
        let canon = path.canonicalize().unwrap_or_else(|_| path.clone());
        if let Ok(rel) = canon.strip_prefix(&canon_root).or_else(|_| path.strip_prefix(&root)) {
            return Ok((rel.to_string_lossy().replace('\\', "/"), canon));
        }
    }
    bail!("`{file}` — не документ полки {}", root.display())
}

/// Runs `stepan_verify doc <args>`; returns the text to print.
pub fn run(home: &Home, raw: &[String]) -> Result<String> {
    let (command, by) = parse(raw)?;
    let by = author(by, |k| std::env::var(k).ok().filter(|v| !v.is_empty()));
    match command {
        Command::Help => Ok(USAGE.to_owned()),
        Command::Comments { file, all } => {
            let (rel, path) = resolve_file(home, &file)?;
            let threads = threads(&load(home, &rel));
            let shown: Vec<&Thread> = threads.iter().filter(|t| all || !t.root.resolved).collect();
            if shown.is_empty() {
                return Ok(if all { "комментариев нет".into() } else { "открытых комментариев нет".into() });
            }
            Ok(to_xml(&path, &shown, &doc_blocks(&path), None))
        }
        Command::Reply { file, id, text } => {
            let (rel, _) = resolve_file(home, &file)?;
            let c = reply(home, &rel, &id, &text, &by)?;
            Ok(format!("ответил в тред {}: {} ({})", c.reply_to.unwrap_or_default(), c.id, c.author))
        }
        Command::Resolve { file, ids } => {
            let (rel, _) = resolve_file(home, &file)?;
            let roots = set_resolved(home, &rel, &ids, true, &by)?;
            Ok(format!("закрыто: {}", roots.join(", ")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn comment(quote: &str, prev: &str, next: &str) -> Comment {
        Comment {
            id: "c1".into(),
            quote: quote.into(),
            prev: prev.into(),
            next: next.into(),
            text: "x".into(),
            kind: Kind::Comment,
            author: "ты".into(),
            reply_to: None,
            resolved: false,
            created_at: Utc::now(),
            sent_to: None,
        }
    }

    fn temp_home(tag: &str) -> Home {
        let root = std::env::temp_dir().join(format!("stepan_verify-comments-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        Home { root }
    }

    #[test]
    fn splits_blocks() {
        let body = "# Заголовок\nабзац один\nпродолжение\n\n- пункт 1\n  вложенный\n- пункт 2\n\n1. раз\n2. два\n\n```sh\necho 1\n\necho 2\n```\n| a | b |\n|---|---|\n\n> цитата\n";
        let b = blocks(body);
        assert_eq!(
            b,
            s(&["# Заголовок", "абзац один\nпродолжение", "- пункт 1\n  вложенный", "- пункт 2", "1. раз", "2. два", "```sh\necho 1\n\necho 2\n```", "| a | b |\n|---|---|", "> цитата"])
        );
        // A loose item's indented paragraph stays with it.
        assert_eq!(blocks("- пункт\n\n  ещё про пункт\n\nабзац"), s(&["- пункт\n\n  ещё про пункт", "абзац"]));
    }

    #[test]
    fn anchors_and_reanchors() {
        let doc = s(&["# A", "Первый абзац про индекс.", "Второй абзац про fuzzy поиск.", "Третий."]);
        // Exact, and a narrower fragment.
        assert_eq!(anchor(&doc, &comment("Второй абзац про fuzzy поиск.", "", "")), Anchor::Block { ix: 2, moved: false });
        assert_eq!(anchor(&doc, &comment("fuzzy", "", "")), Anchor::Block { ix: 2, moved: false });
        // Whitespace differs.
        assert_eq!(anchor(&doc, &comment("про   fuzzy\nпоиск", "", "")), Anchor::Block { ix: 2, moved: false });
        // Copied from rendered text: markdown marks are gone.
        let md = s(&["Ответ: **ILIKE** без `индекса`.", "- пункт про *кэш*"]);
        assert_eq!(anchor(&md, &comment("ILIKE без индекса", "", "")), Anchor::Block { ix: 0, moved: false });
        assert_eq!(anchor(&md, &comment("пункт про кэш", "", "")), Anchor::Block { ix: 1, moved: false });
        // Rewritten paragraph: found between its old neighbours.
        let rewritten = s(&["# A", "Первый абзац про индекс.", "Совсем другой текст.", "Третий."]);
        let c = comment("Второй абзац про fuzzy поиск.", "Первый абзац про индекс.", "Третий.");
        assert_eq!(anchor(&rewritten, &c), Anchor::Block { ix: 2, moved: true });
        // One neighbour is enough.
        let moved = s(&["Новое начало", "Первый абзац про индекс.", "Другое", "Новый конец"]);
        assert_eq!(anchor(&moved, &c), Anchor::Block { ix: 2, moved: true });
        // Nothing left: stale.
        assert_eq!(anchor(&s(&["ничего общего"]), &c), Anchor::Stale);
        assert_eq!(anchor(&doc, &comment("", "", "")), Anchor::Doc);
        // The same words twice: the neighbours decide.
        let twice = s(&["до", "повтор", "между", "повтор", "после"]);
        assert_eq!(anchor(&twice, &comment("повтор", "между", "после")), Anchor::Block { ix: 3, moved: false });
        assert_eq!(locate(&md, "ILIKE без индекса"), Some(0));
        assert_eq!(locate(&doc, "Третий.\nчто-то ещё"), Some(3));
    }

    #[test]
    fn store_roundtrip() {
        let home = temp_home("store");
        let rel = "proj/research/p95.md";
        let blocks = s(&["# P95", "ILIKE без индекса.", "Вывод."]);
        let a = add(&home, rel, NewComment { text: "Почему не trigram?".into(), author: "ты".into(), ..NewComment::on_block(&blocks, 1, None) }).unwrap();
        assert_eq!((a.quote.as_str(), a.prev.as_str(), a.next.as_str()), ("ILIKE без индекса.", "# P95", "Вывод."));
        let b = add(&home, rel, NewComment { kind: Kind::Approval, author: "ты".into(), ..NewComment::on_block(&blocks, 2, Some("Вывод")) }).unwrap();
        assert_eq!(b.quote, "Вывод");
        assert_ne!(a.id, b.id);
        assert!(add(&home, rel, NewComment { author: "ты".into(), ..NewComment::default() }).is_err(), "an empty remark");

        let r = reply(&home, rel, &a.id, "trigram дороже на запись", "claude").unwrap();
        // A reply to a reply lands in the same thread.
        let r2 = reply(&home, rel, &r.id, "ок", "ты").unwrap();
        assert_eq!(r2.reply_to.as_deref(), Some(a.id.as_str()));
        assert_eq!(open_count(&home, rel), 2);

        let t = threads(&load(&home, rel));
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].replies.len(), 2);
        assert_eq!(t[0].unsent().len(), 2, "your root and your reply, not claude's");

        mark_sent(&home, rel, &[a.id.clone(), r2.id.clone()], "sess-1").unwrap();
        assert!(threads(&load(&home, rel))[0].unsent().is_empty());

        assert_eq!(set_resolved(&home, rel, std::slice::from_ref(&r.id), true, "claude").unwrap(), std::slice::from_ref(&a.id));
        assert_eq!(open_count(&home, rel), 1);
        set_resolved(&home, rel, std::slice::from_ref(&a.id), false, "ты").unwrap();
        assert_eq!(open_count(&home, rel), 2);
        delete(&home, rel, &a.id, "ты").unwrap();
        let left = load(&home, rel);
        assert_eq!(left.len(), 1, "a root goes with its replies");
        assert!(reply(&home, rel, "nope", "x", "ты").is_err());
        // Garbage lines are skipped.
        fs::OpenOptions::new().append(true).open(log_path(&home, rel)).unwrap().write_all(b"not json\n").unwrap();
        assert_eq!(load(&home, rel).len(), 1);
        let _ = fs::remove_dir_all(&home.root);
    }

    #[test]
    fn xml_for_agents() {
        let blocks = s(&["Вступление.", "ILIKE без индекса.", "Вывод."]);
        let mut c = comment("ILIKE без индекса.", "Вступление.", "Вывод.");
        c.id = "c3f1".into();
        c.text = "Почему не trigram?".into();
        let mut reply = comment("", "", "");
        reply.author = "claude".into();
        reply.text = "Дороже на запись".into();
        let t = Thread { root: c, replies: vec![reply] };
        let xml = to_xml(Path::new("/d/p.md"), &[&t], &blocks, None);
        assert_eq!(
            xml,
            "<doc_comments for=\"/d/p.md\">\n<comment id=\"c3f1\" kind=\"comment\">\n  | Вступление.\n> | ILIKE без индекса.\n  | Вывод.\nты: Почему не trigram?\nclaude: Дороже на запись\n</comment>\n</doc_comments>"
        );
        let msg = message("P95", Path::new("/d/p.md"), &[&t], &s(&["другое"]), true);
        assert!(msg.contains("stale=\"true\"") && msg.contains("<note>") && msg.contains("doc reply /d/p.md"));
    }

    #[test]
    fn cli_parsing_and_author() {
        assert_eq!(parse(&s(&["comments", "p.md", "--all"])).unwrap().0, Command::Comments { file: "p.md".into(), all: true });
        assert_eq!(
            parse(&s(&["reply", "p.md", "c1", "готово,", "поправил", "--by", "codex"])).unwrap(),
            (Command::Reply { file: "p.md".into(), id: "c1".into(), text: "готово, поправил".into() }, Some("codex".into()))
        );
        assert_eq!(parse(&s(&["resolve", "p.md", "c1", "c2"])).unwrap().0, Command::Resolve { file: "p.md".into(), ids: s(&["c1", "c2"]) });
        assert!(parse(&s(&["reply", "p.md", "c1"])).is_err());
        assert!(parse(&s(&["resolve", "p.md"])).is_err());
        assert!(parse(&s(&["zap"])).is_err());
        assert_eq!(parse(&[]).unwrap().0, Command::Help);

        let none = |_: &str| None;
        assert_eq!(author(Some("Claude".into()), none), "claude");
        assert_eq!(author(None, none), "ты");
        assert_eq!(author(None, |k| (k == "CLAUDECODE").then(|| "1".into())), "claude");
        assert_eq!(author(None, |k| (k == "CODEX_THREAD_ID").then(|| "t".into())), "codex");
        assert_eq!(author(None, |k| (k == "STEPAN_VERIFY_AGENT").then(|| "codex".into())), "codex");
    }

    #[test]
    fn cli_roundtrip() {
        let home = temp_home("cli");
        let dir = home.docs().join("proj");
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("p.md");
        fs::write(&file, "---\ntitle: P\n---\n# P\n\nILIKE без индекса.\n").unwrap();
        let rel = "proj/p.md";
        let blocks = doc_blocks(&file);
        let c = add(&home, rel, NewComment { text: "Почему?".into(), author: "ты".into(), ..NewComment::on_block(&blocks, 1, None) }).unwrap();

        let out = run(&home, &s(&["comments", rel])).unwrap();
        assert!(out.contains(&format!("id=\"{}\"", c.id)) && out.contains("> | ILIKE без индекса."), "{out}");
        let abs = file.to_string_lossy().into_owned();
        assert!(run(&home, &s(&["reply", &abs, &c.id, "Потому что", "--by", "codex"])).unwrap().contains("codex"));
        assert!(run(&home, &s(&["comments", rel])).unwrap().contains("codex: Потому что"));
        assert!(run(&home, &s(&["resolve", rel, &c.id, "--by", "codex"])).unwrap().contains(&c.id));
        assert_eq!(run(&home, &s(&["comments", rel])).unwrap(), "открытых комментариев нет");
        assert!(run(&home, &s(&["comments", rel, "--all"])).unwrap().contains("resolved=\"true\""));
        assert!(run(&home, &s(&["comments", "/etc/hosts"])).is_err(), "outside the shelf");
        let _ = fs::remove_dir_all(&home.root);
    }
}
