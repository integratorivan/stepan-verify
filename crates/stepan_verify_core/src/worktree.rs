//! Separate checkouts for board tasks and interactive sessions under
//! `~/.stepan_verify/worktrees/<project>/<id>` on `stepan/<id>` branches.
//! Preparation follows `<repo>/.stepan_verify/workspace.yaml`
//! (copy `.env`-like files, clone `node_modules` copy-on-write, run a setup command).
//!
//! Also the one place that knows which project a working directory belongs
//! to: a session in a linked worktree is still a session of its main checkout.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Mutex, OnceLock, atomic::{AtomicU64, Ordering}},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::{Home, Task, TaskStatus};

/// Where an agent works on a task.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Isolation {
    /// Its own `git worktree` on branch `stepan/<task-id>`.
    Worktree,
    /// Right in the project folder.
    Here,
}

/// `<repo>/.stepan_verify/workspace.yaml`; every field is optional.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct WorkspaceConfig {
    /// What a new branch starts from; default: the main checkout's `HEAD`.
    pub source_ref: Option<String>,
    /// Globs of untracked files to copy over (`.env` and the like).
    pub copy: Option<Vec<String>>,
    /// Heavy folders cloned copy-on-write when the lockfiles match.
    pub clone: Option<Vec<String>>,
    /// A shell command run in the new copy (`pnpm install`…).
    pub setup: Option<String>,
}

pub const CONFIG_FILE: &str = ".stepan_verify/workspace.yaml";
const DEFAULT_COPY: [&str; 3] = [".env", ".env.*", ".claude/settings.local.json"];
const DEFAULT_CLONE: [&str; 1] = ["node_modules"];
const LOCKFILES: [&str; 5] = ["package-lock.json", "pnpm-lock.yaml", "yarn.lock", "bun.lock", "bun.lockb"];
/// Never walked when looking for files to copy.
const SKIP_DIRS: [&str; 3] = [".git", "node_modules", "target"];

impl WorkspaceConfig {
    pub fn load(repo: &Path) -> Result<Self> {
        let path = repo.join(CONFIG_FILE);
        match fs::read_to_string(&path) {
            Ok(text) if text.trim().is_empty() => Ok(Self::default()),
            Ok(text) => serde_yaml::from_str(&text).with_context(|| format!("не разобрал {}", path.display())),
            Err(_) => Ok(Self::default()),
        }
    }
}

// ------------------------------------------------------------------ project identity

/// A directory inside a linked worktree, seen from its main checkout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Linked {
    /// The same place in the main checkout.
    pub root: PathBuf,
    /// The worktree's branch, when it is on one.
    pub branch: Option<String>,
}

/// The project folder `cwd` stands for: the same path in the main checkout
/// when `cwd` is in a linked worktree (Stepan's own or any other), else `cwd`.
pub fn project_root(cwd: &Path) -> PathBuf {
    linked(cwd).map_or_else(|| cwd.to_owned(), |l| l.root)
}

/// The project name of a working directory: the folder name of its project root.
pub fn project_name(cwd: &Path) -> Option<String> {
    project_root(cwd).file_name().map(|n| n.to_string_lossy().into_owned())
}

/// `Some` when `cwd` lies in a linked worktree. Cached: the dashboard asks
/// this for every session on every frame.
pub fn linked(cwd: &Path) -> Option<Linked> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Option<Linked>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(hit) = cache.lock().ok().and_then(|c| c.get(cwd).cloned()) {
        return hit;
    }
    let found = find_linked(cwd, &Home::locate().worktrees());
    if let Ok(mut c) = cache.lock() {
        c.insert(cwd.to_owned(), found.clone());
    }
    found
}

