//! Tasks: `~/.iv3/tasks/<id>.md`, YAML frontmatter + markdown body.
//!
//! The board model follows Vercel Board: a task is claimed by one agent with a
//! lease, the agent logs progress and completes it (→ review), dependent tasks
//! become ready once their dependencies are done, and an expired lease puts
//! the task back in the queue. Every change is appended to the event journal.

use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    Home,
    events::{self, Event},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// Captured, not yet decided.
    #[default]
    Inbox,
    /// Ready to be picked up (once dependencies are done).
    Todo,
    InProgress,
    /// The agent needs a human answer.
    Blocked,
    /// Agent finished; a human checks the result.
    Review,
    Done,
}

impl TaskStatus {
    pub fn label(self) -> &'static str {
        match self {
            TaskStatus::Inbox => "Входящие",
            TaskStatus::Todo => "К работе",
            TaskStatus::InProgress => "В работе",
            TaskStatus::Blocked => "Нужен ответ",
            TaskStatus::Review => "Ревью",
            TaskStatus::Done => "Готово",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "inbox" => Self::Inbox,
            "todo" => Self::Todo,
            "in_progress" => Self::InProgress,
            "blocked" => Self::Blocked,
            "review" => Self::Review,
            "done" => Self::Done,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TaskMeta {
    pub title: String,
    #[serde(default)]
    pub status: TaskStatus,
    /// Display name of the project (defaults to the cwd folder name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// Where an agent works on this task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    /// `key=value` labels, e.g. `kind=bug`, `area=admin`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    /// Task ids that must be done before this one is ready.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    /// Attached files (screenshots, logs…), copied to `tasks/<id>.files/`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<PathBuf>,
    /// Who holds the task: "claude", "codex", a person…
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claimed_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_until: Option<DateTime<Utc>>,
    /// Agent sessions that worked on this task, as "claude:<id>" / "codex:<id>".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sessions: Vec<String>,
    /// Last result reported on complete / block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<DateTime<Utc>>,
}

/// A task file: `~/.iv3/tasks/<id>.md` with YAML frontmatter.
#[derive(Clone, Debug)]
pub struct Task {
    pub id: String,
    pub path: PathBuf,
    pub meta: TaskMeta,
    pub body: String,
}

impl Task {
    pub fn load(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)?;
        let (meta, body) = split_frontmatter(&text);
        let meta: TaskMeta = match meta {
            Some(m) => serde_yaml::from_str(m).with_context(|| format!("frontmatter in {}", path.display()))?,
            None => TaskMeta { title: first_heading(body).unwrap_or_default(), ..Default::default() },
        };
        Ok(Self {
            id: path.file_stem().unwrap_or_default().to_string_lossy().into_owned(),
            path: path.to_owned(),
            meta,
            body: body.to_owned(),
        })
    }

    pub fn save(&self) -> Result<()> {
        let yaml = serde_yaml::to_string(&self.meta)?;
        // Write-then-rename so readers never see a half-written file.
        let tmp = self.path.with_extension("md.tmp");
        fs::write(&tmp, format!("---\n{yaml}---\n{}", self.body))?;
        fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    pub fn load_all(dir: &Path) -> Vec<Task> {
        let mut tasks: Vec<Task> = fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "md"))
            .filter_map(|p| Task::load(&p).ok())
            .collect();
        tasks.sort_by(|a, b| a.id.cmp(&b.id));
        tasks
    }

    pub fn project_name(&self) -> Option<String> {
        self.meta.project.clone().or_else(|| {
            self.meta.cwd.as_ref().and_then(|c| c.file_name()).map(|n| n.to_string_lossy().into_owned())
        })
    }

    pub fn lease_expired(&self, now: DateTime<Utc>) -> bool {
        self.meta.lease_until.is_some_and(|t| t < now)
    }

