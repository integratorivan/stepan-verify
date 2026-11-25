//! Язык описания флоу: `.stepan_verify/flows/` в репозитории проекта.
//!
//! ```text
//! .stepan_verify/flows/
//!   app.yml              навигация всего приложения: старт, экраны, переходы
//!   features/<id>.yml    одна фича: шаги «экран: действие -> результат», доказательства
//!   .sync.yml            служебное: с какого коммита карта сверена
//! ```
//!
//! Файлы выглядят как YAML и открываются любым редактором, но читает их свой
//! построчный парсер: он прощает то, на чём YAML падает (`/catalog/[id]`,
//! двоеточия внутри текста), и даёт ошибки с номером строки. Грамматика — в
//! `docs/FLOWS.md`; [`compile`] собирает из папки [`FlowMap`], [`write`] пишет
//! обратно (не трогая файлы, смысл которых не изменился).

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};

use crate::flows::{AppNav, Area, Edge, Feature, FlowMap, FlowPath, Ignored, Proof, ProofKind, Screen, Step};

pub const DIR: &str = ".stepan_verify/flows";
pub const APP: &str = "app.yml";
pub const FEATURES: &str = "features";
pub const SYNC: &str = ".sync.yml";

pub const KINDS: [&str; 6] = ["page", "dialog", "sheet", "tab", "panel", "external"];
pub const STATUSES: [&str; 5] = ["done", "in_progress", "planned", "broken", "draft"];

pub fn dir(repo: &Path) -> PathBuf {
    repo.join(DIR)
}

// ------------------------------------------------------------------ diagnostics

/// A problem in a flow file: `features/cart.yml:12: …`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diag {
    /// Relative to the flows folder: `app.yml`, `features/cart.yml`.
    pub file: String,
    /// 1-based; 0 = the whole file.
    pub line: usize,
    pub msg: String,
    /// Errors stop loading; warnings (dangling references) only show up in `check`.
    pub error: bool,
}

impl fmt::Display for Diag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let level = if self.error { "ошибка" } else { "предупреждение" };
        if self.line > 0 {
            write!(f, "{}:{}: {level}: {}", self.file, self.line, self.msg)
        } else {
            write!(f, "{}: {level}: {}", self.file, self.msg)
        }
    }
}

struct Diags<'a> {
    file: &'a str,
    out: &'a mut Vec<Diag>,
}

impl Diags<'_> {
    fn error(&mut self, line: usize, msg: impl Into<String>) {
        self.out.push(Diag { file: self.file.to_owned(), line, msg: msg.into(), error: true });
    }
    fn warn(&mut self, line: usize, msg: impl Into<String>) {
        self.out.push(Diag { file: self.file.to_owned(), line, msg: msg.into(), error: false });
    }
}

// ------------------------------------------------------------------ outline parser

#[derive(Clone, Debug, PartialEq)]
enum Kind {
    /// `key: value` or `key:` with a block under it.
    Pair(String, String),
    /// `- text`
    Item(String),
    /// `-> text`
    Edge(String),
}

#[derive(Clone, Debug)]
struct Node {
    line: usize,
    kind: Kind,
    children: Vec<Node>,
}

struct Line {
    no: usize,
    indent: usize,
    kind: Kind,
}

/// Split a file into an indentation tree. Comments: a line starting with `#`,
/// or `  #` (two spaces) after text — `#5` inside text stays text.
fn outline(text: &str, d: &mut Diags) -> Vec<Node> {
    let mut lines = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let no = i + 1;
        if raw.contains('\t') {
            d.error(no, "табуляция в отступе или тексте — используй пробелы");
        }
        let raw = raw.replace('\t', "  ");
        let body = match raw.find("  #") {
            Some(at) => &raw[..at],
            None => &raw[..],
        };
        let trimmed = body.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indent = body.len() - body.trim_start().len();
        let kind = if let Some(rest) = trimmed.strip_prefix("->") {
            Kind::Edge(rest.trim().to_owned())
        } else if trimmed == "-" {
            Kind::Item(String::new())
        } else if let Some(rest) = trimmed.strip_prefix("- ") {
            Kind::Item(rest.trim().to_owned())
        } else {
            match split_key(trimmed) {
                Some((k, v)) => Kind::Pair(k.to_owned(), v.to_owned()),
                None => {
                    d.error(no, format!("ожидалось «ключ: значение», «- пункт» или «-> переход», а тут «{trimmed}»"));
                    continue;
                }
            }
        };
        lines.push(Line { no, indent, kind });
    }
    let mut i = 0;
    block(&lines, &mut i, 0)
}

fn block(lines: &[Line], i: &mut usize, indent: usize) -> Vec<Node> {
    let mut out = Vec::new();
    while *i < lines.len() {
        let l = &lines[*i];
        if l.indent < indent {
            break;
        }
        *i += 1;
        let children = if *i < lines.len() && lines[*i].indent > l.indent { block(lines, i, lines[*i].indent) } else { Vec::new() };
        out.push(Node { line: l.no, kind: l.kind.clone(), children });
    }
    out
}

/// `key: value` → (key, value); the key ends at the first `:` followed by a space or the end.
fn split_key(s: &str) -> Option<(&str, &str)> {
    let mut chars = s.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        if c == ':' {
            match chars.peek() {
                None => return Some((s[..at].trim(), "")),
                Some((_, ' ')) => return Some((s[..at].trim(), s[at + 1..].trim())),
                _ => {}
            }
        }
    }
    None
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    for q in ['"', '\''] {
        if s.len() >= 2 && s.starts_with(q) && s.ends_with(q) {
            return s[1..s.len() - 1].to_owned();
        }
    }
    s.to_owned()
}