/// Reads git's files instead of running git: `.git` in a linked worktree is a
/// file pointing at `<main>/.git/worktrees/<name>`, whose `commondir` leads to
/// the main `.git`. A removed Stepan copy is found through its task.
fn find_linked(cwd: &Path, worktrees: &Path) -> Option<Linked> {
    if cwd.exists() || !cwd.starts_with(worktrees) {
        let top = git_toplevel(cwd)?;
        let (main, branch) = read_linked(&top)?;
        let rel = cwd.strip_prefix(&top).unwrap_or(Path::new(""));
        return Some(Linked { root: join_rel(&main, rel), branch });
    }
    // `<worktrees>/<project>/<task-id>/…` that no longer exists.
    let rest = cwd.strip_prefix(worktrees).ok()?;
    let mut parts = rest.components();
    let (_project, id) = (parts.next()?, parts.next()?.as_os_str().to_string_lossy().into_owned());
    let tasks = worktrees.parent()?.join("tasks");
    let main = Task::load(&tasks.join(format!("{id}.md"))).ok()?.meta.cwd?;
    Some(Linked { root: join_rel(&main, parts.as_path()), branch: Some(branch_name(&id)) })
}

fn join_rel(base: &Path, rel: &Path) -> PathBuf {
    if rel.as_os_str().is_empty() { base.to_owned() } else { base.join(rel) }
}

/// (main checkout, branch) when `top` is the top of a linked worktree.
fn read_linked(top: &Path) -> Option<(PathBuf, Option<String>)> {
    let dotgit = top.join(".git");
    if !dotgit.is_file() {
        return None;
    }
    let text = fs::read_to_string(&dotgit).ok()?;
    let gitdir = top.join(text.trim().strip_prefix("gitdir:")?.trim());
    // Submodules have a `.git` file too, but no `commondir`.
    let common = fs::read_to_string(gitdir.join("commondir")).ok()?;
    let common = gitdir.join(common.trim()).canonicalize().ok()?;
    if common.file_name()? != ".git" {
        return None; // a bare repository has no main checkout
    }
    let branch = fs::read_to_string(gitdir.join("HEAD"))
        .ok()
        .and_then(|h| h.trim().strip_prefix("ref: refs/heads/").map(str::to_owned));
    Some((common.parent()?.to_owned(), branch))
}

/// The nearest folder above `dir` (or `dir`) that holds `.git`.
pub fn git_toplevel(dir: &Path) -> Option<PathBuf> {
    dir.ancestors().find(|d| d.join(".git").exists()).map(Path::to_owned)
}

pub fn branch_name(task_id: &str) -> String {
    format!("stepan/{task_id}")
}

// ------------------------------------------------------------------ isolation

/// Where the agent for `task` should work. Unset means auto: a separate copy
/// when the project is a git repo and someone already works in it — another
/// task of the project is in progress, or a live session runs in its folder.
pub fn resolve(task: &Task, all: &[Task], live_cwds: &[PathBuf]) -> Isolation {
    if let Some(isolation) = task.meta.isolation {
        return isolation;
    }
    // A re-run goes back to the copy it already has.
    if task.meta.worktree.as_ref().is_some_and(|w| w.is_dir()) {
        return Isolation::Worktree;
    }
    let Some(top) = task.meta.cwd.as_deref().and_then(git_toplevel) else { return Isolation::Here };
    let busy = all.iter().any(|t| {
        t.id != task.id
            && (t.meta.status == TaskStatus::InProgress || t.meta.claimed_by.is_some())
            && t.meta.cwd.as_deref().and_then(git_toplevel).is_some_and(|other| other == top)
    });
    let live = live_cwds.iter().any(|c| c.starts_with(&top));
    if busy || live { Isolation::Worktree } else { Isolation::Here }
}

/// Where the agent's session runs: the task's copy (at the same subfolder) if
/// it has one on disk, else the task's folder.
pub fn session_cwd(task: &Task) -> Option<PathBuf> {
    let cwd = task.meta.cwd.clone()?;
    let Some(wt) = task.meta.worktree.as_ref().filter(|w| w.is_dir()) else { return Some(cwd) };
    let top = git_toplevel(&cwd).unwrap_or_else(|| cwd.clone());
    Some(join_rel(wt, cwd.strip_prefix(&top).unwrap_or(Path::new(""))))
}

// ------------------------------------------------------------------ create

/// A prepared copy.
#[derive(Debug)]
pub struct Created {
    pub path: PathBuf,
    pub branch: String,
    /// It was already there (the task runs again).
    pub adopted: bool,
    /// What preparing did, one line each, for the task journal.
    pub notes: Vec<String>,
    /// The setup command failed: what to show on the task.
    pub setup_failed: Option<String>,
}

