//! The structure of a change, computed rather than guessed: which code files
//! (modules) changed, the imports between them at a git base and in the work
//! tree, and what the manifests and lockfiles say about dependencies.
//!
//! Imports are read from the files themselves: Rust (`use`, `crate::`/`super::`
//! paths, workspace crates, Cargo dependencies) and JS/TS (`import`/`export …
//! from`, `require`, dynamic `import()`, relative paths and tsconfig `paths`).
//! Other languages simply have no edges.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Added,
    Removed,
    Changed,
    #[default]
    Same,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Module {
    /// Path from the repository root, or `pkg:<name>` for an external package.
    pub id: String,
    /// Crate or npm package the file belongs to (or the package itself).
    pub package: String,
    /// Short name: `changes::media`, `WelcomeBonusDialog`, `rusqlite`.
    pub label: String,
    pub status: Status,
    pub external: bool,
    pub plus: usize,
    pub minus: usize,
}

/// `from` imports `to`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(default)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub status: Status,
}

/// A dependency line that differs between the base and the work tree.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DepChange {
    /// `crates/core/Cargo.toml`, `apps/web/package.json`.
    pub manifest: String,
    /// `dependencies`, `dev-dependencies`, `devDependencies`, …
    pub section: String,
    pub name: String,
    pub before: Option<String>,
    pub after: Option<String>,
}

/// Packages that appeared in or left a lockfile (transitive dependencies included).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LockChange {
    pub file: String,
    pub added: Vec<String>,
    pub removed: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Arch {
    /// The commit «Было» is read from.
    pub base: String,
    pub modules: Vec<Module>,
    pub edges: Vec<Edge>,
    pub deps: Vec<DepChange>,
    pub locks: Vec<LockChange>,
    /// Changed code files the graph does not cover (too many, unknown language).
    pub skipped: usize,
}

impl Arch {
    pub fn module(&self, id: &str) -> Option<&Module> {
        self.modules.iter().find(|m| m.id == id)
    }
}

/// Changed files past this are left out of the graph (counted in `skipped`).
const MAX_SCOPE: usize = 80;
/// Files read to find who imports the changed ones.
const MAX_SCAN: usize = 6000;
const MAX_FILE: u64 = 400 * 1024;
/// Neighbours (unchanged importers / imports) kept around the changed files.
const MAX_NEIGHBOURS: usize = 28;
/// Of them, ones whose links did not change: context, not news.
const MAX_QUIET: usize = 8;

const JS_EXT: [&str; 9] = ["ts", "tsx", "js", "jsx", "mjs", "cjs", "astro", "vue", "svelte"];

fn ext(path: &str) -> &str {
    path.rsplit_once('.').map_or("", |(_, e)| e)
}

fn is_rust(path: &str) -> bool {
    ext(path) == "rs"
}

fn is_js(path: &str) -> bool {
    JS_EXT.contains(&ext(path)) && !path.ends_with(".d.ts")
}

pub fn is_code(path: &str) -> bool {
    is_rust(path) || is_js(path)
}

// ------------------------------------------------------------------ sides

/// One state of the repository: the base commit or the work tree.
trait Side {
    fn has(&self, path: &str) -> bool;
    fn read(&mut self, path: &str) -> Option<String>;
}

struct Work<'a> {
    root: &'a Path,
    files: &'a HashSet<String>,
}

impl Side for Work<'_> {
    fn has(&self, path: &str) -> bool {
        self.files.contains(path)
    }
    fn read(&mut self, path: &str) -> Option<String> {
        let p = self.root.join(path);
        if std::fs::metadata(&p).ok()?.len() > MAX_FILE {
            return None;
        }
        std::fs::read_to_string(p).ok()
    }
}

/// `git cat-file --batch` on the base commit: one process for every read.
struct Base<'a> {
    rev: String,
    files: &'a HashSet<String>,
    proc: Option<(Child, ChildStdin, BufReader<ChildStdout>)>,
}

impl<'a> Base<'a> {
    fn new(root: &Path, rev: &str, files: &'a HashSet<String>) -> Self {
        let proc = Command::new("git")
            .args(["cat-file", "--batch"])
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()
            .and_then(|mut c| {
                let stdin = c.stdin.take()?;
                let stdout = BufReader::new(c.stdout.take()?);
                Some((c, stdin, stdout))
            });
        Base { rev: rev.to_owned(), files, proc }
    }
}

impl Drop for Base<'_> {
    fn drop(&mut self) {
        if let Some((mut c, stdin, _)) = self.proc.take() {
            drop(stdin);
            let _ = c.wait();
        }
    }
}

impl Side for Base<'_> {
    fn has(&self, path: &str) -> bool {
        self.files.contains(path)
    }
    fn read(&mut self, path: &str) -> Option<String> {
        if !self.files.contains(path) {
            return None;
        }
        let (_, stdin, out) = self.proc.as_mut()?;
        writeln!(stdin, "{}:{}", self.rev, path).ok()?;
        stdin.flush().ok()?;
        let mut header = String::new();
        out.read_line(&mut header).ok()?;
        // "<sha> blob <size>" or "<spec> missing"
        let size: usize = header.split_whitespace().nth(2)?.parse().ok()?;
        let mut buf = vec![0u8; size + 1]; // content + trailing newline
        out.read_exact(&mut buf).ok()?;
        buf.pop();
        if size as u64 > MAX_FILE {
            return None;
        }
        String::from_utf8(buf).ok()
    }
}