/// `[a, b, /catalog/[id]]` or a block of `- item` children.
fn list(value: &str, children: &[Node], d: &mut Diags, line: usize) -> Vec<String> {
    let v = value.trim();
    if v.is_empty() {
        return children
            .iter()
            .filter_map(|n| match &n.kind {
                Kind::Item(t) => Some(unquote(t)),
                _ => {
                    d.error(n.line, "в списке ожидались строки «- …»");
                    None
                }
            })
            .filter(|s| !s.is_empty())
            .collect();
    }
    let inner = match v.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
        Some(inner) => inner,
        None => {
            d.error(line, "список пишется как [a, b] или блоком «- a»");
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    let (mut depth, mut cur) = (0i32, String::new());
    for c in inner.chars() {
        match c {
            '[' => depth += 1,
            ']' => depth -= 1,
            ',' if depth == 0 => {
                out.push(unquote(&cur));
                cur.clear();
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    out.push(unquote(&cur));
    out.retain(|s| !s.is_empty());
    out
}

// ------------------------------------------------------------------ grammar pieces

/// `Экран: действие -> результат  [Сущность, файл.ts]` → a step (id set by the caller).
pub fn parse_step(text: &str) -> Step {
    let text = text.trim();
    let (body, refs) = trailing_refs(text);
    let (screen, rest) = match split_key(body) {
        Some((s, r)) => (s.to_owned(), r.to_owned()),
        None => (body.to_owned(), String::new()),
    };
    let (action, result) = match rest.split_once(" -> ") {
        Some((a, r)) => (a.trim().to_owned(), r.trim().to_owned()),
        None => match rest.strip_prefix("-> ") {
            Some(r) => (String::new(), r.trim().to_owned()),
            None => (rest.trim().to_owned(), String::new()),
        },
    };
    let (mut entities, mut files) = (Vec::new(), Vec::new());
    for r in refs {
        if is_file(&r) { files.push(r) } else { entities.push(r) }
    }
    Step { id: String::new(), screen: unquote(&screen), action, result, entities, files }
}

/// A `[...]` group at the very end, after a space: references to code.
/// `/catalog/[id]` (no space before `[`) stays text.
fn trailing_refs(text: &str) -> (&str, Vec<String>) {
    if !text.ends_with(']') {
        return (text, Vec::new());
    }
    let mut depth = 0i32;
    for (at, c) in text.char_indices().rev() {
        match c {
            ']' => depth += 1,
            '[' => {
                depth -= 1;
                if depth == 0 {
                    if at > 0 && text[..at].ends_with(' ') {
                        let inner = &text[at + 1..text.len() - 1];
                        let refs = inner.split(',').map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()).collect();
                        return (text[..at].trim_end(), refs);
                    }
                    return (text, Vec::new());
                }
            }
            _ => {}
        }
    }
    (text, Vec::new())
}

const FILE_EXTS: [&str; 30] = [
    "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "vue", "svelte", "swift", "kt", "kts", "py", "go", "css", "scss", "html", "rb", "java",
    "dart", "m", "mm", "json", "yml", "yaml", "md", "sql", "c", "cpp", "h",
];

fn is_file(r: &str) -> bool {
    if r.contains('/') {
        return true;
    }
    let base = r.split("::").next().unwrap_or(r);
    base.rsplit_once('.').is_some_and(|(_, ext)| FILE_EXTS.contains(&ext))
}

pub fn render_step(s: &Step) -> String {
    let clean = |t: &str| t.replace('\n', " ").replace(" -> ", " → ");
    let screen = clean(&s.screen).replace(": ", " — ");
    let mut out = screen;
    out.push(':');
    if !s.action.is_empty() {
        out.push(' ');
        out.push_str(&clean(&s.action));
    }
    if !s.result.is_empty() {
        out.push_str(" -> ");
        out.push_str(&clean(&s.result));
    }
    let refs: Vec<&String> = s.entities.iter().chain(&s.files).collect();
    if !refs.is_empty() {
        out.push_str("  [");
        out.push_str(&refs.iter().map(|r| r.as_str()).collect::<Vec<_>>().join(", "));
        out.push(']');
    }
    out
}

/// `Цель: действие (Фича)` → an edge.
pub fn parse_edge(text: &str) -> Edge {
    let (to, rest) = match split_key(text) {
        Some((t, r)) => (t.to_owned(), r.to_owned()),
        None => (text.trim().trim_end_matches(':').to_owned(), String::new()),
    };
    let (action, feature) = trailing_feature(&rest);
    Edge { to: unquote(&to), action, feature }
}

fn trailing_feature(text: &str) -> (String, String) {
    let t = text.trim();
    if !t.ends_with(')') {
        return (t.to_owned(), String::new());
    }
    let mut depth = 0i32;
    for (at, c) in t.char_indices().rev() {
        match c {
            ')' => depth += 1,
            '(' => {
                depth -= 1;
                if depth == 0 {
                    if at == 0 || t[..at].ends_with(' ') {
                        return (t[..at].trim().to_owned(), t[at + 1..t.len() - 1].trim().to_owned());
                    }
                    break;
                }
            }
            _ => {}
        }
    }
    (t.to_owned(), String::new())
}

fn render_edge(e: &Edge) -> String {
    let mut out = format!("-> {}", e.to.replace(": ", " — "));
    if !e.action.is_empty() || !e.feature.is_empty() {
        out.push(':');
    }
    if !e.action.is_empty() {
        out.push(' ');
        out.push_str(&e.action.replace('\n', " "));
    }
    if !e.feature.is_empty() {
        out.push_str(&format!(" ({})", e.feature));
    }
    out
}

/// `test: path::name  covers: 1, 2  at: 2026-…  title: Текст`
fn parse_proof(text: &str, d: &mut Diags, line: usize) -> Option<Proof> {
    let Some((kind, rest)) = split_key(text) else {
        d.error(line, "доказательство пишется как «test: путь», «screenshot: путь», «video: …», «cli: команда» или «manual: что проверено»");
        return None;
    };
    let kind = match kind {
        "test" | "тест" => ProofKind::Test,
        "screenshot" | "скриншот" => ProofKind::Screenshot,
        "video" | "видео" => ProofKind::Video,
        "cli" => ProofKind::Cli,
        "manual" | "руками" => ProofKind::Manual,
        other => {
            d.error(line, format!("неизвестный вид доказательства «{other}» — test | screenshot | video | cli | manual"));
            return None;
        }
    };
    // Keywords split the rest: everything before the first one is the reference.
    let keys = ["covers:", "at:", "title:"];
    let mut marks: Vec<(usize, &str)> = Vec::new();
    for k in keys {
        let mut from = 0;
        while let Some(pos) = rest[from..].find(k) {
            let at = from + pos;
            if at == 0 || rest[..at].ends_with(' ') {
                marks.push((at, k));
                break;
            }
            from = at + k.len();
        }
    }
    marks.sort();
    let reference = rest[..marks.first().map_or(rest.len(), |m| m.0)].trim().to_owned();
    let mut proof = Proof { kind, reference, ..Default::default() };
    for (n, (at, k)) in marks.iter().enumerate() {
        let end = marks.get(n + 1).map_or(rest.len(), |m| m.0);
        let value = rest[at + k.len()..end].trim();
        match *k {
            "covers:" => proof.covers = value.split(',').map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()).collect(),
            "at:" => match DateTime::parse_from_rfc3339(value) {
                Ok(t) => proof.at = Some(t.with_timezone(&Utc)),
                Err(_) => d.error(line, format!("«at: {value}» — нужна дата в формате 2026-11-25T10:00:00Z")),
            },
            _ => proof.title = value.to_owned(),
        }
    }
    Some(proof)
}

fn render_proof(p: &Proof) -> String {
    let kind = match p.kind {
        ProofKind::Test => "test",
        ProofKind::Screenshot => "screenshot",
        ProofKind::Video => "video",
        ProofKind::Cli => "cli",
        ProofKind::Manual => "manual",
    };
    let mut out = format!("{kind}: {}", p.reference.replace('\n', " "));
    if !p.covers.is_empty() {
        out.push_str(&format!("  covers: {}", p.covers.join(", ")));
    }
    if let Some(at) = p.at {
        out.push_str(&format!("  at: {}", at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)));
    }
    if !p.title.is_empty() {
        out.push_str(&format!("  title: {}", p.title.replace('\n', " ")));
    }
    out
}

pub fn slug(title: &str) -> String {
    let mut out = String::new();
    for c in title.trim().to_lowercase().chars() {
        if c.is_alphanumeric() || c == '.' || c == '_' {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_matches('-').to_owned();
    if out.is_empty() { "misc".into() } else { out }
}

/// A feature id that is safe as a file name.
fn file_stem(id: &str) -> String {
    id.chars().map(|c| if c.is_alphanumeric() || matches!(c, '.' | '_' | '-') { c } else { '-' }).collect()
}

// ------------------------------------------------------------------ files → model

/// `app.yml` → navigation, area order and ignored entries.
fn compile_app(text: &str, d: &mut Diags) -> (AppNav, Vec<String>, Vec<Ignored>) {
    let mut nav = AppNav::default();
    let (mut areas, mut ignored) = (Vec::new(), Vec::new());
    let mut seen = BTreeSet::new();
    for n in outline(text, d) {
        let Kind::Pair(key, value) = &n.kind else {
            d.error(n.line, "на верхнем уровне app.yml ожидались app / start / areas / screens / ignored");
            continue;
        };
        match key.as_str() {
            "app" => nav.app = unquote(value),
            "start" => nav.start = unquote(value),
            "areas" => areas = list(value, &n.children, d, n.line),
            "ignored" => {
                for c in &n.children {
                    let text = match &c.kind {
                        Kind::Item(t) => t.clone(),
                        _ => {
                            d.error(c.line, "в ignored ожидались строки «- точка входа — причина»");
                            continue;
                        }
                    };
                    let (entry, reason) = text.split_once(" — ").unwrap_or((&text, ""));
                    ignored.push(Ignored { entry: unquote(entry), reason: reason.trim().to_owned() });
                }
            }
            "screens" => {
                for s in &n.children {
                    let Kind::Pair(name, inline) = &s.kind else {
                        d.error(s.line, "экран пишется как «Имя экрана:» с переходами «-> …» под ним");
                        continue;
                    };
                    let name = unquote(name);
                    if !seen.insert(name.clone()) {
                        d.error(s.line, format!("экран «{name}» описан дважды"));
                    }
                    let mut screen = Screen { name, kind: "page".into(), ..Default::default() };
                    if !inline.is_empty() {
                        screen.about = unquote(inline);
                    }
                    for c in &s.children {
                        match &c.kind {
                            Kind::Edge(t) => screen.edges.push(parse_edge(t)),
                            Kind::Pair(k, v) => match k.as_str() {
                                "kind" => {
                                    let v = unquote(v);
                                    if !KINDS.contains(&v.as_str()) {
                                        d.error(c.line, format!("kind «{v}» — бывает page | dialog | sheet | tab | panel | external"));
                                    }
                                    screen.kind = v;
                                }
                                "route" => screen.route = unquote(v),
                                "about" => screen.about = unquote(v),
                                other => d.warn(c.line, format!("неизвестное поле экрана «{other}» (kind, route, about, -> переход)")),
                            },
                            Kind::Item(_) => d.error(c.line, "переход пишется со стрелкой: «-> Экран: действие»"),
                        }
                    }
                    nav.screens.push(screen);
                }
            }
            other => d.warn(n.line, format!("неизвестное поле «{other}»")),
        }
    }
    (nav, areas, ignored)
}

/// One `features/<id>.yml` → a feature (area holds the area *title* here).
fn compile_feature(id: &str, text: &str, d: &mut Diags) -> Option<Feature> {
    let mut f = Feature { id: id.to_owned(), ..Default::default() };
    let mut step_no = 0usize;
    let mut steps = |items: &[Node], d: &mut Diags| -> Vec<Step> {
        let mut out = Vec::new();
        for n in items {
            match &n.kind {
                Kind::Item(t) if !t.is_empty() => {
                    step_no += 1;
                    let mut s = parse_step(t);
                    s.id = step_no.to_string();
                    if s.screen.is_empty() {
                        d.error(n.line, "у шага нет экрана: «Экран: действие -> результат»");
                    }
                    out.push(s);
                }
                _ => d.error(n.line, "шаг пишется как «- Экран: действие -> результат  [код]»"),
            }
        }
        out
    };
    let mut proofs_nodes: Vec<Node> = Vec::new();
    for n in outline(text, d) {
        let Kind::Pair(key, value) = &n.kind else {
            d.error(n.line, "на верхнем уровне ожидались поля фичи: feature, area, status, summary, steps, proofs…");
            continue;
        };
        match key.as_str() {
            "feature" => f.title = unquote(value),
            "id" => f.id = unquote(value),
            "area" => f.area = unquote(value),
            "status" => {
                let v = unquote(value);
                if !STATUSES.contains(&v.as_str()) {
                    d.error(n.line, format!("status «{v}» — бывает done | in_progress | planned | broken | draft"));
                }
                f.status = v;
            }
            "summary" => f.summary = unquote(value),
            "aliases" => f.aliases = list(value, &n.children, d, n.line),
            "entries" => f.entries = list(value, &n.children, d, n.line),
            "starts_at" => f.starts_at = unquote(value),
            "steps" => {
                let s = steps(&n.children, d);
                f.paths.push(FlowPath { id: "main".into(), title: String::new(), steps: s });
            }
            "paths" => {
                for p in &n.children {
                    let Kind::Pair(title, _) = &p.kind else {
                        d.error(p.line, "путь пишется как «Название пути:» со списком шагов под ним");
                        continue;
                    };
                    let title = unquote(title);
                    let mut pid = slug(&title);
                    while f.paths.iter().any(|x| x.id == pid) {
                        pid.push('-');
                    }
                    let s = steps(&p.children, d);
                    f.paths.push(FlowPath { id: pid, title, steps: s });
                }
            }
            "proofs" => proofs_nodes = n.children.clone(),
            other => d.warn(n.line, format!("неизвестное поле «{other}»")),
        }
    }
    for n in &proofs_nodes {
        match &n.kind {
            Kind::Item(t) => {
                if let Some(p) = parse_proof(t, d, n.line) {
                    for c in &p.covers {
                        if c.parse::<usize>().map_or(true, |k| k == 0 || k > step_no) {
                            d.error(n.line, format!("covers: {c} — такого шага нет (шагов {step_no})"));
                        }
                    }
                    f.proofs.push(p);
                }
            }
            _ => d.error(n.line, "доказательство пишется как «- test: путь  covers: 1, 2»"),
        }
    }
    if f.title.is_empty() {
        d.error(0, "нет названия: «feature: Как фичу назвал бы пользователь»");
        return None;
    }
    Some(f)
}

fn compile_sync(text: &str, map: &mut FlowMap, d: &mut Diags) {
    for n in outline(text, d) {
        if let Kind::Pair(k, v) = &n.kind {
            let v = unquote(v);
            match k.as_str() {
                "synced_commit" if !v.is_empty() => map.synced_commit = Some(v),
                "updated_at" => map.updated_at = DateTime::parse_from_rfc3339(&v).ok().map(|t| t.with_timezone(&Utc)),
                "project" => map.project = v,
                _ => {}
            }
        }
    }
}

/// Read the flows folder into a map. `None` when the folder does not exist.
/// Diagnostics include warnings (dangling screen / feature references).
pub fn compile(repo: &Path) -> Result<Option<(FlowMap, Vec<Diag>)>> {
    let root = dir(repo);
    if !root.is_dir() {
        return Ok(None);
    }
    let mut diags = Vec::new();
    let mut map = FlowMap { version: 1, ..Default::default() };
    let mut area_order: Vec<String> = Vec::new();
    let app = root.join(APP);
    if app.exists() {
        let text = fs::read_to_string(&app).with_context(|| format!("read {}", app.display()))?;
        let mut d = Diags { file: APP, out: &mut diags };
        let (nav, areas, ignored) = compile_app(&text, &mut d);
        map.nav = nav;
        area_order = areas;
        map.ignored = ignored;
    }
    let sync = root.join(SYNC);
    if let Ok(text) = fs::read_to_string(&sync) {
        compile_sync(&text, &mut map, &mut Diags { file: SYNC, out: &mut diags });
    }
    if map.project.is_empty() {
        map.project = repo.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    }
    let mut files: Vec<PathBuf> = fs::read_dir(root.join(FEATURES))
        .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "yml" || e == "yaml")).collect())
        .unwrap_or_default();
    files.sort();
    let mut titles: Vec<String> = area_order.clone();
    for path in files {
        let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let rel = format!("{FEATURES}/{}", path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default());
        let text = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let mut d = Diags { file: &rel, out: &mut diags };
        if let Some(mut f) = compile_feature(&stem, &text, &mut d) {
            let title = if f.area.is_empty() { "Без области".to_owned() } else { f.area.clone() };
            if !titles.contains(&title) {
                titles.push(title.clone());
            }
            f.area = slug(&title);
            map.features.push(f);
        }
    }
    let used: BTreeSet<String> = map.features.iter().map(|f| f.area.clone()).collect();
    map.areas = titles.into_iter().map(|t| Area { id: slug(&t), title: t }).filter(|a| used.contains(&a.id)).collect();
    cross_check(&map, &mut diags);
    Ok(Some((map, diags)))
}