/// Make (or take back) the copy for task `task_id` of the repo around `cwd`.
/// Blocking: git, file copies and the setup command; run it off the UI thread.
pub fn create(home: &Home, cwd: &Path, task_id: &str) -> Result<Created> {
    let checkout = git_toplevel(cwd).with_context(|| format!("{} — не git-репозиторий", cwd.display()))?;
    let path = copy_path(home, &checkout, task_id);
    create_at(home, &checkout, path, task_id, true)
}

/// Prepare a fresh session's worktree and map the selected folder into it.
/// Blocking, like `create`.
pub fn create_session(home: &Home, cwd: &Path) -> Result<(PathBuf, Option<String>)> {
    static NEXT: AtomicU64 = AtomicU64::new(0);

    let cwd = cwd.canonicalize().with_context(|| format!("не открыть {}", cwd.display()))?;
    let checkout = git_toplevel(&cwd).with_context(|| format!("{} — не git-репозиторий", cwd.display()))?;
    let project = read_linked(&checkout).map_or_else(|| checkout.clone(), |(main, _)| main);
    let time = chrono::Utc::now().timestamp_micros();
    let id = format!("session-{time}-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
    let path = copy_path(home, &project, &id);
    let created = create_at(home, &checkout, path, &id, false)?;
    let root = created.path.canonicalize()?;
    let folder = join_rel(&created.path, cwd.strip_prefix(&checkout)?);
    if !folder.is_dir() || !folder.canonicalize()?.starts_with(root) {
        bail!("рабочая папка {} недоступна внутри отдельной копии", folder.display());
    }
    Ok((folder, created.setup_failed))
}

fn copy_path(home: &Home, project: &Path, id: &str) -> PathBuf {
    let name = project.file_name().map(|name| name.to_string_lossy()).unwrap_or_else(|| "project".into());
    home.worktrees().join(name.as_ref()).join(id)
}

fn create_at(home: &Home, main: &Path, path: PathBuf, task_id: &str, initialize_empty: bool) -> Result<Created> {
    let config = WorkspaceConfig::load(main)?;
    let branch = branch_name(task_id);

    if path.exists() {
        let same = read_linked(&path).is_some_and(|(m, b)| same_dir(&m, main) && b.as_deref() == Some(branch.as_str()));
        if !same {
            bail!("папка {} уже есть, и это не копия этой задачи", path.display());
        }
        return Ok(Created { path, branch, adopted: true, notes: vec!["взял прежнюю копию".into()], setup_failed: None });
    }

    let mut notes = Vec::new();
    if initialize_empty && ensure_head(main)? {
        notes.push("в репозитории не было коммитов — сделал пустой первый".into());
    }
    let source = config.source_ref.clone().unwrap_or_else(|| "HEAD".into());
    let sha = git(main, &["rev-parse", "--verify", &format!("{source}^{{commit}}")]).with_context(|| format!("нет {source}"))?;
    // A copy removed by hand leaves its registration behind.
    let _ = git(main, &["worktree", "prune"]);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let target = path.to_string_lossy().into_owned();
    if git(main, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")]).is_ok() {
        git(main, &["worktree", "add", &target, &branch])?;
        notes.push(format!("ветка {branch} уже была — взял её"));
    } else {
        git(main, &["worktree", "add", "-b", &branch, &target, &sha])?;
        notes.push(format!("ветка {branch} от {source} ({})", &sha[..sha.len().min(8)]));
    }

    let patterns: Vec<String> = config.copy.clone().unwrap_or_else(|| DEFAULT_COPY.map(str::to_owned).to_vec());
    let copied = copy_globs(main, &path, &patterns, &home.worktrees())?;
    if copied > 0 {
        notes.push(format!("скопировал файлов: {copied}"));
    }
    let clone: Vec<String> = config.clone.clone().unwrap_or_else(|| DEFAULT_CLONE.map(str::to_owned).to_vec());
    notes.extend(clone_dirs(main, &path, &clone));

    let mut setup_failed = None;
    if let Some(cmd) = config.setup.as_deref().filter(|c| !c.trim().is_empty()) {
        let log = home.state().join("worktrees").join(task_id).join("setup.log");
        match run_setup(cmd, &path, &log) {
            Ok(None) => notes.push(format!("setup прошёл: {cmd}")),
            Ok(Some(code)) => setup_failed = Some(format!("setup «{cmd}» не прошёл (код {code}) · лог: {}", log.display())),
            Err(err) => setup_failed = Some(format!("setup «{cmd}» не запустился: {err:#}")),
        }
    }
    Ok(Created { path, branch, adopted: false, notes, setup_failed })
}

fn same_dir(a: &Path, b: &Path) -> bool {
    a.canonicalize().ok().zip(b.canonicalize().ok()).is_some_and(|(a, b)| a == b)
}

/// An empty repo (unborn `HEAD`) gets an empty first commit, so a branch has
/// something to start from. Staged files stay staged. True when it made one.
fn ensure_head(main: &Path) -> Result<bool> {
    if git(main, &["rev-parse", "--verify", "--quiet", "HEAD"]).is_ok() {
        return Ok(false);
    }
    let tree = git(main, &["hash-object", "-t", "tree", "/dev/null"])?;
    let message = "Начало (Stepan)";
    let commit = git(main, &["commit-tree", &tree, "-m", message]).or_else(|_| {
        // No user.name / user.email configured.
        git(main, &["-c", "user.name=Stepan", "-c", "user.email=stepan@localhost", "commit-tree", &tree, "-m", message])
    })?;
    git(main, &["update-ref", "HEAD", &commit])?;
    Ok(true)
}

/// Copy files matching `patterns` (gitignored ones too — that is the point:
/// `.env.local` is never tracked) that the copy lacks. A pattern without `/`
/// matches a file name at any depth, one with `/` the path from the root.
fn copy_globs(main: &Path, dest: &Path, patterns: &[String], worktrees: &Path) -> Result<usize> {
    let mut copied = 0;
    let mut stack = vec![main.to_owned()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() {
                let name = entry.file_name();
                if !SKIP_DIRS.iter().any(|s| name == *s) && path != worktrees && path != dest {
                    stack.push(path);
                }
                continue;
            }
            let Ok(rel) = path.strip_prefix(main) else { continue };
            let rel_text = rel.to_string_lossy();
            let file_name = entry.file_name().to_string_lossy().into_owned();
            let hit = patterns.iter().any(|p| {
                let target = if p.contains('/') { rel_text.as_ref() } else { file_name.as_str() };
                glob(p.as_bytes(), target.as_bytes())
            });
            let to = dest.join(rel);
            if hit && path.is_file() && !to.exists() {
                if let Some(parent) = to.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::copy(&path, &to).with_context(|| format!("не скопировал {}", rel.display()))?;
                copied += 1;
            }
        }
    }
    Ok(copied)
}