    /// Dependencies that are not done yet.
    pub fn open_dependencies<'a>(&'a self, all: &'a [Task]) -> Vec<&'a str> {
        self.meta
            .depends_on
            .iter()
            .filter(|dep| all.iter().find(|t| &t.id == *dep).is_none_or(|t| t.meta.status != TaskStatus::Done))
            .map(String::as_str)
            .collect()
    }

    /// Can an agent pick this up right now?
    pub fn is_ready(&self, all: &[Task]) -> bool {
        self.meta.status == TaskStatus::Todo && self.meta.claimed_by.is_none() && self.open_dependencies(all).is_empty()
    }
}

/// Fields for a new task.
#[derive(Clone, Debug, Default)]
pub struct NewTask {
    pub title: String,
    pub body: String,
    pub cwd: Option<PathBuf>,
    pub labels: Vec<String>,
    pub depends_on: Vec<String>,
    pub status: TaskStatus,
    /// Files to attach; `create` copies them next to the task.
    pub attachments: Vec<PathBuf>,
}

/// Board operations. Each one takes the board lock, re-reads the task from
/// disk, changes it and appends an event, so concurrent agents cannot both
/// claim the same task.
pub struct Board {
    home: Home,
}

impl Board {
    pub fn new(home: Home) -> Self {
        Self { home }
    }

    pub fn tasks(&self) -> Vec<Task> {
        Task::load_all(&self.home.tasks())
    }

    pub fn get(&self, id: &str) -> Result<Task> {
        let path = self.home.tasks().join(format!("{id}.md"));
        Task::load(&path).with_context(|| format!("нет задачи {id}"))
    }

    pub fn create(&self, new: NewTask, actor: &str) -> Result<Task> {
        let _lock = self.lock()?;
        let now = Utc::now();
        let id = self.fresh_id(now);
        let attachments = self.store_attachments(&id, &new.attachments)?;
        let task = Task {
            path: self.home.tasks().join(format!("{id}.md")),
            id,
            meta: TaskMeta {
                title: new.title,
                status: new.status,
                cwd: new.cwd,
                labels: new.labels,
                depends_on: new.depends_on,
                attachments,
                created_at: Some(now),
                updated_at: Some(now),
                ..Default::default()
            },
            body: if new.body.is_empty() { String::new() } else { format!("{}\n", new.body.trim_end()) },
        };
        task.save()?;
        self.journal(&task.id, actor, "create", &task.meta.title);
        Ok(task)
    }

    /// Take a task for `lease`. Fails if someone else holds a live lease.
    pub fn claim(&self, id: &str, by: &str, lease: Duration) -> Result<Task> {
        self.update(id, by, "claim", "", |task, now| {
            if let Some(holder) = &task.meta.claimed_by {
                if holder != by && !task.lease_expired(now) {
                    bail!("задачу уже взял {holder}");
                }
            }
            if matches!(task.meta.status, TaskStatus::Done) {
                bail!("задача уже готова");
            }
            task.meta.claimed_by = Some(by.to_owned());
            task.meta.lease_until = Some(now + lease_delta(lease));
            task.meta.status = TaskStatus::InProgress;
            Ok(())
        })
    }

    /// Claim the first ready task (optionally matching a label).
    pub fn claim_ready(&self, by: &str, label: Option<&str>, lease: Duration) -> Result<Option<Task>> {
        let all = self.tasks();
        let next = all
            .iter()
            .filter(|t| t.is_ready(&all))
            .find(|t| label.is_none_or(|l| t.meta.labels.iter().any(|x| x == l)))
            .map(|t| t.id.clone());
        next.map(|id| self.claim(&id, by, lease)).transpose()
    }

    pub fn renew(&self, id: &str, by: &str, lease: Duration) -> Result<Task> {
        self.update_quiet(id, |task, now| {
            if task.meta.claimed_by.as_deref() != Some(by) {
                bail!("задача не у {by}");
            }
            task.meta.lease_until = Some(now + lease_delta(lease));
            Ok(())
        })
    }