// ------------------------------------------------------------------ git

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).current_dir(root).stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn lines(s: Option<String>) -> Vec<String> {
    s.unwrap_or_default().lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_owned).collect()
}

// ------------------------------------------------------------------ project knowledge

/// A Cargo package: its source folder and the dependency names it may import.
#[derive(Clone, Debug, Default)]
struct Crate {
    name: String,
    dir: String,
    deps: HashSet<String>,
}

/// tsconfig/jsconfig `paths` of a folder: (folder, base_url, [(pattern, targets)]).
type Alias = (String, String, Vec<(String, Vec<String>)>);

/// What resolving imports needs to know about the repository.
#[derive(Default)]
struct Project {
    crates: Vec<Crate>,
    /// npm package of a folder: (folder, name), longest folder first.
    packages: Vec<(String, String)>,
    aliases: Vec<Alias>,
}

fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

fn join(dir: &str, rel: &str) -> String {
    let mut parts: Vec<&str> = if dir.is_empty() { Vec::new() } else { dir.split('/').collect() };
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

fn under(path: &str, dir: &str) -> bool {
    dir.is_empty() || path == dir || path.starts_with(&format!("{dir}/"))
}

/// `// …` and `/* … */` out of JSON-with-comments, and trailing commas.
fn jsonc(text: &str) -> String {
    let b: Vec<char> = text.chars().collect();
    let mut out: Vec<char> = Vec::with_capacity(b.len());
    let (mut i, mut in_str) = (0, false);
    while i < b.len() {
        let c = b[i];
        if in_str {
            out.push(c);
            if c == '\\' && i + 1 < b.len() {
                out.push(b[i + 1]);
                i += 2;
                continue;
            }
            if c == '"' {
                in_str = false;
            }
            i += 1;
        } else if c == '"' {
            in_str = true;
            out.push(c);
            i += 1;
        } else if c == '/' && b.get(i + 1) == Some(&'/') {
            while i < b.len() && b[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && b.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < b.len() && !(b[i] == '*' && b[i + 1] == '/') {
                i += 1;
            }
            i += 2;
        } else {
            // A comma right before a closing bracket.
            if c == ',' && matches!(b[i + 1..].iter().find(|c| !c.is_whitespace()), Some('}') | Some(']')) {
                i += 1;
                continue;
            }
            out.push(c);
            i += 1;
        }
    }
    out.into_iter().collect()
}

fn cargo_deps(v: &toml::Value) -> HashSet<String> {
    let mut out = HashSet::new();
    for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(t) = v.get(section).and_then(|t| t.as_table()) {
            // `package = "real-name"` renames a dependency; the code uses the key.
            out.extend(t.keys().map(|k| k.replace('-', "_")));
        }
    }
    out
}

impl Project {
    fn load(work: &mut Work, files: &HashSet<String>) -> Project {
        let mut p = Project::default();
        let mut manifests: Vec<&String> = files.iter().filter(|f| f.ends_with("Cargo.toml") || f.ends_with("package.json") || f.ends_with("tsconfig.json") || f.ends_with("jsconfig.json")).collect();
        manifests.sort();
        for m in manifests.into_iter().take(400) {
            let dir = parent(m).to_owned();
            let Some(text) = work.read(m) else { continue };
            if m.ends_with("Cargo.toml") {
                let Ok(v) = toml::from_str::<toml::Value>(&text) else { continue };
                if let Some(name) = v.get("package").and_then(|p| p.get("name")).and_then(|n| n.as_str()) {
                    p.crates.push(Crate { name: name.replace('-', "_"), dir, deps: cargo_deps(&v) });
                }
            } else if m.ends_with("package.json") {
                if m.contains("node_modules/") {
                    continue;
                }
                let name = serde_json::from_str::<serde_json::Value>(&text).ok().and_then(|v| v.get("name")?.as_str().map(str::to_owned));
                p.packages.push((dir.clone(), name.unwrap_or_else(|| dir.rsplit('/').next().unwrap_or("").to_owned())));
            } else if let Ok(v) = serde_json::from_str::<serde_json::Value>(&jsonc(&text)) {
                let opts = v.get("compilerOptions");
                let base = opts.and_then(|o| o.get("baseUrl")).and_then(|b| b.as_str()).map(|b| join(&dir, b)).unwrap_or_else(|| dir.clone());
                let paths: Vec<(String, Vec<String>)> = opts
                    .and_then(|o| o.get("paths"))
                    .and_then(|p| p.as_object())
                    .map(|o| o.iter().map(|(k, v)| (k.clone(), v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_owned)).collect()).unwrap_or_default())).collect())
                    .unwrap_or_default();
                p.aliases.push((dir, base, paths));
            }
        }
        p.crates.sort_by_key(|c| std::cmp::Reverse(c.dir.len()));
        p.packages.sort_by_key(|(d, _)| std::cmp::Reverse(d.len()));
        p.aliases.sort_by_key(|(d, _, _)| std::cmp::Reverse(d.len()));
        p
    }

    fn crate_of(&self, path: &str) -> Option<&Crate> {
        self.crates.iter().find(|c| if c.dir.is_empty() { path.starts_with("src/") } else { under(path, &c.dir) })
    }

    fn crate_named(&self, name: &str) -> Option<&Crate> {
        self.crates.iter().find(|c| c.name == name)
    }

    fn package_of(&self, path: &str) -> String {
        if let Some(c) = self.crate_of(path) {
            return c.name.clone();
        }
        if let Some((_, n)) = self.packages.iter().find(|(d, _)| under(path, d)) {
            return n.clone();
        }
        path.split('/').next().filter(|_| path.contains('/')).unwrap_or(".").to_owned()
    }
}