/// `*` and `?` within one path segment, `**` across segments.
fn glob(p: &[u8], t: &[u8]) -> bool {
    match p {
        [] => t.is_empty(),
        [b'*', b'*', rest @ ..] => match rest.strip_prefix(b"/") {
            Some(rest) => (0..=t.len()).filter(|&i| i == 0 || t[i - 1] == b'/').any(|i| glob(rest, &t[i..])),
            None => (0..=t.len()).any(|i| glob(rest, &t[i..])),
        },
        [b'*', rest @ ..] => (0..=t.len()).take_while(|&i| i == 0 || t[i - 1] != b'/').any(|i| glob(rest, &t[i..])),
        [b'?', rest @ ..] => t.first().is_some_and(|&c| c != b'/') && glob(rest, &t[1..]),
        [c, rest @ ..] => t.first() == Some(c) && glob(rest, &t[1..]),
    }
}

/// Clone heavy folders copy-on-write (APFS `cp -c`: instant, no extra disk),
/// but only where the copy's lockfiles equal the main checkout's — otherwise
/// the folder would not match the code. Returns journal notes.
fn clone_dirs(main: &Path, dest: &Path, dirs: &[String]) -> Vec<String> {
    let mut notes = Vec::new();
    for rel in dirs {
        let (from, to) = (main.join(rel), dest.join(rel));
        if !from.is_dir() || to.exists() {
            continue;
        }
        let parent = Path::new(rel).parent().unwrap_or(Path::new(""));
        if !same_lockfiles(&main.join(parent), &dest.join(parent)) {
            notes.push(format!("{rel} не склонировал: lockfile в копии другой"));
            continue;
        }
        if let Some(p) = to.parent() {
            let _ = fs::create_dir_all(p);
        }
        let ok = Command::new("cp").arg("-cR").arg(&from).arg(&to).stdin(Stdio::null()).output().is_ok_and(|o| o.status.success());
        notes.push(if ok { format!("{rel} склонирован (copy-on-write)") } else { format!("{rel} не склонировал: cp -c не сработал") });
    }
    notes
}