/// References between files: screens of steps and edges, features of edges.
fn cross_check(map: &FlowMap, diags: &mut Vec<Diag>) {
    if map.nav.screens.is_empty() {
        if !map.features.is_empty() {
            diags.push(Diag { file: APP.into(), line: 0, msg: "навигация не описана: нет экранов в app.yml".into(), error: false });
        }
        return;
    }
    let screens: BTreeSet<&str> = map.nav.screens.iter().map(|s| s.name.as_str()).collect();
    let features: BTreeSet<&str> = map.features.iter().map(|f| f.title.as_str()).collect();
    let mut warn = |file: String, msg: String| diags.push(Diag { file, line: 0, msg, error: false });
    if !map.nav.start.is_empty() && !screens.contains(map.nav.start.as_str()) {
        warn(APP.into(), format!("start: «{}» — такого экрана нет в screens", map.nav.start));
    }
    for s in &map.nav.screens {
        for e in &s.edges {
            if !screens.contains(e.to.as_str()) {
                warn(APP.into(), format!("«{}» -> «{}»: экран «{}» не описан", s.name, e.to, e.to));
            }
            if !e.feature.is_empty() && !features.contains(e.feature.as_str()) {
                warn(APP.into(), format!("«{}» -> «{}»: фичи «{}» нет в features/", s.name, e.to, e.feature));
            }
        }
    }
    for f in &map.features {
        let file = format!("{FEATURES}/{}.yml", file_stem(&f.id));
        if !f.starts_at.is_empty() && !screens.contains(f.starts_at.as_str()) {
            warn(file.clone(), format!("starts_at: «{}» — такого экрана нет в app.yml", f.starts_at));
        }
        for s in f.steps() {
            if !screens.contains(s.screen.as_str()) {
                warn(file.clone(), format!("шаг {}: экран «{}» не описан в app.yml", s.id, s.screen));
            }
        }
    }
}

