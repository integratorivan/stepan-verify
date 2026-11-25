//! Report runs: every full rebuild and every batch of a long session is kept
//! in its own folder `<out>/runs/<n>/`, so the history of the map is never lost.
//! Also the git side of the input: agents often edit files with scripts, which
//! leaves no tool diffs, so the work tree itself is the source of truth
//! (captured through a shadow index against the session's stable base).

use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::SystemTime,
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::Report;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunKind {
    #[default]
    Full,
    Batch,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Meta {
    pub at: Option<DateTime<Utc>>,
    pub kind: RunKind,
    /// How many session log entries this run covers (the next batch starts here).
    pub upto: usize,
    pub base_run: Option<usize>,
    /// Old runs: the work tree when the run started (`git stash create`, or
    /// HEAD when clean). New runs keep `tree` instead.
    pub git_snapshot: Option<String>,
    /// The session's diff base (a commit sha), resolved on its first run and
    /// carried over: `git.patch` of a full run is everything since it.
    pub base: Option<String>,
    /// The work tree when the run started, as a tree object (shadow index):
    /// the diff base of the next batch.
    pub tree: Option<String>,
    /// Edits in the part of the session this run looked at.
    pub edits: usize,
    pub reviewer: String,
    pub error: Option<String>,
    /// The project the run looked at (the flow map reads proposals per project).
    pub cwd: Option<PathBuf>,
}

pub struct Run {
    pub n: usize,
    pub dir: PathBuf,
    pub meta: Meta,
    /// None: no report.json yet (running, or it never got written).
    pub report: Option<Result<Report, String>>,
}

impl Run {
    pub fn ok(&self) -> Option<&Report> {
        self.report.as_ref().and_then(|r| r.as_ref().ok())
    }
}

pub fn runs_dir(out: &Path) -> PathBuf {
    out.join("runs")
}

/// Before runs existed a report lived flat in `out`: move it into `runs/1`.
pub fn migrate(out: &Path) -> std::io::Result<()> {
    let flat = out.join("report.json");
    let runs = runs_dir(out);
    if !flat.exists() || runs.exists() {
        return Ok(());
    }
    let first = runs.join("1");
    fs::create_dir_all(&first)?;
    let at: Option<DateTime<Utc>> = fs::metadata(&flat).and_then(|m| m.modified()).ok().map(Into::into);
    let reviewer = fs::read_to_string(out.join("reviewer.txt")).map(|s| s.trim().to_owned()).unwrap_or_default();
    for entry in fs::read_dir(out)?.flatten() {
        let name = entry.file_name();
        if name == "runs" || name == "follow" {
            continue;
        }
        fs::rename(entry.path(), first.join(&name))?;
    }
    save_meta(&first, &Meta { at, reviewer, ..Meta::default() })?;
    Ok(())
}

pub fn save_meta(dir: &Path, meta: &Meta) -> std::io::Result<()> {
    fs::write(dir.join("meta.json"), serde_json::to_string_pretty(meta).unwrap_or_default())
}

pub fn load_run(n: usize, dir: PathBuf) -> Run {
    let meta: Meta = fs::read_to_string(dir.join("meta.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    let report = fs::read_to_string(dir.join("report.json"))
        .ok()
        .map(|text| serde_json::from_str::<Report>(&text).map_err(|e| format!("report.json не читается: {e}")));
    Run { n, dir, meta, report }
}

/// All runs, oldest first.
pub fn load_all(out: &Path) -> Vec<Run> {
    let Ok(entries) = fs::read_dir(runs_dir(out)) else { return Vec::new() };
    let mut runs: Vec<Run> = entries
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_str()?.parse::<usize>().ok()?;
            Some(load_run(n, e.path()))
        })
        .collect();
    runs.sort_by_key(|r| r.n);
    runs
}

/// A fresh folder for the next run.
pub fn next_dir(out: &Path) -> std::io::Result<(usize, PathBuf)> {
    let n = load_all(out).last().map_or(1, |r| r.n + 1);
    let dir = runs_dir(out).join(n.to_string());
    fs::create_dir_all(&dir)?;
    Ok((n, dir))
}

// ------------------------------------------------------------------ follow

pub fn follows(out: &Path) -> bool {
    out.join("follow").exists()
}

pub fn set_follow(out: &Path, on: bool) {
    let flag = out.join("follow");
    if on {
        let _ = fs::create_dir_all(out).and_then(|_| fs::write(flag, "1"));
    } else {
        let _ = fs::remove_file(flag);
    }
}

// ------------------------------------------------------------------ git

fn git(cwd: &Path, args: &[&str]) -> Option<String> {
    git_with(cwd, None, args, None).map(|o| String::from_utf8_lossy(&o).into_owned())
}

/// `git` with an optional index file (`GIT_INDEX_FILE`) and stdin.
fn git_with(cwd: &Path, index: Option<&Path>, args: &[&str], input: Option<&[u8]>) -> Option<Vec<u8>> {
    use std::{io::Write as _, process::Stdio};
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(cwd).args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    if let Some(index) = index {
        cmd.env("GIT_INDEX_FILE", index);
    }
    let mut child = cmd.spawn().ok()?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.unwrap_or_default());
    }
    let out = child.wait_with_output().ok()?;
    out.status.success().then_some(out.stdout)
}