    pub fn log(&self, id: &str, by: &str, text: &str) -> Result<Task> {
        self.update(id, by, "log", text, |_, _| Ok(()))
    }

    /// Done by the agent: hand over to a human for review.
    pub fn complete(&self, id: &str, by: &str, summary: &str) -> Result<Task> {
        self.update(id, by, "complete", summary, |task, _| {
            task.meta.status = TaskStatus::Review;
            task.meta.claimed_by = None;
            task.meta.lease_until = None;
            if !summary.is_empty() {
                task.meta.summary = Some(summary.to_owned());
            }
            Ok(())
        })
    }

    /// The agent needs an answer from a human.
    pub fn block(&self, id: &str, by: &str, question: &str) -> Result<Task> {
        self.update(id, by, "block", question, |task, _| {
            task.meta.status = TaskStatus::Blocked;
            task.meta.claimed_by = None;
            task.meta.lease_until = None;
            task.meta.summary = Some(question.to_owned());
            Ok(())
        })
    }

    /// Give the task back to the queue.
    pub fn release(&self, id: &str, by: &str, reason: &str) -> Result<Task> {
        self.update(id, by, "release", reason, |task, _| {
            task.meta.claimed_by = None;
            task.meta.lease_until = None;
            if task.meta.status == TaskStatus::InProgress {
                task.meta.status = TaskStatus::Todo;
            }
            Ok(())
        })
    }

    pub fn set_status(&self, id: &str, by: &str, status: TaskStatus) -> Result<Task> {
        self.update(id, by, "status", status.label(), |task, _| {
            task.meta.status = status;
            if status != TaskStatus::InProgress {
                task.meta.claimed_by = None;
                task.meta.lease_until = None;
            }
            Ok(())
        })
    }

    pub fn link_session(&self, id: &str, session: &str) -> Result<Task> {
        self.update_quiet(id, |task, _| {
            if !task.meta.sessions.iter().any(|s| s == session) {
                task.meta.sessions.push(session.to_owned());
            }
            Ok(())
        })
    }

    /// Return tasks whose holder stopped renewing. Returns the released ids.
    pub fn release_expired(&self) -> Vec<String> {
        let now = Utc::now();
        self.tasks()
            .into_iter()
            .filter(|t| t.meta.claimed_by.is_some() && t.lease_expired(now))
            .filter_map(|t| self.release(&t.id, "iv3", "аренда истекла").ok().map(|t| t.id))
            .collect()
    }

    pub fn events(&self, task: Option<&str>) -> Vec<Event> {
        events::read(&self.home, task)
    }

    // ----------------------------------------------------------------

    fn update(
        &self,
        id: &str,
        actor: &str,
        kind: &str,
        text: &str,
        change: impl FnOnce(&mut Task, DateTime<Utc>) -> Result<()>,
    ) -> Result<Task> {
        let task = self.update_quiet(id, change)?;
        self.journal(id, actor, kind, text);
        Ok(task)
    }

    fn update_quiet(&self, id: &str, change: impl FnOnce(&mut Task, DateTime<Utc>) -> Result<()>) -> Result<Task> {
        let _lock = self.lock()?;
        let mut task = self.get(id)?;
        let now = Utc::now();
        change(&mut task, now)?;
        task.meta.updated_at = Some(now);
        task.save()?;
        Ok(task)
    }

    fn journal(&self, task: &str, actor: &str, kind: &str, text: &str) {
        let event = Event { at: Utc::now(), task: Some(task.to_owned()), actor: actor.to_owned(), kind: kind.to_owned(), text: text.to_owned() };
        if let Err(err) = events::append(&self.home, &event) {
            eprintln!("iv3: journal: {err:#}");
        }
    }