/// Errors (not warnings) as one readable message.
pub fn errors_text(diags: &[Diag]) -> Option<String> {
    let errors: Vec<String> = diags.iter().filter(|d| d.error).map(|d| d.to_string()).collect();
    (!errors.is_empty()).then(|| errors.join("\n"))
}

/// Parse loose feature files an agent wrote (a pass result): `<dir>/*.yml`.
pub fn read_features(dir: &Path) -> (Vec<Feature>, Vec<Diag>) {
    let mut diags = Vec::new();
    let mut out = Vec::new();
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "yml" || e == "yaml")).collect())
        .unwrap_or_default();
    files.sort();
    for path in files {
        let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let name = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let Ok(text) = fs::read_to_string(&path) else { continue };
        let mut d = Diags { file: &name, out: &mut diags };
        if let Some(mut f) = compile_feature(&stem, &text, &mut d) {
            f.area = slug(if f.area.is_empty() { "Без области" } else { &f.area });
            out.push(f);
        }
    }
    (out, diags)
}

/// Parse an `app.yml` an agent wrote.
pub fn read_app(path: &Path) -> Result<(AppNav, Vec<Diag>)> {
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let mut diags = Vec::new();
    let name = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let (nav, _, _) = compile_app(&text, &mut Diags { file: &name, out: &mut diags });
    Ok((nav, diags))
}