/// The object id `rev` names, if it exists.
fn rev(cwd: &Path, rev: &str) -> Option<String> {
    git(cwd, &["rev-parse", "--verify", "-q", rev]).map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
}

fn is_ancestor(cwd: &Path, a: &str, b: &str) -> bool {
    git(cwd, &["merge-base", "--is-ancestor", a, b]).is_some()
}

pub fn is_repo(cwd: &Path) -> bool {
    git(cwd, &["rev-parse", "--is-inside-work-tree"]).is_some_and(|s| s.trim() == "true")
}

/// The work tree as a commit id, without touching it: `git stash create`
/// (empty when clean, then HEAD).
pub fn snapshot(cwd: &Path) -> Option<String> {
    if !is_repo(cwd) {
        return None;
    }
    let stash = git(cwd, &["stash", "create"]).map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
    stash.or_else(|| git(cwd, &["rev-parse", "HEAD"]).map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()))
}

/// Tool state, build output and vendored deps: never part of the change map.
/// (`.github/` and other dot-folders of the project itself still count.)
const NOISE_DIRS: [&str; 7] = [".omc", ".claude", ".git", "node_modules", "target", "dist", ".next"];

pub fn is_noise(path: &str) -> bool {
    let mut parts = path.trim().trim_start_matches("./").split('/').filter(|c| !c.is_empty()).peekable();
    while let Some(c) = parts.next() {
        if parts.peek().is_none() {
            return c == ".DS_Store";
        }
        if NOISE_DIRS.contains(&c) {
            return true;
        }
    }
    false
}