fn same_lockfiles(a: &Path, b: &Path) -> bool {
    LOCKFILES.iter().all(|f| fs::read(a.join(f)).ok() == fs::read(b.join(f)).ok())
}

/// `sh -c <cmd>` in the copy, output to `log`. The app took the login shell's
/// PATH at start (see `main::login_path`), so `pnpm` and friends are found.
/// `Some(code)` when it failed.
fn run_setup(cmd: &str, dir: &Path, log: &Path) -> Result<Option<i32>> {
    if let Some(parent) = log.parent() {
        fs::create_dir_all(parent)?;
    }
    let out = fs::File::create(log)?;
    let status = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(out.try_clone()?)
        .stderr(out)
        .status()?;
    Ok((!status.success()).then(|| status.code().unwrap_or(-1)))
}

// ------------------------------------------------------------------ cleanup

/// What removing a task's copy would lose.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cleanup {
    /// The folder is already gone.
    Gone,
    /// Nothing to lose: no changes, the branch is merged or has no commits of its own.
    Clean,
    /// Clean, but the branch has this many commits not in the main checkout's branch.
    Unmerged(usize),
    /// This many changed or untracked files would be lost.
    Dirty(usize),
}

pub fn cleanup_state(task: &Task) -> Result<Cleanup> {
    let (main, path, branch) = parts(task)?;
    if !path.is_dir() {
        return Ok(Cleanup::Gone);
    }
    let changed = git(&path, &["status", "--porcelain"])?.lines().filter(|l| !l.trim().is_empty()).count();
    if changed > 0 {
        return Ok(Cleanup::Dirty(changed));
    }
    let Some(branch) = branch else { return Ok(Cleanup::Clean) };
    let source = WorkspaceConfig::load(&main).ok().and_then(|c| c.source_ref);
    let merged = ["HEAD".to_owned()]
        .into_iter()
        .chain(source)
        .any(|base| git(&main, &["merge-base", "--is-ancestor", &branch, &base]).is_ok());
    if merged {
        return Ok(Cleanup::Clean);
    }
    let ahead = git(&main, &["rev-list", "--count", &format!("HEAD..{branch}")])?.parse().unwrap_or(0);
    Ok(if ahead == 0 { Cleanup::Clean } else { Cleanup::Unmerged(ahead) })
}

/// Remove the task's copy; `force` drops uncommitted changes, `delete_branch`
/// deletes `stepan/<id>` too. Only ever called after the user clicked.
pub fn remove(task: &Task, force: bool, delete_branch: bool) -> Result<()> {
    let (main, path, branch) = parts(task)?;
    if path.is_dir() {
        let target = path.to_string_lossy().into_owned();
        let mut args = vec!["worktree", "remove"];
        if force {
            args.push("--force");
        }
        args.push(&target);
        git(&main, &args)?;
    } else {
        let _ = git(&main, &["worktree", "prune"]);
    }
    if let (true, Some(branch)) = (delete_branch, branch) {
        git(&main, &["branch", "-D", &branch])?;
    }
    // The project folder under worktrees/ goes once its last copy does.
    if let Some(parent) = path.parent() {
        let _ = fs::remove_dir(parent);
    }
    Ok(())
}