// ------------------------------------------------------------------ model → files

pub fn render_app(map: &FlowMap) -> String {
    let nav = &map.nav;
    let mut out = String::from("# Навигация приложения: откуда куда можно перейти. Язык: docs/FLOWS.md в Stepan.\n");
    out.push_str(&format!("app: {}\n", if nav.app.is_empty() { &map.project } else { &nav.app }));
    if !nav.start.is_empty() {
        out.push_str(&format!("start: {}\n", nav.start));
    }
    if !map.areas.is_empty() {
        out.push_str(&format!("areas: [{}]\n", map.areas.iter().map(|a| a.title.as_str()).collect::<Vec<_>>().join(", ")));
    }
    if !nav.screens.is_empty() {
        out.push_str("\nscreens:\n");
        for (i, s) in nav.screens.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            out.push_str(&format!("  {}:\n", s.name.replace(": ", " — ")));
            if !s.kind.is_empty() && s.kind != "page" {
                out.push_str(&format!("    kind: {}\n", s.kind));
            }
            if !s.route.is_empty() {
                out.push_str(&format!("    route: {}\n", s.route));
            }
            if !s.about.is_empty() {
                out.push_str(&format!("    about: {}\n", s.about.replace('\n', " ")));
            }
            for e in &s.edges {
                out.push_str(&format!("    {}\n", render_edge(e)));
            }
        }
    }
    if !map.ignored.is_empty() {
        out.push_str("\nignored:\n");
        for i in &map.ignored {
            if i.reason.is_empty() {
                out.push_str(&format!("  - {}\n", i.entry));
            } else {
                out.push_str(&format!("  - {} — {}\n", i.entry, i.reason.replace('\n', " ")));
            }
        }
    }
    out
}