/// Drop the files of a `git diff` text that are tool state or build output.
fn without_noise(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for (i, chunk) in text.split("\ndiff --git ").enumerate() {
        let body = if i == 0 { chunk.strip_prefix("diff --git ").unwrap_or(chunk) } else { chunk };
        let path = body.lines().next().and_then(|h| h.split(" b/").nth(1)).unwrap_or_default();
        if body.trim().is_empty() || (!path.is_empty() && (is_noise(path) || is_secret(path))) {
            continue;
        }
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("diff --git ");
        out.push_str(body);
    }
    if text.ends_with('\n') && !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Tracked changes against `base` (HEAD when none) plus untracked files
/// (only those touched after `since`, when given), capped in size.
pub fn diff(cwd: &Path, base: Option<&str>, since: Option<SystemTime>) -> String {
    const MAX: usize = 400 * 1024;
    if !is_repo(cwd) {
        return String::new();
    }
    let base = base.unwrap_or("HEAD");
    let mut out = without_noise(&git(cwd, &["diff", "--no-color", "--no-ext-diff", "-U3", base]).unwrap_or_default());
    let untracked = git(cwd, &["ls-files", "--others", "--exclude-standard"]).unwrap_or_default();
    for rel in untracked.lines().filter(|l| !l.trim().is_empty() && !is_noise(l)).take(40) {
        if out.len() > MAX {
            break;
        }
        let path = cwd.join(rel);
        let Ok(meta) = fs::metadata(&path) else { continue };
        if meta.len() > 200 * 1024 || since.is_some_and(|t| meta.modified().is_ok_and(|m| m <= t)) {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else { continue }; // binary
        let lines: Vec<&str> = text.lines().take(300).collect();
        out.push_str(&format!("diff --git a/{rel} b/{rel}\nnew file mode 100644\n--- /dev/null\n+++ b/{rel}\n@@ -0,0 +1,{} @@\n", lines.len()));
        for l in lines {
            out.push('+');
            out.push_str(l);
            out.push('\n');
        }
    }
    cap(out)
}

/// A patch for the reviewer is capped in size.
fn cap(mut out: String) -> String {
    const MAX: usize = 400 * 1024;
    if out.len() > MAX {
        let mut cut = MAX;
        while !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
        out.push_str("\n… (обрезано)\n");
    }
    out
}

/// `git diff` text → (path, hunks starting at the first `@@`) per file.
pub fn parse_patch(text: &str) -> Vec<(String, String)> {
    let mut files: Vec<(String, String)> = Vec::new();
    for chunk in text.split("\ndiff --git ").map(|c| c.strip_prefix("diff --git ").unwrap_or(c)) {
        let Some(header) = chunk.lines().next() else { continue };
        let Some(path) = header.split(" b/").nth(1) else { continue };
        if is_noise(path) {
            continue;
        }
        let body = chunk.find("\n@@").map(|i| &chunk[i + 1..]).unwrap_or("");
        if !body.is_empty() {
            files.push((path.trim().to_owned(), body.trim_end().to_owned()));
        }
    }
    files
}

// ------------------------------------------------------------------ shadow capture
//
// The map's git input, after HumanLayer's daemon: a stable base per session,
// and the work tree captured through a throwaway index, so the agent's own
// commits stay in the diff and the user's staging area is never touched.

/// `.env*` holds secrets: never hashed into git objects nor shown to the reviewer.
fn is_secret(path: &str) -> bool {
    path.rsplit('/').next().is_some_and(|name| name.starts_with(".env"))
}

/// The whole tree minus secrets, tool state and build output.
fn pathspec() -> Vec<String> {
    let mut out = vec![".".to_owned(), ":(exclude,glob)**/.env*".to_owned(), ":(exclude,glob)**/.DS_Store".to_owned()];
    out.extend(NOISE_DIRS.iter().filter(|d| **d != ".git").map(|d| format!(":(exclude,glob)**/{d}/**")));
    out
}

/// `origin`'s default branch as known locally (no fetch): `origin/HEAD`, else main / master.
fn upstream_default(cwd: &Path) -> Option<String> {
    let head = git(cwd, &["symbolic-ref", "-q", "refs/remotes/origin/HEAD"]).map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
    head.into_iter()
        .chain(["refs/remotes/origin/main".to_owned(), "refs/remotes/origin/master".to_owned()])
        .find(|r| rev(cwd, &format!("{r}^{{commit}}")).is_some())
}

/// The session's diff base, always a sha. On a branch other than the upstream
/// default: `merge-base HEAD origin/<default>`, so the agent's own commits stay
/// in the diff. On the default branch itself (a merge-base would pull in older
/// unpushed work) or without a remote: HEAD when the session started (`start`:
/// a `git stash create` snapshot or a commit), else HEAD now. No commits yet:
/// the empty tree. Not a repository: None.
pub fn resolve_base(cwd: &Path, start: Option<&str>) -> Option<String> {
    if !is_repo(cwd) {
        return None;
    }
    let Some(head) = rev(cwd, "HEAD^{commit}") else {
        let empty = git_with(cwd, None, &["hash-object", "-t", "tree", "--stdin"], Some(b""))?;
        return Some(String::from_utf8_lossy(&empty).trim().to_owned()).filter(|s| !s.is_empty());
    };
    if let Some(up) = upstream_default(cwd) {
        let branch = git(cwd, &["symbolic-ref", "-q", "HEAD"]).unwrap_or_default();
        let on_default = branch.trim().strip_prefix("refs/heads/").is_some_and(|b| Some(b) == up.strip_prefix("refs/remotes/origin/"));
        if !on_default {
            if let Some(mb) = git(cwd, &["merge-base", "HEAD", &up]).map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()) {
                return Some(mb);
            }
        }
    }
    // A stash snapshot is not in the history: its first parent is the HEAD it was taken on.
    let started = start.and_then(|s| [s.to_owned(), format!("{s}^1")].into_iter().filter_map(|c| rev(cwd, &format!("{c}^{{commit}}"))).find(|c| is_ancestor(cwd, c, &head)));
    Some(started.unwrap_or(head))
}

/// The base still names an object here (not gc'ed, same repository).
pub fn base_ok(cwd: &Path, base: &str) -> bool {
    rev(cwd, &format!("{base}^{{tree}}")).is_some()
}

/// One path that differs between two trees.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Changed {
    /// git's status letter: A, M, D, R, C, T.
    pub status: char,
    pub path: String,
    /// The old path of a rename or copy.
    pub from: Option<String>,
}