fn parts(task: &Task) -> Result<(PathBuf, PathBuf, Option<String>)> {
    let path = task.meta.worktree.clone().context("у задачи нет отдельной копии")?;
    let main = task.meta.cwd.as_deref().and_then(git_toplevel).context("у задачи нет папки проекта")?;
    Ok((main, path, task.meta.branch.clone()))
}

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .context("git не запустился")?;
    if !out.status.success() {
        bail!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Board, NewTask};

    struct Fixture {
        root: PathBuf,
        home: Home,
        repo: PathBuf,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn fixture(name: &str, commit: bool) -> Fixture {
        let root = std::env::temp_dir().join(format!("stepan_verify-wt-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let home = Home { root: root.join("home") };
        home.ensure().unwrap();
        let repo = root.join("proj");
        fs::create_dir_all(&repo).unwrap();
        let repo = repo.canonicalize().unwrap();
        sh(&repo, &["init", "-q", "-b", "main"]);
        sh(&repo, &["config", "user.email", "t@t"]);
        sh(&repo, &["config", "user.name", "t"]);
        if commit {
            fs::write(repo.join("README.md"), "hi\n").unwrap();
            fs::write(repo.join(".gitignore"), ".env*\nnode_modules/\n").unwrap();
            sh(&repo, &["add", "."]);
            sh(&repo, &["commit", "-q", "-m", "init"]);
        }
        Fixture { root, home, repo }
    }

    fn sh(dir: &Path, args: &[&str]) -> String {
        git(dir, args).unwrap_or_else(|e| panic!("{e:#}"))
    }

    fn task(f: &Fixture, title: &str) -> Task {
        Board::new(f.home.clone()).create(NewTask { title: title.into(), cwd: Some(f.repo.clone()), status: TaskStatus::Todo, ..Default::default() }, "me").unwrap()
    }

    fn with_copy(mut t: Task, c: &Created) -> Task {
        t.meta.worktree = Some(c.path.clone());
        t.meta.branch = Some(c.branch.clone());
        t
    }

    #[test]
    fn glob_patterns() {
        assert!(glob(b".env", b".env"));
        assert!(glob(b".env.*", b".env.local"));
        assert!(!glob(b".env.*", b".env"));
        assert!(glob(b".claude/settings.local.json", b".claude/settings.local.json"));
        assert!(!glob(b"*.json", b"a/b.json"));
        assert!(glob(b"**/x.json", b"a/b/x.json"));
        assert!(glob(b"**/x.json", b"x.json"));
        assert!(!glob(b"**/x.json", b"ax.json"));
    }

    #[test]
    fn session_worktrees_start_from_the_selected_checkout_and_keep_its_subdirectory() {
        let f = fixture("session", true);
        let linked = f.root.join("another-checkout");
        sh(&f.repo, &["worktree", "add", "-b", "feature", linked.to_str().unwrap()]);
        let folder = linked.join("apps/web");
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("feature.txt"), "feature checkout").unwrap();
        sh(&linked, &["add", "."]);
        sh(&linked, &["commit", "-m", "feature"]);
        let head = sh(&linked, &["rev-parse", "HEAD"]);

        let (first, warning) = create_session(&f.home, &folder).unwrap();
        let (second, _) = create_session(&f.home, &folder).unwrap();
        assert!(warning.is_none());
        assert!(first.starts_with(f.home.worktrees().join("proj")));
        assert!(first.ends_with("apps/web"));
        assert_ne!(first, second);
        assert_ne!(sh(&first, &["branch", "--show-current"]), sh(&second, &["branch", "--show-current"]));
        assert_eq!(sh(&first, &["rev-parse", "HEAD"]), head);
        assert_eq!(sh(&second, &["rev-parse", "HEAD"]), head);

        fs::write(first.join("feature.txt"), "only the first worktree").unwrap();
        assert_eq!(fs::read_to_string(folder.join("feature.txt")).unwrap(), "feature checkout");
        assert_eq!(fs::read_to_string(second.join("feature.txt")).unwrap(), "feature checkout");
    }

    #[test]
    fn session_worktree_does_not_commit_in_an_empty_repository() {
        let f = fixture("empty-session", false);
        assert!(create_session(&f.home, &f.repo).is_err());
        assert!(git(&f.repo, &["rev-parse", "--verify", "HEAD"]).is_err());
    }

    #[test]
    fn create_copies_ignored_files_and_adopts_on_rerun() {
        let f = fixture("create", true);
        fs::write(f.repo.join(".env"), "A=1").unwrap();
        fs::write(f.repo.join(".env.local"), "B=2").unwrap();
        fs::create_dir_all(f.repo.join("apps/web")).unwrap();
        fs::write(f.repo.join("apps/web/.env.production"), "C=3").unwrap();
        fs::create_dir_all(f.repo.join(".claude")).unwrap();
        fs::write(f.repo.join(".claude/settings.local.json"), "{}").unwrap();
        fs::create_dir_all(f.repo.join("node_modules/pkg")).unwrap();
        fs::write(f.repo.join("node_modules/pkg/.env"), "skip me").unwrap();
        let t = task(&f, "a");

        let c = create(&f.home, &f.repo, &t.id).unwrap();
        assert!(!c.adopted);
        assert_eq!(c.path, f.home.worktrees().join("proj").join(&t.id));
        assert_eq!(c.branch, format!("stepan/{}", t.id));
        assert_eq!(sh(&c.path, &["rev-parse", "--abbrev-ref", "HEAD"]), c.branch);
        assert_eq!(fs::read_to_string(c.path.join(".env.local")).unwrap(), "B=2");
        assert!(c.path.join(".env").is_file() && c.path.join("apps/web/.env.production").is_file());
        assert!(c.path.join(".claude/settings.local.json").is_file());
        // No lockfiles on either side → node_modules is cloned whole, not walked for .env.
        assert!(c.path.join("node_modules/pkg/.env").is_file());
        assert!(c.notes.iter().any(|n| n.contains("node_modules склонирован")), "{:?}", c.notes);

        // Running the task again takes the same copy.
        let again = create(&f.home, &f.repo, &t.id).unwrap();
        assert!(again.adopted);
        assert_eq!(again.path, c.path);

        // Removed by hand, the branch is still there: re-created on it.
        fs::remove_dir_all(&c.path).unwrap();
        let back = create(&f.home, &f.repo, &t.id).unwrap();
        assert!(!back.adopted && back.notes.iter().any(|n| n.contains("уже была")), "{:?}", back.notes);

        // A foreign folder at the copy's place is not taken.
        let other = task(&f, "b");
        fs::create_dir_all(f.home.worktrees().join("proj").join(&other.id)).unwrap();
        assert!(create(&f.home, &f.repo, &other.id).is_err());
    }

    #[test]
    fn clone_only_with_equal_lockfiles() {
        let f = fixture("clone", true);
        fs::write(f.repo.join("pnpm-lock.yaml"), "v1").unwrap();
        sh(&f.repo, &["add", "pnpm-lock.yaml"]);
        sh(&f.repo, &["commit", "-q", "-m", "lock"]);
        fs::create_dir_all(f.repo.join("node_modules/a")).unwrap();
        fs::write(f.repo.join("node_modules/a/index.js"), "x").unwrap();
        let t = task(&f, "a");
        let c = create(&f.home, &f.repo, &t.id).unwrap();
        assert!(c.path.join("node_modules/a/index.js").is_file());

        // The main checkout's lockfile moved on (uncommitted): the copy's differs.
        fs::write(f.repo.join("pnpm-lock.yaml"), "v2").unwrap();
        let t2 = task(&f, "b");
        let c2 = create(&f.home, &f.repo, &t2.id).unwrap();
        assert!(!c2.path.join("node_modules").exists());
        assert!(c2.notes.iter().any(|n| n.contains("lockfile")), "{:?}", c2.notes);
    }

    #[test]
    fn empty_repo_gets_a_first_commit() {
        let f = fixture("empty", false);
        fs::write(f.repo.join("staged.txt"), "x").unwrap();
        sh(&f.repo, &["add", "staged.txt"]);
        let t = task(&f, "a");
        let c = create(&f.home, &f.repo, &t.id).unwrap();
        assert!(c.path.is_dir());
        assert_eq!(sh(&f.repo, &["rev-list", "--count", "HEAD"]), "1");
        // What was staged stays staged, not swallowed by the empty commit.
        assert_eq!(sh(&f.repo, &["diff", "--cached", "--name-only"]), "staged.txt");
    }

    #[test]
    fn setup_failure_is_reported_not_fatal() {
        let f = fixture("setup", true);
        fs::create_dir_all(f.repo.join(".stepan_verify")).unwrap();
        fs::write(f.repo.join(CONFIG_FILE), "setup: \"echo hello; exit 3\"\ncopy: []\n").unwrap();
        let t = task(&f, "a");
        let c = create(&f.home, &f.repo, &t.id).unwrap();
        assert!(c.setup_failed.as_deref().is_some_and(|s| s.contains("код 3")), "{:?}", c.setup_failed);
        let log = f.home.state().join("worktrees").join(&t.id).join("setup.log");
        assert_eq!(fs::read_to_string(log).unwrap().trim(), "hello");
    }

    #[test]
    fn project_root_maps_linked_worktrees() {
        let f = fixture("root", true);
        let t = task(&f, "a");
        let c = create(&f.home, &f.repo, &t.id).unwrap();
        fs::create_dir_all(c.path.join("src")).unwrap();
        let wts = f.home.worktrees();
        let l = find_linked(&c.path.join("src"), &wts).unwrap();
        assert_eq!(l.root, f.repo.join("src"));
        assert_eq!(l.branch.as_deref(), Some(c.branch.as_str()));
        assert_eq!(find_linked(&c.path, &wts).unwrap().root, f.repo);
        // The main checkout and plain folders are themselves.
        assert!(find_linked(&f.repo, &wts).is_none());
        assert!(find_linked(&f.root, &wts).is_none());
        // A removed copy is still found through its task.
        remove(&with_copy(t.clone(), &c), true, false).unwrap();
        assert_eq!(find_linked(&c.path, &wts).unwrap().root, f.repo);
    }

    #[test]
    fn auto_isolation() {
        let f = fixture("auto", true);
        let a = task(&f, "a");
        let b = task(&f, "b");
        let board = Board::new(f.home.clone());
        assert_eq!(resolve(&a, &board.tasks(), &[]), Isolation::Here);
        // A session already works in the project.
        assert_eq!(resolve(&a, &board.tasks(), &[f.repo.join("src")]), Isolation::Worktree);
        assert_eq!(resolve(&a, &board.tasks(), &[f.root.join("elsewhere")]), Isolation::Here);
        // Another task of the project is taken.
        board.claim(&b.id, "claude", std::time::Duration::from_secs(60)).unwrap();
        assert_eq!(resolve(&a, &board.tasks(), &[]), Isolation::Worktree);
        // An explicit choice wins.
        let mut here = a.clone();
        here.meta.isolation = Some(Isolation::Here);
        assert_eq!(resolve(&here, &board.tasks(), &[]), Isolation::Here);
        // Not a git repo: nothing to isolate.
        let mut loose = a.clone();
        loose.meta.cwd = Some(f.root.clone());
        assert_eq!(resolve(&loose, &board.tasks(), std::slice::from_ref(&f.root)), Isolation::Here);
    }

    #[test]
    fn cleanup_states() {
        let f = fixture("cleanup", true);
        let t = task(&f, "a");
        let c = create(&f.home, &f.repo, &t.id).unwrap();
        let t = with_copy(t, &c);
        // Ignored copies (.env, node_modules) do not make it dirty.
        fs::write(c.path.join(".env"), "x").unwrap();
        assert_eq!(cleanup_state(&t).unwrap(), Cleanup::Clean);

        fs::write(c.path.join("new.txt"), "x").unwrap();
        assert_eq!(cleanup_state(&t).unwrap(), Cleanup::Dirty(1));

        sh(&c.path, &["add", "new.txt"]);
        sh(&c.path, &["commit", "-q", "-m", "work"]);
        assert_eq!(cleanup_state(&t).unwrap(), Cleanup::Unmerged(1));

        sh(&f.repo, &["merge", "-q", "--ff-only", &c.branch]);
        assert_eq!(cleanup_state(&t).unwrap(), Cleanup::Clean);

        remove(&t, false, true).unwrap();
        assert!(!c.path.exists());
        assert!(git(&f.repo, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{}", c.branch)]).is_err());
        assert_eq!(cleanup_state(&t).unwrap(), Cleanup::Gone);
    }

    #[test]
    fn unmerged_branch_is_kept_on_remove() {
        let f = fixture("keep", true);
        let t = task(&f, "a");
        let c = create(&f.home, &f.repo, &t.id).unwrap();
        let t = with_copy(t, &c);
        fs::write(c.path.join("w.txt"), "x").unwrap();
        sh(&c.path, &["add", "."]);
        sh(&c.path, &["commit", "-q", "-m", "w"]);
        remove(&t, false, false).unwrap();
        assert!(!c.path.exists());
        assert!(git(&f.repo, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{}", c.branch)]).is_ok());
    }
}