    /// Copy attached files to `tasks/<id>.files/`, so the task keeps them when
    /// the originals (often a temp screenshot) go away. Folders are linked by
    /// path, not copied. Returns the paths to store in the task.
    fn store_attachments(&self, id: &str, files: &[PathBuf]) -> Result<Vec<PathBuf>> {
        if files.is_empty() {
            return Ok(Vec::new());
        }
        let dir = self.home.tasks().join(format!("{id}.files"));
        let copy_all = || -> Result<Vec<PathBuf>> {
            let mut stored = Vec::new();
            for src in files {
                if src.is_dir() {
                    stored.push(src.clone());
                    continue;
                }
                if !src.is_file() {
                    bail!("нет файла {}", src.display());
                }
                fs::create_dir_all(&dir)?;
                let name = src.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
                let dst = unique_path(&dir, &name);
                fs::copy(src, &dst).with_context(|| format!("не удалось скопировать {}", src.display()))?;
                stored.push(dst);
            }
            Ok(stored)
        };
        copy_all().inspect_err(|_| {
            let _ = fs::remove_dir_all(&dir);
        })
    }

    fn fresh_id(&self, now: DateTime<Utc>) -> String {
        let base = now.format("%y%m%d-%H%M%S").to_string();
        let mut id = base.clone();
        let mut n = 1;
        while self.home.tasks().join(format!("{id}.md")).exists() {
            n += 1;
            id = format!("{base}-{n}");
        }
        id
    }

    fn lock(&self) -> Result<LockGuard> {
        LockGuard::acquire(self.home.state().join("board.lock"))
    }
}

/// `dir/name`, or `dir/stem-2.ext`… if taken.
fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, format!(".{ext}")),
        _ => (name, String::new()),
    };
    (2..).map(|n| dir.join(format!("{stem}-{n}{ext}"))).find(|p| !p.exists()).expect("free name")
}

fn lease_delta(lease: Duration) -> chrono::Duration {
    chrono::Duration::from_std(lease).unwrap_or(chrono::Duration::hours(1))
}

/// Cross-process lock: a file created with `create_new`. Stale locks (a
/// crashed holder) are broken after a few seconds.
struct LockGuard(PathBuf);

impl LockGuard {
    fn acquire(path: PathBuf) -> Result<Self> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        for _ in 0..200 {
            match fs::OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(_) => return Ok(Self(path)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let stale = fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .is_ok_and(|t| t.elapsed().unwrap_or_default() > Duration::from_secs(5));
                    if stale {
                        let _ = fs::remove_file(&path);
                    } else {
                        std::thread::sleep(Duration::from_millis(15));
                    }
                }
                Err(e) => return Err(e.into()),
            }
        }
        bail!("доска занята: {}", path.display())
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn split_frontmatter(text: &str) -> (Option<&str>, &str) {
    let Some(rest) = text.strip_prefix("---\n") else { return (None, text) };
    match rest.find("\n---\n") {
        Some(end) => (Some(&rest[..end + 1]), &rest[end + 5..]),
        None => (None, text),
    }
}