pub fn render_feature(f: &Feature, area_title: &str) -> String {
    let mut out = format!("feature: {}\n", f.title.replace('\n', " "));
    if !area_title.is_empty() {
        out.push_str(&format!("area: {area_title}\n"));
    }
    if !f.status.is_empty() {
        out.push_str(&format!("status: {}\n", f.status));
    }
    if !f.summary.is_empty() {
        out.push_str(&format!("summary: {}\n", f.summary.replace('\n', " ")));
    }
    let inline = |v: &[String]| format!("[{}]", v.join(", "));
    if !f.aliases.is_empty() {
        out.push_str(&format!("aliases: {}\n", inline(&f.aliases)));
    }
    if !f.entries.is_empty() {
        out.push_str(&format!("entries: {}\n", inline(&f.entries)));
    }
    if !f.starts_at.is_empty() {
        out.push_str(&format!("starts_at: {}\n", f.starts_at));
    }
    let single = f.paths.len() == 1 && (f.paths[0].title.is_empty() || f.paths[0].id == "main");
    if single {
        out.push_str("\nsteps:\n");
        for s in &f.paths[0].steps {
            out.push_str(&format!("  - {}\n", render_step(s)));
        }
    } else if !f.paths.is_empty() {
        out.push_str("\npaths:\n");
        for p in &f.paths {
            let title = if p.title.is_empty() { p.id.clone() } else { p.title.clone() };
            out.push_str(&format!("  {}:\n", title.replace(": ", " — ")));
            for s in &p.steps {
                out.push_str(&format!("    - {}\n", render_step(s)));
            }
        }
    }
    if !f.proofs.is_empty() {
        out.push_str("\nproofs:\n");
        for p in &f.proofs {
            out.push_str(&format!("  - {}\n", render_proof(p)));
        }
    }
    out
}

fn render_sync(map: &FlowMap) -> String {
    let mut out = String::from("# Служебное: с какого коммита карта сверена с кодом. Руками не править.\n");
    out.push_str(&format!("project: {}\n", map.project));
    if let Some(c) = &map.synced_commit {
        out.push_str(&format!("synced_commit: {c}\n"));
    }
    if let Some(t) = map.updated_at {
        out.push_str(&format!("updated_at: {}\n", t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)));
    }
    out
}

/// Step ids are positions in the file; covers point at them. Renumber so a
/// map from elsewhere (old yaml, an agent) round-trips through the files.
fn renumber(f: &Feature) -> Feature {
    let mut f = f.clone();
    let mut map: BTreeMap<String, String> = BTreeMap::new();
    let mut n = 0;
    for p in &mut f.paths {
        for s in &mut p.steps {
            n += 1;
            map.insert(s.id.clone(), n.to_string());
            s.id = n.to_string();
        }
    }
    for proof in &mut f.proofs {
        proof.covers = proof.covers.iter().filter_map(|c| map.get(c).cloned()).collect();
    }
    f
}