/// The work tree captured without touching the user's index.
pub struct Capture {
    /// Repository root: paths are relative to it.
    pub top: PathBuf,
    /// The work tree as a tree object (untracked files that are not ignored included).
    pub tree: String,
    /// What differs from the base to `tree`.
    pub files: Vec<Changed>,
}

/// Untracked files bigger than this stay out (the reviewer reads text diffs;
/// hashing a stray video into the object store helps nobody).
const MAX_UNTRACKED: u64 = 1024 * 1024;

/// Shadow-index capture: a temp index `<scratch>/shadow.index` seeded from the
/// real one, every change of the work tree added to it, written as a tree and
/// compared with `from` (a commit or a tree). Only blobs land in the object
/// store; the user's index and work tree stay as they are.
pub fn capture(cwd: &Path, scratch: &Path, from: &str) -> Option<Capture> {
    if !is_repo(cwd) {
        return None;
    }
    let top = PathBuf::from(git(cwd, &["rev-parse", "--show-toplevel"])?.trim());
    fs::create_dir_all(scratch).ok()?;
    let index = scratch.join("shadow.index");
    let tree = shadow_tree(&top, &index);
    let _ = fs::remove_file(&index);
    let _ = fs::remove_file(scratch.join("shadow.index.lock"));
    let tree = tree?;
    let spec = pathspec();
    let mut args = vec!["diff", "-z", "--name-status", "-M", "--no-ext-diff", from, &tree, "--"];
    args.extend(spec.iter().map(String::as_str));
    let out = git_with(&top, None, &args, None)?;
    Some(Capture { files: parse_name_status(&out), top, tree })
}

fn shadow_tree(top: &Path, index: &Path) -> Option<String> {
    let real = top.join(git(top, &["rev-parse", "--git-path", "index"])?.trim());
    let _ = fs::remove_file(index);
    let has_head = rev(top, "HEAD^{commit}").is_some();
    if fs::copy(&real, index).is_err() && has_head {
        git_with(top, Some(index), &["read-tree", "HEAD"], None)?;
    }
    let spec = pathspec();
    let mut add = vec!["add", "-u", "--"];
    add.extend(spec.iter().map(String::as_str));
    // Nothing tracked yet (no index, no commits): only untracked files to add.
    if index.exists() && git_with(top, Some(index), &add, None).is_none() {
        // A copied index git can't use from here (split index…): start from HEAD.
        let _ = fs::remove_file(index);
        if has_head {
            git_with(top, Some(index), &["read-tree", "HEAD"], None)?;
        }
        git_with(top, Some(index), &add, None)?;
    }
    let mut ls = vec!["ls-files", "-z", "--others", "--exclude-standard", "--"];
    ls.extend(spec.iter().map(String::as_str));
    let listed = git_with(top, Some(index), &ls, None).unwrap_or_default();
    let mut untracked: Vec<u8> = Vec::new();
    for rel in listed.split(|b| *b == 0).filter(|r| !r.is_empty()).take(2000) {
        let small = fs::symlink_metadata(top.join(&*String::from_utf8_lossy(rel))).is_ok_and(|m| m.is_symlink() || (m.is_file() && m.len() <= MAX_UNTRACKED));
        if small {
            untracked.extend_from_slice(rel);
            untracked.push(0);
        }
    }
    if !untracked.is_empty() {
        git_with(top, Some(index), &["--literal-pathspecs", "add", "--pathspec-from-file=-", "--pathspec-file-nul"], Some(&untracked))?;
    }
    let tree = git_with(top, Some(index), &["write-tree"], None)?;
    Some(String::from_utf8_lossy(&tree).trim().to_owned()).filter(|s| !s.is_empty())
}