fn first_heading(body: &str) -> Option<String> {
    body.lines().find_map(|l| l.strip_prefix("# ")).map(|s| s.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn board() -> (Board, PathBuf) {
        let root = std::env::temp_dir().join(format!("iv3-board-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = fs::remove_dir_all(&root);
        let home = Home { root: root.clone() };
        home.ensure().unwrap();
        (Board::new(home), root)
    }

    #[test]
    fn frontmatter_roundtrip() {
        let (m, b) = split_frontmatter("---\ntitle: x\nstatus: todo\n---\nbody\n");
        let meta: TaskMeta = serde_yaml::from_str(m.unwrap()).unwrap();
        assert_eq!(meta.title, "x");
        assert_eq!(meta.status, TaskStatus::Todo);
        assert_eq!(b, "body\n");
    }

    #[test]
    fn no_frontmatter_uses_heading() {
        let (m, b) = split_frontmatter("# Hello\ntext");
        assert!(m.is_none());
        assert_eq!(first_heading(b).as_deref(), Some("Hello"));
    }

    #[test]
    fn claim_complete_and_dependencies() {
        let (board, root) = board();
        let fix = board.create(NewTask { title: "fix".into(), status: TaskStatus::Todo, ..Default::default() }, "me").unwrap();
        let review = board
            .create(
                NewTask { title: "review".into(), status: TaskStatus::Todo, depends_on: vec![fix.id.clone()], ..Default::default() },
                "me",
            )
            .unwrap();

        // Only the fix is ready; the review waits for it.
        let all = board.tasks();
        assert!(all.iter().find(|t| t.id == fix.id).unwrap().is_ready(&all));
        assert!(!all.iter().find(|t| t.id == review.id).unwrap().is_ready(&all));

        let claimed = board.claim_ready("claude", None, Duration::from_secs(3600)).unwrap().unwrap();
        assert_eq!(claimed.id, fix.id);
        assert_eq!(claimed.meta.status, TaskStatus::InProgress);
        // A second agent cannot take it while the lease is live.
        assert!(board.claim(&fix.id, "codex", Duration::from_secs(60)).is_err());
        assert!(board.claim_ready("codex", None, Duration::from_secs(60)).unwrap().is_none());

        board.complete(&fix.id, "claude", "PR #12").unwrap();
        assert_eq!(board.get(&fix.id).unwrap().meta.status, TaskStatus::Review);
        board.set_status(&fix.id, "me", TaskStatus::Done).unwrap();

        // Now the review is ready.
        let all = board.tasks();
        assert!(all.iter().find(|t| t.id == review.id).unwrap().is_ready(&all));

        let journal: Vec<String> = board.events(Some(&fix.id)).into_iter().map(|e| e.kind).collect();
        assert_eq!(journal, ["create", "claim", "complete", "status"]);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn attachments_are_copied_next_to_the_task() {
        let (board, root) = board();
        let src = root.join("src");
        fs::create_dir_all(src.join("a")).unwrap();
        fs::create_dir_all(src.join("b")).unwrap();
        fs::write(src.join("a/shot.png"), b"png-a").unwrap();
        fs::write(src.join("b/shot.png"), b"png-b").unwrap();
        fs::write(src.join("log.txt"), b"log").unwrap();

        let files = vec![src.join("a/shot.png"), src.join("b/shot.png"), src.join("log.txt")];
        let t = board.create(NewTask { title: "t".into(), attachments: files, ..Default::default() }, "me").unwrap();
        let dir = root.join("tasks").join(format!("{}.files", t.id));
        assert_eq!(t.meta.attachments, [dir.join("shot.png"), dir.join("shot-2.png"), dir.join("log.txt")]);
        assert_eq!(fs::read(dir.join("shot-2.png")).unwrap(), b"png-b");
        // Survives the originals and a reload.
        fs::remove_dir_all(&src).unwrap();
        assert_eq!(board.get(&t.id).unwrap().meta.attachments.len(), 3);
        assert_eq!(board.tasks().len(), 1);

        // A missing file fails the create and leaves nothing behind.
        let missing = NewTask { title: "x".into(), attachments: vec![root.join("nope.png")], ..Default::default() };
        assert!(board.create(missing, "me").is_err());
        assert_eq!(board.tasks().len(), 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn expired_lease_returns_task_to_queue() {
        let (board, root) = board();
        let t = board.create(NewTask { title: "t".into(), status: TaskStatus::Todo, ..Default::default() }, "me").unwrap();
        board.claim(&t.id, "claude", Duration::from_millis(1)).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(board.release_expired(), vec![t.id.clone()]);
        let t = board.get(&t.id).unwrap();
        assert_eq!(t.meta.status, TaskStatus::Todo);
        assert!(t.meta.claimed_by.is_none());
        let _ = fs::remove_dir_all(root);
    }
}