// ------------------------------------------------------------------ Rust

fn src_dir(c: &Crate) -> String {
    if c.dir.is_empty() { "src".to_owned() } else { format!("{}/src", c.dir) }
}

/// Module path of a file inside its crate: `src/a/b.rs` → [a, b]; lib.rs/main.rs → [].
fn rust_module(c: &Crate, path: &str) -> Vec<String> {
    let src = src_dir(c);
    let rel = path.strip_prefix(&format!("{src}/")).unwrap_or(path).trim_end_matches(".rs");
    let mut segs: Vec<String> = rel.split('/').map(str::to_owned).collect();
    if segs.last().is_some_and(|s| s == "mod") {
        segs.pop();
    }
    if segs.len() == 1 && (segs[0] == "lib" || segs[0] == "main") {
        segs.clear();
    }
    // src/bin/x.rs and examples are their own roots; good enough to keep the path.
    segs
}

/// The file of the longest existing module prefix of `segs` in crate `c`, and
/// how many segments matched (0: the crate root).
fn rust_file(c: &Crate, segs: &[String], side: &dyn Side) -> Option<(String, usize)> {
    let src = src_dir(c);
    for k in (1..=segs.len()).rev() {
        let p = segs[..k].join("/");
        for cand in [format!("{src}/{p}.rs"), format!("{src}/{p}/mod.rs")] {
            if side.has(&cand) {
                return Some((cand, k));
            }
        }
    }
    [format!("{src}/lib.rs"), format!("{src}/main.rs")].into_iter().find(|f| side.has(f)).map(|f| (f, 0))
}