/// Write the map as flow files. A feature file whose meaning did not change is
/// left alone (keeps your formatting and comments); `force` rewrites all (fmt).
pub fn write(repo: &Path, map: &FlowMap, force: bool) -> Result<()> {
    let root = dir(repo);
    let features_dir = root.join(FEATURES);
    fs::create_dir_all(&features_dir)?;
    let title_of = |id: &str| map.areas.iter().find(|a| a.id == id).map(|a| a.title.clone()).unwrap_or_else(|| id.to_owned());
    // app.yml: rewrite only when its meaning changed.
    let app_path = root.join(APP);
    let app_text = render_app(map);
    let same_app = !force
        && fs::read_to_string(&app_path).ok().is_some_and(|old| {
            let mut sink = Vec::new();
            let (nav, areas, ignored) = compile_app(&old, &mut Diags { file: APP, out: &mut sink });
            let want: Vec<String> = map.areas.iter().map(|a| a.title.clone()).collect();
            nav == map.nav && ignored == map.ignored && (areas.is_empty() || areas == want)
        });
    if !same_app && (!map.nav.screens.is_empty() || !map.nav.start.is_empty() || app_path.exists() || !map.areas.is_empty()) {
        write_atomic(&app_path, &app_text)?;
    }
    let mut keep = BTreeSet::new();
    for f in &map.features {
        let stem = file_stem(&f.id);
        keep.insert(stem.clone());
        let path = features_dir.join(format!("{stem}.yml"));
        let area = title_of(&f.area);
        let want = renumber(f);
        if !force {
            if let Ok(old) = fs::read_to_string(&path) {
                let mut sink = Vec::new();
                if let Some(mut was) = compile_feature(&stem, &old, &mut Diags { file: "", out: &mut sink }) {
                    was.area = slug(if was.area.is_empty() { "Без области" } else { &was.area });
                    let mut cmp = want.clone();
                    cmp.id = stem.clone();
                    cmp.area = slug(if area.is_empty() { "Без области" } else { &area });
                    if was == cmp && !sink.iter().any(|d| d.error) {
                        continue;
                    }
                }
            }
        }
        write_atomic(&path, &render_feature(&want, &area))?;
    }
    // Features removed from the map lose their file.
    if let Ok(rd) = fs::read_dir(&features_dir) {
        for e in rd.flatten() {
            let p = e.path();
            let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            if p.extension().is_some_and(|x| x == "yml" || x == "yaml") && !keep.contains(&stem) {
                let _ = fs::remove_file(p);
            }
        }
    }
    write_atomic(&root.join(SYNC), &render_sync(map))?;
    Ok(())
}

