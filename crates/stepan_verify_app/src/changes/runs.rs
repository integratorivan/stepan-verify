//! Report runs: every full rebuild and every batch of a long session is kept
//! in its own folder `<out>/runs/<n>/`, so the history of the map is never lost.
//! Also the git side of the input: agents often edit files with scripts, which
//! leaves no tool diffs, so the work tree itself is the source of truth.

use std::{
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
    /// The work tree when the run started (`git stash create`, or HEAD when
    /// clean): the diff base of the next batch.
    pub git_snapshot: Option<String>,
    /// Edits in the part of the session this run looked at.
    pub edits: usize,
    pub reviewer: String,
    pub error: Option<String>,
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
    let out = Command::new("git").arg("-C").arg(cwd).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
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
pub fn is_noise(path: &str) -> bool {
    const DIRS: [&str; 7] = [".omc", ".claude", ".git", "node_modules", "target", "dist", ".next"];
    let mut parts = path.trim().trim_start_matches("./").split('/').filter(|c| !c.is_empty()).peekable();
    while let Some(c) = parts.next() {
        if parts.peek().is_none() {
            return c == ".DS_Store";
        }
        if DIRS.contains(&c) {
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
        if body.trim().is_empty() || (!path.is_empty() && is_noise(path)) {
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
}