/// Remove `//` and `/* */` comments and string literals' contents (keeps quotes).
fn strip_rust(text: &str) -> String {
    let b: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == '/' && b.get(i + 1) == Some(&'/') {
            while i < b.len() && b[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && b.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < b.len() && !(b[i] == '*' && b[i + 1] == '/') {
                i += 1;
            }
            i += 2;
        } else if c == '"' {
            out.push('"');
            i += 1;
            while i < b.len() && b[i] != '"' {
                if b[i] == '\\' {
                    i += 1;
                }
                i += 1;
            }
            out.push('"');
            i += 1;
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

/// `a::{b, c::{d, e}}` → [a::b, a::c::d, a::c::e]; `x as y` keeps `x`.
fn expand_use(tree: &str) -> Vec<String> {
    // Collapse whitespace, but keep the one around `as`.
    let words: Vec<&str> = tree.split_whitespace().collect();
    let mut t = String::new();
    for (i, w) in words.iter().enumerate() {
        if *w == "as" {
            t.push_str(" as ");
        } else {
            if i > 0 && words[i - 1] != "as" && !t.is_empty() {
                let last = t.chars().last().unwrap_or(' ');
                let first = w.chars().next().unwrap_or(' ');
                if ident(last) && ident(first) {
                    t.push(' ');
                }
            }
            t.push_str(w);
        }
    }
    fn go(prefix: &str, t: &str, out: &mut Vec<String>) {
        let t = t.trim();
        if let Some(open) = t.find('{') {
            let head = &t[..open];
            let close = t.rfind('}').unwrap_or(t.len());
            let inner = &t[open + 1..close.max(open + 1)];
            let pre = format!("{prefix}{head}");
            let (mut depth, mut start) = (0, 0);
            for (i, ch) in inner.char_indices() {
                match ch {
                    '{' => depth += 1,
                    '}' => depth -= 1,
                    ',' if depth == 0 => {
                        go(&pre, &inner[start..i], out);
                        start = i + 1;
                    }
                    _ => {}
                }
            }
            go(&pre, &inner[start..], out);
        } else if !t.is_empty() {
            let leaf = t.split(" as ").next().unwrap_or(t).trim();
            out.push(format!("{prefix}{leaf}").trim_end_matches("::self").to_owned());
        }
    }
    let mut out = Vec::new();
    go("", &t, &mut out);
    out
}

fn ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Every `a::b::c` path in the code: `use` trees expanded, inline paths too.
fn rust_paths(text: &str) -> Vec<Vec<String>> {
    let code = strip_rust(text);
    let mut out = Vec::new();
    // `use …;` trees (may span lines).
    let mut rest = code.as_str();
    while let Some(pos) = rest.find("use ") {
        let before_ok = pos == 0 || !ident(rest[..pos].chars().last().unwrap_or(' '));
        let after = &rest[pos + 4..];
        if before_ok {
            if let Some(end) = after.find(';') {
                for p in expand_use(&after[..end]) {
                    out.push(p.split("::").map(str::to_owned).filter(|s| !s.is_empty() && s != "*").collect());
                }
            }
        }
        rest = after;
    }
    // Inline paths: `crate::x::y`, `super::x`, `other_crate::x`.
    let chars: Vec<char> = code.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if ident(chars[i]) && (i == 0 || !(ident(chars[i - 1]) || chars[i - 1] == ':')) {
            let mut segs = Vec::new();
            let mut j = i;
            loop {
                let s = j;
                while j < chars.len() && ident(chars[j]) {
                    j += 1;
                }
                segs.push(chars[s..j].iter().collect::<String>());
                if j + 2 < chars.len() && chars[j] == ':' && chars[j + 1] == ':' && ident(chars[j + 2]) {
                    j += 2;
                } else {
                    break;
                }
            }
            if segs.len() > 1 {
                out.push(segs);
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    out
}

enum Target {
    File(String),
    Package(String),
}

fn rust_imports(p: &Project, path: &str, text: &str, side: &dyn Side) -> Vec<Target> {
    let Some(c) = p.crate_of(path) else { return Vec::new() };
    let here = rust_module(c, path);
    let mut out = Vec::new();
    for segs in rust_paths(text) {
        let Some(first) = segs.first() else { continue };
        let (krate, rel): (&Crate, Vec<String>) = match first.as_str() {
            "crate" => (c, segs[1..].to_vec()),
            "self" => (c, here.iter().chain(&segs[1..]).cloned().collect()),
            "super" => {
                let ups = segs.iter().take_while(|s| *s == "super").count();
                let base = &here[..here.len().saturating_sub(ups)];
                (c, base.iter().chain(&segs[ups..]).cloned().collect())
            }
            "std" | "core" | "alloc" | "Self" => continue,
            name if c.deps.contains(name) => match p.crate_named(name).filter(|o| o.name != c.name) {
                // Another crate of the workspace: down to its module.
                Some(other) => (other, segs[1..].to_vec()),
                None => {
                    out.push(Target::Package(name.to_owned()));
                    continue;
                }
            },
            // `mod x;` + `x::y` from the parent module.
            _ => {
                let child: Vec<String> = here.iter().chain(&segs).cloned().collect();
                if let Some((f, k)) = rust_file(c, &child, side) {
                    if k > here.len() && f != path {
                        out.push(Target::File(f));
                    }
                }
                continue;
            }
        };
        match rust_file(krate, &rel, side) {
            // The crate root of this crate is mostly re-exports: not a real link.
            Some((_, 0)) if krate.name == c.name => {}
            Some((f, _)) if f != path => out.push(Target::File(f)),
            _ => {}
        }
    }
    out
}

// ------------------------------------------------------------------ JS / TS

/// Module specifiers: `from 'x'`, `import 'x'`, `import('x')`, `require('x')`.
fn js_specifiers(text: &str) -> Vec<String> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let quoted = |mut i: usize| -> Option<String> {
        while i < b.len() && (b[i] as char).is_whitespace() {
            i += 1;
        }
        if i < b.len() && b[i] == b'(' {
            i += 1;
            while i < b.len() && (b[i] as char).is_whitespace() {
                i += 1;
            }
        }
        let q = *b.get(i)?;
        if q != b'\'' && q != b'"' {
            return None;
        }
        let end = text[i + 1..].find(q as char)? + i + 1;
        let s = &text[i + 1..end];
        (!s.is_empty() && !s.contains('\n')).then(|| s.to_owned())
    };
    for kw in ["from", "import", "require"] {
        let mut start = 0;
        while let Some(pos) = text[start..].find(kw) {
            let at = start + pos;
            let before_ok = at == 0 || !(ident(b[at - 1] as char) || b[at - 1] == b'.' || b[at - 1] == b'$');
            let after = at + kw.len();
            let after_ok = after >= b.len() || !(ident(b[after] as char) || b[after] == b'$');
            if before_ok && after_ok {
                if let Some(s) = quoted(after) {
                    out.push(s);
                }
            }
            start = after;
        }
    }
    out
}

fn js_resolve(side: &dyn Side, p: &str) -> Option<String> {
    let p = p.trim_end_matches('/');
    if side.has(p) && is_js(p) {
        return Some(p.to_owned());
    }
    // ESM TypeScript writes `./x.js` for `./x.ts`.
    let stem = p.strip_suffix(".js").or_else(|| p.strip_suffix(".mjs")).or_else(|| p.strip_suffix(".jsx")).unwrap_or(p);
    for e in ["ts", "tsx", "js", "jsx", "mjs", "cjs", "astro", "vue", "svelte"] {
        let c = format!("{stem}.{e}");
        if side.has(&c) {
            return Some(c);
        }
    }
    for e in ["ts", "tsx", "js", "jsx", "mjs"] {
        let c = format!("{p}/index.{e}");
        if side.has(&c) {
            return Some(c);
        }
    }
    None
}

fn package_name(spec: &str) -> String {
    let mut parts = spec.split('/');
    let first = parts.next().unwrap_or(spec);
    if first.starts_with('@') {
        format!("{first}/{}", parts.next().unwrap_or(""))
    } else {
        first.to_owned()
    }
}

const NODE_BUILTINS: [&str; 22] = [
    "fs", "path", "os", "url", "util", "crypto", "child_process", "http", "https", "stream", "events", "buffer", "zlib", "net", "tls", "assert", "readline", "process", "module", "worker_threads", "timers", "querystring",
];

fn js_imports(p: &Project, path: &str, text: &str, side: &dyn Side) -> Vec<Target> {
    let dir = parent(path);
    let mut out = Vec::new();
    for spec in js_specifiers(text) {
        if spec.starts_with('.') {
            if let Some(f) = js_resolve(side, &join(dir, &spec)) {
                out.push(Target::File(f));
            }
            continue;
        }
        if spec.starts_with("node:") || spec.starts_with("http") || NODE_BUILTINS.contains(&spec.as_str()) {
            continue;
        }
        // tsconfig `paths` of the nearest config above the file.
        let mut aliased = None;
        if let Some((_, base, paths)) = p.aliases.iter().find(|(d, _, _)| under(path, d)) {
            for (pat, targets) in paths {
                let hit = match pat.split_once('*') {
                    Some((pre, post)) => spec.strip_prefix(pre).and_then(|r| r.strip_suffix(post)).map(str::to_owned),
                    None => (spec == *pat).then(String::new),
                };
                if let Some(star) = hit {
                    for t in targets {
                        if let Some(f) = js_resolve(side, &join(base, &t.replace('*', &star))) {
                            aliased = Some(f);
                            break;
                        }
                    }
                }
                if aliased.is_some() {
                    break;
                }
            }
        }
        match aliased {
            Some(f) => out.push(Target::File(f)),
            None if spec.starts_with('~') || spec.starts_with("@/") || spec.starts_with('#') => {}
            None => out.push(Target::Package(package_name(&spec))),
        }
    }
    out
}

fn imports(p: &Project, path: &str, text: &str, side: &dyn Side) -> Vec<Target> {
    if is_rust(path) {
        rust_imports(p, path, text, side)
    } else if is_js(path) {
        js_imports(p, path, text, side)
    } else {
        Vec::new()
    }
}

// ------------------------------------------------------------------ manifests

fn cargo_dep_table(text: &str) -> BTreeMap<(String, String), String> {
    let mut out = BTreeMap::new();
    let Ok(v) = toml::from_str::<toml::Value>(text) else { return out };
    let mut sections: Vec<(String, &toml::value::Table)> = Vec::new();
    for s in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(t) = v.get(s).and_then(|t| t.as_table()) {
            sections.push((s.to_owned(), t));
        }
    }
    if let Some(t) = v.get("workspace").and_then(|w| w.get("dependencies")).and_then(|t| t.as_table()) {
        sections.push(("workspace.dependencies".to_owned(), t));
    }
    for (s, t) in sections {
        for (k, d) in t {
            let ver = match d {
                toml::Value::String(v) => v.clone(),
                toml::Value::Table(t) => {
                    if t.get("workspace").and_then(|w| w.as_bool()) == Some(true) {
                        "workspace".to_owned()
                    } else if let Some(p) = t.get("path").and_then(|p| p.as_str()) {
                        format!("path {p}")
                    } else {
                        let mut v = t.get("version").and_then(|v| v.as_str()).unwrap_or("*").to_owned();
                        if let Some(f) = t.get("features").and_then(|f| f.as_array()) {
                            let f: Vec<&str> = f.iter().filter_map(|x| x.as_str()).collect();
                            if !f.is_empty() {
                                v.push_str(&format!(" [{}]", f.join(", ")));
                            }
                        }
                        v
                    }
                }
                other => other.to_string(),
            };
            out.insert((s.clone(), k.clone()), ver);
        }
    }
    out
}

fn npm_dep_table(text: &str) -> BTreeMap<(String, String), String> {
    let mut out = BTreeMap::new();
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else { return out };
    for s in ["dependencies", "devDependencies", "peerDependencies", "optionalDependencies"] {
        if let Some(o) = v.get(s).and_then(|o| o.as_object()) {
            for (k, d) in o {
                out.insert((s.to_owned(), k.clone()), d.as_str().unwrap_or("").to_owned());
            }
        }
    }
    out
}

fn dep_changes(manifest: &str, before: Option<&str>, after: Option<&str>) -> Vec<DepChange> {
    let table = |t: Option<&str>| t.map(|t| if manifest.ends_with("Cargo.toml") { cargo_dep_table(t) } else { npm_dep_table(t) }).unwrap_or_default();
    let (b, a) = (table(before), table(after));
    let keys: BTreeSet<&(String, String)> = b.keys().chain(a.keys()).collect();
    keys.into_iter()
        .filter(|k| b.get(*k) != a.get(*k))
        .map(|(s, n)| DepChange { manifest: manifest.to_owned(), section: s.clone(), name: n.clone(), before: b.get(&(s.clone(), n.clone())).cloned(), after: a.get(&(s.clone(), n.clone())).cloned() })
        .collect()
}

/// `name version` of every package in a Cargo.lock or package-lock.json.
fn lock_packages(file: &str, text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    if file.ends_with("Cargo.lock") {
        let mut name = None;
        for l in text.lines() {
            if let Some(n) = l.strip_prefix("name = ") {
                name = Some(n.trim_matches('"').to_owned());
            } else if let (Some(v), Some(n)) = (l.strip_prefix("version = "), name.take()) {
                out.insert(format!("{n} {}", v.trim_matches('"')));
            }
        }
    } else if let Ok(v) = serde_json::from_str::<serde_json::Value>(text) {
        if let Some(pkgs) = v.get("packages").and_then(|p| p.as_object()) {
            for (k, p) in pkgs {
                if let Some(name) = k.rsplit("node_modules/").next().filter(|n| !n.is_empty() && k.contains("node_modules/")) {
                    out.insert(format!("{name} {}", p.get("version").and_then(|v| v.as_str()).unwrap_or("")));
                }
            }
        }
    }
    out
}

// ------------------------------------------------------------------ compute

fn label(p: &Project, path: &str) -> String {
    if let Some(c) = p.crate_of(path).filter(|_| is_rust(path)) {
        if !under(path, &src_dir(c)) {
            // examples/, tests/, benches/: their own roots.
            let rel = path.strip_prefix(&format!("{}/", c.dir)).unwrap_or(path);
            return rel.trim_end_matches(".rs").to_owned();
        }
        let segs = rust_module(c, path);
        return if segs.is_empty() { c.name.clone() } else { segs.join("::") };
    }
    let file = path.rsplit('/').next().unwrap_or(path);
    let stem = file.split('.').next().unwrap_or(file);
    if stem == "index" || stem == "+page" || stem == "page" {
        let dir = parent(path).rsplit('/').next().unwrap_or(stem);
        return format!("{dir}/{stem}");
    }
    stem.to_owned()
}

/// `client` / `client` → `auth/client` / `futures/client`: add parent folders until the labels differ.
fn disambiguate(modules: &mut [Module]) {
    for depth in 1..4 {
        let mut count: HashMap<String, usize> = HashMap::new();
        for m in modules.iter() {
            *count.entry(m.label.clone()).or_default() += 1;
        }
        let mut any = false;
        for m in modules.iter_mut().filter(|m| !m.external && count[&m.label] > 1) {
            let dirs: Vec<&str> = parent(&m.id).rsplit('/').take(depth).collect();
            let stem = m.label.rsplit('/').next().unwrap_or(&m.label).to_owned();
            m.label = dirs.into_iter().rev().chain([stem.as_str()]).collect::<Vec<_>>().join("/");
            any = true;
        }
        if !any {
            break;
        }
    }
}

/// The structure of the change between `base` and the work tree of the repository at `cwd`.
/// `noise` filters tool and build folders out.
pub fn compute(cwd: &Path, base: &str, noise: &dyn Fn(&str) -> bool) -> Option<Arch> {
    let root = PathBuf::from(git(cwd, &["rev-parse", "--show-toplevel"])?.trim());
    let base_rev = git(&root, &["rev-parse", "--verify", &format!("{base}^{{commit}}")])?.trim().to_owned();
    let base_files: HashSet<String> = lines(git(&root, &["ls-tree", "-r", "--name-only", &base_rev])).into_iter().collect();
    let work_files: HashSet<String> = lines(git(&root, &["ls-files", "--cached", "--others", "--exclude-standard"])).into_iter().filter(|f| !noise(f) && root.join(f).is_file()).collect();

    // What changed, with +/- per file.
    let mut changed: BTreeSet<String> = BTreeSet::new();
    for l in lines(git(&root, &["diff", "--name-status", "--no-renames", &base_rev])) {
        if let Some(path) = l.split('\t').nth(1) {
            changed.insert(path.to_owned());
        }
    }
    let untracked = lines(git(&root, &["ls-files", "--others", "--exclude-standard"]));
    changed.extend(untracked.iter().cloned());
    changed.retain(|f| !noise(f));
    let mut stat: HashMap<String, (usize, usize)> = HashMap::new();
    for l in lines(git(&root, &["diff", "--numstat", "--no-renames", &base_rev])) {
        let mut it = l.split('\t');
        let (a, d, f) = (it.next(), it.next(), it.next());
        if let (Some(a), Some(d), Some(f)) = (a, d, f) {
            stat.insert(f.to_owned(), (a.parse().unwrap_or(0), d.parse().unwrap_or(0)));
        }
    }
    for f in &untracked {
        if !stat.contains_key(f) {
            let n = std::fs::read_to_string(root.join(f)).map(|t| t.lines().count()).unwrap_or(0);
            stat.insert(f.clone(), (n, 0));
        }
    }

    let mut work = Work { root: &root, files: &work_files };
    let project = Project::load(&mut work, &work_files);
    let mut basei = Base::new(&root, &base_rev, &base_files);

    // Dependencies: manifests and lockfiles that changed.
    let mut deps = Vec::new();
    let mut locks = Vec::new();
    for f in &changed {
        let name = f.rsplit('/').next().unwrap_or(f);
        if name == "Cargo.toml" || name == "package.json" {
            let (b, a) = (basei.read(f), work.read(f));
            deps.extend(dep_changes(f, b.as_deref(), a.as_deref()));
        } else if name == "Cargo.lock" || name == "package-lock.json" {
            let own = |p: &String| project.crates.iter().any(|c| p.split(' ').next().is_some_and(|n| n.replace('-', "_") == c.name));
            let b: BTreeSet<String> = basei.read(f).map(|t| lock_packages(f, &t)).unwrap_or_default().into_iter().filter(|p| !own(p)).collect();
            let a: BTreeSet<String> = work.read(f).map(|t| lock_packages(f, &t)).unwrap_or_default().into_iter().filter(|p| !own(p)).collect();
            let added: Vec<String> = a.difference(&b).cloned().collect();
            let removed: Vec<String> = b.difference(&a).cloned().collect();
            // A version bump shows as one removed and one added: keep both, the UI pairs them.
            if !added.is_empty() || !removed.is_empty() {
                locks.push(LockChange { file: f.clone(), added, removed });
            }
        }
    }

    let code_changed: Vec<String> = changed.iter().filter(|f| is_code(f)).cloned().collect();
    let skipped = code_changed.len().saturating_sub(MAX_SCOPE);
    let scope: BTreeSet<String> = code_changed.into_iter().take(MAX_SCOPE).collect();

    // Edges at each side. Changed files are read at both; unchanged files only
    // in the work tree (their imports are the same at the base).
    let mut before: BTreeSet<(String, String)> = BTreeSet::new();
    let mut after: BTreeSet<(String, String)> = BTreeSet::new();
    let key = |t: Target| match t {
        Target::File(f) => f,
        Target::Package(n) => format!("pkg:{n}"),
    };
    for f in &scope {
        if let Some(text) = basei.read(f) {
            for t in imports(&project, f, &text, &basei) {
                before.insert((f.clone(), key(t)));
            }
        }
        if let Some(text) = work.read(f) {
            for t in imports(&project, f, &text, &work) {
                after.insert((f.clone(), key(t)));
            }
        }
    }
    // Who imports the changed files: scan the rest of the code (same packages first).
    let packages: HashSet<String> = scope.iter().map(|f| project.package_of(f)).collect();
    let mut others: Vec<&String> = work_files.iter().filter(|f| is_code(f) && !scope.contains(*f)).collect();
    others.sort_by_key(|f| (!packages.contains(&project.package_of(f)), f.len()));
    for f in others.into_iter().take(MAX_SCAN) {
        let Some(text) = work.read(f) else { continue };
        for t in imports(&project, f, &text, &work) {
            let to = key(t);
            if scope.contains(&to) {
                after.insert((f.clone(), to.clone()));
                if base_files.contains(&to) && base_files.contains(f.as_str()) {
                    before.insert((f.clone(), to));
                }
            }
        }
    }

    let mut edges: Vec<Edge> = before
        .union(&after)
        .map(|(a, b)| Edge {
            from: a.clone(),
            to: b.clone(),
            status: match (before.contains(&(a.clone(), b.clone())), after.contains(&(a.clone(), b.clone()))) {
                (true, true) => Status::Same,
                (false, true) => Status::Added,
                _ => Status::Removed,
            },
        })
        .collect();
    // Packages only when the manifest changed: a changed file importing react
    // or anyhow that the project had for years is not news.
    let dep_names: HashSet<String> = deps.iter().map(|d| d.name.replace('-', "_")).chain(deps.iter().map(|d| d.name.clone())).collect();
    edges.retain(|e| !e.to.starts_with("pkg:") || dep_names.contains(&e.to[4..]));

    // Modules: the changed files, then neighbours. A neighbour joined by a new or
    // removed link always stays; ones only linked as before fill a few places.
    let mut weight: HashMap<String, (bool, usize)> = HashMap::new();
    for e in &edges {
        for end in [&e.from, &e.to] {
            if !scope.contains(end) {
                let w = weight.entry(end.clone()).or_default();
                w.0 |= e.status != Status::Same;
                w.1 += 1;
            }
        }
    }
    let mut neighbours: Vec<(String, (bool, usize))> = weight.into_iter().collect();
    neighbours.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut kept: HashSet<String> = scope.iter().cloned().collect();
    let mut quiet = 0;
    for (id, (news, _)) in neighbours {
        if news && kept.len() < scope.len() + MAX_NEIGHBOURS {
            kept.insert(id);
        } else if !news && quiet < MAX_QUIET {
            quiet += 1;
            kept.insert(id);
        }
    }
    edges.retain(|e| kept.contains(&e.from) && kept.contains(&e.to) && e.from != e.to);
    edges.sort();

    let mut modules: Vec<Module> = kept
        .iter()
        .filter(|id| scope.contains(*id) || edges.iter().any(|e| &e.from == *id || &e.to == *id))
        .map(|id| {
            if let Some(name) = id.strip_prefix("pkg:") {
                // New to the project only when nothing used it before.
                let links: Vec<Status> = edges.iter().filter(|e| &e.to == id).map(|e| e.status).collect();
                let status = if !links.is_empty() && links.iter().all(|s| *s == Status::Added) && !before.iter().any(|(_, t)| t == id) {
                    Status::Added
                } else if !links.is_empty() && links.iter().all(|s| *s == Status::Removed) && !after.iter().any(|(_, t)| t == id) {
                    Status::Removed
                } else if links.iter().any(|s| *s != Status::Same) {
                    Status::Changed
                } else {
                    Status::Same
                };
                return Module { id: id.clone(), package: name.to_owned(), label: name.to_owned(), status, external: true, plus: 0, minus: 0 };
            }
            let (plus, minus) = stat.get(id).copied().unwrap_or_default();
            let status = match (base_files.contains(id), work_files.contains(id)) {
                (false, true) => Status::Added,
                (true, false) => Status::Removed,
                _ if changed.contains(id) => Status::Changed,
                _ => Status::Same,
            };
            Module { id: id.clone(), package: project.package_of(id), label: label(&project, id), status, external: false, plus, minus }
        })
        .collect();
    modules.sort_by(|a, b| a.external.cmp(&b.external).then(a.package.cmp(&b.package)).then(a.id.cmp(&b.id)));
    disambiguate(&mut modules);

    Some(Arch { base: base_rev, modules, edges, deps, locks, skipped })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Files(HashSet<String>);
    impl Side for Files {
        fn has(&self, p: &str) -> bool {
            self.0.contains(p)
        }
        fn read(&mut self, _: &str) -> Option<String> {
            None
        }
    }
    fn files(list: &[&str]) -> Files {
        Files(list.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn expands_use_trees() {
        let mut v = expand_use("crate::{store::{FileRow, Store}, home::Home as H, self}");
        v.sort();
        assert_eq!(v, ["crate", "crate::home::Home", "crate::store::FileRow", "crate::store::Store"]);
    }

    #[test]
    fn resolves_rust_paths_inside_a_workspace() {
        let app = Crate { name: "app".into(), dir: "crates/app".into(), deps: ["core_lib", "serde"].map(String::from).into() };
        let core = Crate { name: "core_lib".into(), dir: "crates/core".into(), deps: HashSet::new() };
        let p = Project { crates: vec![app, core], ..Default::default() };
        let side = files(&["crates/app/src/main.rs", "crates/app/src/workspace.rs", "crates/app/src/changes/mod.rs", "crates/app/src/changes/media.rs", "crates/core/src/lib.rs", "crates/core/src/store.rs"]);
        let text = "use core_lib::{store::Store, Home};\nuse serde::Serialize;\n// use crate::nope;\nfn f() { crate::workspace::save(); media::render(); let s = \"crate::fake\"; }";
        let got: Vec<String> = rust_imports(&p, "crates/app/src/changes/mod.rs", text, &side)
            .into_iter()
            .map(|t| match t {
                Target::File(f) => f,
                Target::Package(n) => format!("pkg:{n}"),
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        assert_eq!(got, ["crates/app/src/changes/media.rs", "crates/app/src/workspace.rs", "crates/core/src/lib.rs", "crates/core/src/store.rs", "pkg:serde"]);
    }

    #[test]
    fn resolves_js_specifiers_and_aliases() {
        let text = "import a from './a'\nimport { b } from \"../lib/b.js\";\nimport type {T} from '@/types'\nexport * from './c';\nconst x = await import('./lazy');\nconst y = require('react-dom/client');\nimport fs from 'node:fs'\nimport '@scope/pkg/x.css'";
        let mut specs = js_specifiers(text);
        specs.sort();
        assert_eq!(specs, ["../lib/b.js", "./a", "./c", "./lazy", "@/types", "@scope/pkg/x.css", "node:fs", "react-dom/client"]);
        let p = Project { aliases: vec![("apps/web".into(), "apps/web".into(), vec![("@/*".into(), vec!["./src/*".into()])])], ..Default::default() };
        let side = files(&["apps/web/src/ui/a.tsx", "apps/web/src/lib/b.ts", "apps/web/src/types/index.ts", "apps/web/src/ui/c.ts", "apps/web/src/ui/lazy/index.tsx"]);
        let got: BTreeSet<String> = js_imports(&p, "apps/web/src/ui/x.tsx", text, &side)
            .into_iter()
            .map(|t| match t {
                Target::File(f) => f,
                Target::Package(n) => format!("pkg:{n}"),
            })
            .collect();
        let want: BTreeSet<String> = ["apps/web/src/ui/a.tsx", "apps/web/src/lib/b.ts", "apps/web/src/types/index.ts", "apps/web/src/ui/c.ts", "apps/web/src/ui/lazy/index.tsx", "pkg:react-dom", "pkg:@scope/pkg"].map(String::from).into();
        assert_eq!(got, want);
    }

    #[test]
    fn diffs_manifests_and_lockfiles() {
        let before = "[package]\nname = \"x\"\n[dependencies]\nserde = \"1\"\nold = \"0.1\"\n";
        let after = "[package]\nname = \"x\"\n[dependencies]\nserde = \"1\"\nrusqlite = { version = \"0.43\", features = [\"bundled\"] }\n";
        let d = dep_changes("Cargo.toml", Some(before), Some(after));
        assert_eq!(d.len(), 2);
        assert_eq!((d[0].name.as_str(), d[0].before.as_deref(), d[0].after.as_deref()), ("old", Some("0.1"), None));
        assert_eq!((d[1].name.as_str(), d[1].after.as_deref()), ("rusqlite", Some("0.43 [bundled]")));
        let lock = "[[package]]\nname = \"a\"\nversion = \"1.0.0\"\n\n[[package]]\nname = \"b\"\nversion = \"2.1.0\"\n";
        assert_eq!(lock_packages("Cargo.lock", lock).into_iter().collect::<Vec<_>>(), ["a 1.0.0", "b 2.1.0"]);
        let npm = r#"{"dependencies": {"react": "^19"}, "devDependencies": {"vite": "6"}}"#;
        let d = dep_changes("package.json", None, Some(npm));
        assert_eq!(d.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["react", "vite"]);
    }

    #[test]
    fn strips_jsonc() {
        let v: serde_json::Value = serde_json::from_str(&jsonc("{\n // c\n \"a\": \"x//y\", /* b */ \"p\": [1,],\n}")).unwrap();
        assert_eq!(v["a"], "x//y");
    }
}