/// `git diff -z --name-status` output.
fn parse_name_status(out: &[u8]) -> Vec<Changed> {
    let text = String::from_utf8_lossy(out);
    let mut parts = text.split('\0').filter(|s| !s.is_empty());
    let mut files = Vec::new();
    while let Some(status) = parts.next() {
        let letter = status.chars().next().unwrap_or('M');
        let Some(first) = parts.next() else { break };
        let (from, path) = if matches!(letter, 'R' | 'C') { (Some(first.to_owned()), parts.next().unwrap_or(first).to_owned()) } else { (None, first.to_owned()) };
        if !is_noise(&path) && !is_secret(&path) {
            files.push(Changed { status: letter, path, from });
        }
    }
    files
}

/// `git diff <from> <to>` of these files only (empty when there are none), capped.
pub fn tree_diff(top: &Path, from: &str, to: &str, files: &[&Changed]) -> String {
    if files.is_empty() {
        return String::new();
    }
    let mut args = vec!["--literal-pathspecs", "diff", "--no-color", "--no-ext-diff", "-M", "-U3", from, to, "--"];
    for f in files.iter().take(2000) {
        args.push(&f.path);
        if let Some(old) = &f.from {
            args.push(old);
        }
    }
    let text = git_with(top, None, &args, None).map(|o| String::from_utf8_lossy(&o).into_owned()).unwrap_or_default();
    cap(without_noise(&text))
}

/// `abs` relative to the repository root as git prints it (canonical). Paths
/// may differ by a symlink (/var → /private/var) and the file may be gone:
/// canonicalize the nearest parent that exists.
fn rel_to(top: &Path, abs: &Path) -> Option<String> {
    if let Ok(r) = abs.strip_prefix(top) {
        return Some(r.to_string_lossy().into_owned());
    }
    let mut head = abs;
    let mut tail = Vec::new();
    loop {
        if let Ok(real) = fs::canonicalize(head) {
            let full = tail.iter().rev().fold(real, |acc: PathBuf, t| acc.join(t));
            return full.strip_prefix(top).ok().map(|r| r.to_string_lossy().into_owned());
        }
        tail.push(head.file_name()?);
        head = head.parent()?;
    }
}