fn write_atomic(path: &Path, text: &str) -> Result<()> {
    let tmp = path.with_extension("yml.tmp");
    fs::write(&tmp, text)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

/// `stepan_verify flows check [<repo>]`: every problem, errors first.
pub fn check(repo: &Path) -> Result<Vec<Diag>> {
    match compile(repo)? {
        Some((_, mut diags)) => {
            diags.sort_by_key(|d| (!d.error, d.file.clone(), d.line));
            Ok(diags)
        }
        None => Err(anyhow!("нет карты флоу: {} (построй её на экране «Флоу» в Stepan)", dir(repo).display())),
    }
}

/// `stepan_verify flows fmt [<repo>]`: rewrite every file in the canonical form.
pub fn fmt(repo: &Path) -> Result<usize> {
    let (map, diags) = compile(repo)?.ok_or_else(|| anyhow!("нет карты флоу в {}", dir(repo).display()))?;
    if let Some(errors) = errors_text(&diags) {
        return Err(anyhow!("сначала исправь ошибки:\n{errors}"));
    }
    write(repo, &map, true)?;
    Ok(map.features.len())
}

/// `stepan_verify flows check|fmt [<repo>]`. Returns (output, ok). The repo
/// defaults to the current folder or its nearest parent with `.stepan_verify/flows`.
pub fn cli(args: &[String]) -> Result<(String, bool)> {
    let repo = |arg: Option<&String>| -> PathBuf {
        if let Some(a) = arg {
            return PathBuf::from(a);
        }
        let cwd = std::env::current_dir().unwrap_or_default();
        cwd.ancestors().find(|d| dir(d).is_dir() || d.join(crate::flows::FILE).is_file()).map(Path::to_path_buf).unwrap_or(cwd)
    };
    match args.first().map(String::as_str) {
        Some("check") => {
            let r = repo(args.get(1));
            // An old single-file map is migrated first, like the app does.
            crate::flows::FlowMap::load(&r).ok();
            let diags = check(&r)?;
            let errors = diags.iter().filter(|d| d.error).count();
            let warnings = diags.len() - errors;
            let mut out: Vec<String> = diags.iter().map(|d| d.to_string()).collect();
            out.push(match (errors, warnings) {
                (0, 0) => format!("ok: {} — ошибок нет", dir(&r).display()),
                (0, w) => format!("ok: ошибок нет, предупреждений: {w}"),
                (e, w) => format!("ошибок: {e}, предупреждений: {w}"),
            });
            Ok((out.join("\n"), errors == 0))
        }
        Some("fmt") => {
            let r = repo(args.get(1));
            let n = fmt(&r)?;
            Ok((format!("переписано в каноническом виде: {n} фич + app.yml ({})", dir(&r).display()), true))
        }
        _ => Ok((
            "stepan_verify flows check [<repo>]  — проверить карту флоу (.stepan_verify/flows), ошибки с файлом и строкой\n\
             stepan_verify flows fmt [<repo>]    — переписать файлы карты в каноническом виде"
                .into(),
            args.first().is_none_or(|a| a == "help" || a == "--help"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const APP_YML: &str = "app: Outpoll Web
start: Запуск
areas: [Авторизация, Награды]

screens:
  Запуск:
    -> Дашборд: если вошёл
    -> Вход: если не вошёл
  Вход:
    route: /login
    -> Дашборд: войти (Вход по почте)
  Дашборд:
    route: /
    -> Товар: клик по карточке
  Товар:
    route: /catalog/[id]
    kind: page
    -> Корзина  # без действия
  Корзина:
    kind: sheet
";

    const FEATURE: &str = "feature: Вход по почте
area: Авторизация
status: done
summary: Логин по почте и паролю: без соцсетей
aliases: [логин, «та форма слева»]
entries: [/login, app/(auth)/login/page.tsx]
starts_at: Вход

steps:
  - Вход: ввести почту и пароль, нажать «Войти» -> открылся Дашборд  [LoginForm, app/(auth)/login/page.tsx]
  - Товар: открыть /catalog/[id] -> видна цена
  - Дашборд: увидеть приветствие

proofs:
  - test: tests/login.spec.ts::logs_in  covers: 1, 2  at: 2026-11-25T10:00:00Z
  - manual: проверено руками  title: Иван
";

    fn repo(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sv-dsl-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(DIR).join(FEATURES)).unwrap();
        dir
    }

    #[test]
    fn steps_edges_and_brackets() {
        let s = parse_step("Вход: ввести почту, нажать «Войти» -> открылся Дашборд  [LoginForm, app/login/page.tsx]");
        assert_eq!(s.screen, "Вход");
        assert_eq!(s.action, "ввести почту, нажать «Войти»");
        assert_eq!(s.result, "открылся Дашборд");
        assert_eq!(s.entities, vec!["LoginForm"]);
        assert_eq!(s.files, vec!["app/login/page.tsx"]);
        // A route with brackets inside the text is text, not code references.
        let s = parse_step("Товар: открыть /catalog/[id] -> цена");
        assert_eq!(s.action, "открыть /catalog/[id]");
        assert!(s.entities.is_empty() && s.files.is_empty());
        let s = parse_step("Товар: открыть /catalog/[id]");
        assert_eq!(s.action, "открыть /catalog/[id]");
        // Colons inside the action stay in the action.
        let s = parse_step("Форма: время 10:30: выбрать -> ок  [session.rs::send_text]");
        assert_eq!(s.action, "время 10:30: выбрать");
        assert_eq!(s.files, vec!["session.rs::send_text"]);
        let e = parse_edge("Дашборд: войти (Вход по почте)");
        assert_eq!((e.to.as_str(), e.action.as_str(), e.feature.as_str()), ("Дашборд", "войти", "Вход по почте"));
        let e = parse_edge("Корзина");
        assert_eq!((e.to.as_str(), e.action.as_str()), ("Корзина", ""));
        let e = parse_edge("Профиль: открыть меню(справа)");
        assert_eq!(e.feature, "");
    }

    #[test]
    fn compiles_app_and_feature() {
        let r = repo("compile");
        fs::write(r.join(DIR).join(APP), APP_YML).unwrap();
        fs::write(r.join(DIR).join(FEATURES).join("auth.login.yml"), FEATURE).unwrap();
        let (map, diags) = compile(&r).unwrap().unwrap();
        assert!(errors_text(&diags).is_none(), "{diags:?}");
        assert_eq!(map.nav.start, "Запуск");
        assert_eq!(map.nav.screens.len(), 5);
        assert_eq!(map.nav.screens[3].route, "/catalog/[id]");
        assert_eq!(map.nav.screens[4].kind, "sheet");
        assert_eq!(map.nav.screens[1].edges[0].feature, "Вход по почте");
        let f = &map.features[0];
        assert_eq!(f.id, "auth.login");
        assert_eq!(f.summary, "Логин по почте и паролю: без соцсетей");
        assert_eq!(f.entries, vec!["/login", "app/(auth)/login/page.tsx"]);
        assert_eq!(f.paths[0].steps.len(), 3);
        assert_eq!(f.proofs[0].covers, vec!["1", "2"]);
        assert!(f.proofs[0].at.is_some());
        assert_eq!(f.proofs[1].title, "Иван");
        assert_eq!(map.areas[0].title, "Авторизация");
        assert_eq!(f.area, map.areas[0].id);
        let _ = fs::remove_dir_all(r);
    }

    #[test]
    fn errors_carry_file_and_line() {
        let r = repo("errors");
        fs::write(r.join(DIR).join(APP), "app: X\nscreens:\n  Вход:\n    kind: modal\n").unwrap();
        fs::write(r.join(DIR).join(FEATURES).join("a.yml"), "feature: A\nstatus: done\nsteps:\n  - Вход: x\nproofs:\n  - test: t  covers: 3\nэто не поле\n").unwrap();
        let (_, diags) = compile(&r).unwrap().unwrap();
        let text = errors_text(&diags).unwrap();
        assert!(text.contains("app.yml:4:"), "{text}");
        assert!(text.contains("features/a.yml:6:"), "{text}");
        assert!(text.contains("features/a.yml:7:"), "{text}");
        let _ = fs::remove_dir_all(r);
    }

    #[test]
    fn round_trip_and_untouched_files() {
        let r = repo("roundtrip");
        fs::write(r.join(DIR).join(APP), APP_YML).unwrap();
        let path = r.join(DIR).join(FEATURES).join("auth.login.yml");
        fs::write(&path, FEATURE).unwrap();
        let (map, _) = compile(&r).unwrap().unwrap();
        // Same meaning: the hand-written file (comments, layout) stays byte for byte.
        write(&r, &map, false).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), FEATURE);
        // Canonical form compiles back to the same map.
        write(&r, &map, true).unwrap();
        let (again, diags) = compile(&r).unwrap().unwrap();
        assert!(errors_text(&diags).is_none(), "{diags:?}");
        assert_eq!(again.features, map.features);
        assert_eq!(again.nav, map.nav);
        let _ = fs::remove_dir_all(r);
    }
}