/// Splits a capture into this session's files and the rest of the folder.
/// The session's: files it edited with tools (`edits`, absolute or relative to
/// `cwd`) and files its commands name (scripts, `git mv`, generators). The
/// rest changed some other way — maybe another agent or the user.
pub fn split_own<'a>(cap: &'a Capture, cwd: &Path, edits: &[&str], commands: &[String]) -> (Vec<&'a Changed>, Vec<&'a Changed>) {
    let own: HashSet<String> = edits
        .iter()
        .filter_map(|p| rel_to(&cap.top, &if Path::new(p).is_absolute() { PathBuf::from(p) } else { cwd.join(p) }))
        .collect();
    let cwd_rel = rel_to(&cap.top, cwd).unwrap_or_default();
    let named = |path: &str| {
        let mut forms = vec![path.to_owned(), cap.top.join(path).display().to_string()];
        if let Some(local) = path.strip_prefix(&format!("{cwd_rel}/")).filter(|_| !cwd_rel.is_empty()) {
            forms.push(local.to_owned());
            forms.push(cwd.join(local).display().to_string());
        }
        commands.iter().any(|c| forms.iter().any(|f| c.contains(f.as_str())))
    };
    cap.files.iter().partition(|f| own.contains(&f.path) || f.from.as_ref().is_some_and(|o| own.contains(o)) || named(&f.path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("stepan_verify-runs-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[core::prelude::v1::test]
    fn flat_report_moves_into_run_one() {
        let out = tmp("migrate");
        fs::write(out.join("report.json"), r#"{"title":"t"}"#).unwrap();
        fs::write(out.join("flow.png"), "png").unwrap();
        fs::write(out.join("reviewer.txt"), "Claude Sonnet\n").unwrap();
        fs::write(out.join("follow"), "1").unwrap();
        migrate(&out).unwrap();
        assert!(!out.join("report.json").exists() && out.join("follow").exists());
        let runs = load_all(&out);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].n, 1);
        assert_eq!(runs[0].ok().unwrap().title, "t");
        assert_eq!(runs[0].meta.reviewer, "Claude Sonnet");
        assert!(runs[0].dir.join("flow.png").exists());
        // Idempotent, and the next run gets its own folder.
        migrate(&out).unwrap();
        assert_eq!(next_dir(&out).unwrap().0, 2);
        let _ = fs::remove_dir_all(out);
    }

    #[core::prelude::v1::test]
    fn patch_splits_per_file() {
        let text = "diff --git a/m.js b/m.js\nindex 1..2\n--- a/m.js\n+++ b/m.js\n@@ -1,2 +1,3 @@\n a\n+b\ndiff --git a/n.js b/n.js\nnew file mode 100644\n--- /dev/null\n+++ b/n.js\n@@ -0,0 +1 @@\n+x\n";
        let files = parse_patch(text);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].0, "m.js");
        assert!(files[0].1.starts_with("@@ -1,2") && files[0].1.contains("+b"));
        assert_eq!(files[1].0, "n.js");
    }

    #[core::prelude::v1::test]
    fn tool_state_and_build_output_are_not_the_change() {
        for noise in [".omc/project-memory.json", "crates/.omc/state/x.json", ".claude/skills/a/SKILL.md", "web/node_modules/x/i.js", "target/debug/stepan_verify", "apps/web/.next/cache/a", "dist/app.js", "src/.DS_Store", ".git/HEAD"] {
            assert!(is_noise(noise), "{noise}");
        }
        for real in ["src/main.rs", ".github/workflows/ci.yml", "docs/target.md", "src/dist.rs", ".gitignore"] {
            assert!(!is_noise(real), "{real}");
        }
        let text = "diff --git a/.omc/m.json b/.omc/m.json\n--- a/.omc/m.json\n+++ b/.omc/m.json\n@@ -1 +1 @@\n-a\n+b\ndiff --git a/src/m.js b/src/m.js\n--- a/src/m.js\n+++ b/src/m.js\n@@ -1 +1,2 @@\n a\n+b\n";
        let clean = without_noise(text);
        assert!(clean.starts_with("diff --git a/src/m.js") && !clean.contains(".omc"), "{clean}");
        assert_eq!(parse_patch(text).iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>(), vec!["src/m.js"]);
    }

    fn sh(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    fn repo(name: &str) -> PathBuf {
        let dir = tmp(name);
        sh(&dir, &["init", "-q", "-b", "main"]);
        fs::write(dir.join("a.txt"), "one\n").unwrap();
        sh(&dir, &["add", "a.txt"]);
        sh(&dir, &["commit", "-q", "-m", "init"]);
        dir
    }

    #[core::prelude::v1::test]
    fn agent_commits_stay_and_other_files_go_aside() {
        let dir = repo("capture");
        let base = resolve_base(&dir, None).unwrap();
        assert_eq!(base, sh(&dir, &["rev-parse", "HEAD"]), "no remote: HEAD now, as a sha");
        // The agent writes and commits a file, then edits another without committing.
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();
        sh(&dir, &["add", "src/main.rs"]);
        sh(&dir, &["commit", "-q", "-m", "agent"]);
        fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
        // A script of the session, another agent's file, a staged change of the user, secrets and tool state.
        fs::create_dir_all(dir.join("gen")).unwrap();
        fs::write(dir.join("gen/out.txt"), "generated\n").unwrap();
        fs::write(dir.join("other.txt"), "not mine\n").unwrap();
        fs::write(dir.join("staged.txt"), "user\n").unwrap();
        sh(&dir, &["add", "staged.txt"]);
        fs::write(dir.join(".env.local"), "TOKEN=x\n").unwrap();
        fs::create_dir_all(dir.join(".omc")).unwrap();
        fs::write(dir.join(".omc/state.json"), "{}\n").unwrap();
        let index_before = sh(&dir, &["diff", "--cached", "--name-only"]);
        let status_before = sh(&dir, &["status", "--porcelain"]);

        let scratch = tmp("capture-scratch");
        let cap = capture(&dir, &scratch, &base).unwrap();
        assert_eq!(sh(&dir, &["diff", "--cached", "--name-only"]), index_before, "the user's index is untouched");
        assert_eq!(sh(&dir, &["status", "--porcelain"]), status_before);
        assert!(!scratch.join("shadow.index").exists());
        let mut paths: Vec<&str> = cap.files.iter().map(|f| f.path.as_str()).collect();
        paths.sort_unstable();
        assert_eq!(paths, vec!["a.txt", "gen/out.txt", "other.txt", "src/main.rs", "staged.txt"]);

        // Tool edits are absolute (through the /var → /private/var symlink on macOS) or relative.
        let edits = [dir.join("src/main.rs").display().to_string(), "a.txt".to_owned()];
        let edits: Vec<&str> = edits.iter().map(String::as_str).collect();
        let commands = vec!["python3 tools/gen.py > gen/out.txt".to_owned()];
        let (own, other) = split_own(&cap, &dir, &edits, &commands);
        let mut own_paths: Vec<&str> = own.iter().map(|f| f.path.as_str()).collect();
        own_paths.sort_unstable();
        assert_eq!(own_paths, vec!["a.txt", "gen/out.txt", "src/main.rs"]);
        assert_eq!(other.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), vec!["other.txt", "staged.txt"]);

        let patch = tree_diff(&cap.top, &base, &cap.tree, &own);
        assert!(patch.contains("+fn main() {}") && patch.contains("+two"), "{patch}");
        assert!(!patch.contains("not mine") && !patch.contains("TOKEN"), "{patch}");
        assert!(tree_diff(&cap.top, &base, &cap.tree, &other).contains("+not mine"));

        // The next batch diffs from this tree: only what changed since.
        fs::write(dir.join("src/main.rs"), "fn main() { run() }\n").unwrap();
        let next = capture(&dir, &scratch, &cap.tree).unwrap();
        assert_eq!(next.files, vec![Changed { status: 'M', path: "src/main.rs".into(), from: None }]);
        let _ = fs::remove_dir_all(dir);
        let _ = fs::remove_dir_all(scratch);
    }

    #[core::prelude::v1::test]
    fn base_is_merge_base_on_a_branch_and_session_start_on_main() {
        let dir = repo("base");
        let root = sh(&dir, &["rev-parse", "HEAD"]);
        sh(&dir, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        // A feature branch: everything since it left origin/main, commits included.
        sh(&dir, &["checkout", "-q", "-b", "feature"]);
        fs::write(dir.join("b.txt"), "b\n").unwrap();
        sh(&dir, &["add", "b.txt"]);
        sh(&dir, &["commit", "-q", "-m", "feature"]);
        assert_eq!(resolve_base(&dir, None).as_deref(), Some(root.as_str()));
        // On the default branch with older unpushed work: HEAD when the session started.
        sh(&dir, &["checkout", "-q", "main"]);
        fs::write(dir.join("a.txt"), "earlier, not this session\n").unwrap();
        sh(&dir, &["commit", "-q", "-am", "unpushed"]);
        let start = sh(&dir, &["rev-parse", "HEAD"]);
        fs::write(dir.join("a.txt"), "dirty at start\n").unwrap();
        let snap = sh(&dir, &["stash", "create"]);
        sh(&dir, &["commit", "-q", "-am", "agent"]);
        assert_eq!(resolve_base(&dir, Some(&snap)).as_deref(), Some(start.as_str()), "a stash snapshot means its parent");
        assert_eq!(resolve_base(&dir, Some(&start)).as_deref(), Some(start.as_str()));
        assert_eq!(resolve_base(&dir, None), Some(sh(&dir, &["rev-parse", "HEAD"])));
        let _ = fs::remove_dir_all(dir);
    }

    #[core::prelude::v1::test]
    fn no_commits_and_no_repo() {
        let dir = tmp("empty-repo");
        sh(&dir, &["init", "-q"]);
        fs::write(dir.join("new.txt"), "x\n").unwrap();
        let base = resolve_base(&dir, None).unwrap();
        assert!(base_ok(&dir, &base));
        let scratch = tmp("empty-scratch");
        let cap = capture(&dir, &scratch, &base).unwrap();
        assert_eq!(cap.files.iter().map(|f| (f.status, f.path.as_str())).collect::<Vec<_>>(), vec![('A', "new.txt")]);
        let plain = tmp("not-a-repo");
        assert!(resolve_base(&plain, None).is_none() && capture(&plain, &scratch, &base).is_none());
        for d in [dir, scratch, plain] {
            let _ = fs::remove_dir_all(d);
        }
    }
}
