//! Tasks: `~/.stepan_verify/tasks/<id>.md`, YAML frontmatter + markdown body.
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
    Home, Provider,
    events::{self, Event},
    worktree::{self, Isolation},
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

/// Where a plan-first task is: the agent writes a plan map, or implements the approved one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Plan,
    Implement,
}

impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Phase::Plan => "план",
            Phase::Implement => "по плану",
        }
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
    /// Own git worktree or the project folder; unset = decide at run time
    /// (see `worktree::resolve`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolation: Option<Isolation>,
    /// The task's own checkout, once made (`~/.stepan_verify/worktrees/<project>/<id>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<PathBuf>,
    /// Its branch, `stepan/<id>`; stays when the copy is removed but the branch kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Model when Claude takes the task; unset = Настройки → «Модель по умолчанию».
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_model: Option<String>,
    /// Model when Codex takes the task; unset = the default from settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_model: Option<String>,
    /// `key=value` labels, e.g. `kind=bug`, `area=admin`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    /// Task ids that must be done before this one is ready.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    /// Attached files (screenshots, logs…), copied to `tasks/<id>.files/`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<PathBuf>,
    /// «Сначала план»: the agent first draws the target state, you approve it, then it implements.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub plan: bool,
    /// Phase of a plan-first task (unset on direct tasks).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<Phase>,
    /// The approved plan iteration: `state/plans/<id>/runs/<n>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_approved: Option<usize>,
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

/// A task file: `~/.stepan_verify/tasks/<id>.md` with YAML frontmatter.
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
        self.meta.project.clone().or_else(|| self.meta.cwd.as_deref().and_then(worktree::project_name))
    }

    /// The model chosen for `provider` on this task, if any.
    pub fn model(&self, provider: Provider) -> Option<&str> {
        match provider {
            Provider::Claude => self.meta.claude_model.as_deref(),
            Provider::Codex => self.meta.codex_model.as_deref(),
        }
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

    /// The phase a plan-first task is in; None for a direct task.
    pub fn phase(&self) -> Option<Phase> {
        self.meta.plan.then(|| self.meta.phase.unwrap_or(Phase::Plan))
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
    pub isolation: Option<Isolation>,
    /// Per-agent model; `None` = the default from settings at run time.
    pub claude_model: Option<String>,
    pub codex_model: Option<String>,
    /// Files to attach; `create` copies them next to the task.
    pub attachments: Vec<PathBuf>,
    /// «Сначала план».
    pub plan: bool,
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
                isolation: new.isolation,
                claude_model: new.claude_model.filter(|m| !m.is_empty()),
                codex_model: new.codex_model.filter(|m| !m.is_empty()),
                labels: new.labels,
                depends_on: new.depends_on,
                attachments,
                plan: new.plan,
                phase: new.plan.then_some(Phase::Plan),
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

    /// «Сначала план» on or off. Turning it on before a plan is approved starts at the plan phase.
    pub fn set_plan(&self, id: &str, by: &str, plan: bool) -> Result<Task> {
        self.update(id, by, "workflow", if plan { "сначала план" } else { "сразу делать" }, |task, _| {
            task.meta.plan = plan;
            task.meta.phase = match (plan, task.meta.plan_approved) {
                (false, _) => None,
                (true, Some(_)) => Some(Phase::Implement),
                (true, None) => Some(Phase::Plan),
            };
            Ok(())
        })
    }

    /// The agent wrote plan iteration `run`: hand it over for review as a plan.
    pub fn plan_ready(&self, id: &str, by: &str, run: usize, summary: &str) -> Result<Task> {
        let text = if summary.is_empty() { format!("итерация #{run}") } else { format!("итерация #{run} · {summary}") };
        self.update(id, by, "plan_ready", &text, |task, _| {
            check_planning(task)?;
            task.meta.phase = Some(Phase::Plan);
            task.meta.status = TaskStatus::Review;
            task.meta.claimed_by = None;
            task.meta.lease_until = None;
            task.meta.summary = Some(if summary.is_empty() { format!("План готов: итерация #{run}") } else { summary.to_owned() });
            Ok(())
        })
    }

    /// You approved plan iteration `run`: the task goes back to the queue to be implemented.
    pub fn approve_plan(&self, id: &str, by: &str, run: usize) -> Result<Task> {
        self.update(id, by, "plan_approved", &format!("итерация #{run}"), |task, _| {
            check_planning(task)?;
            task.meta.phase = Some(Phase::Implement);
            task.meta.plan_approved = Some(run);
            task.meta.status = TaskStatus::Todo;
            task.meta.claimed_by = None;
            task.meta.lease_until = None;
            task.meta.summary = None;
            Ok(())
        })
    }

    pub fn set_isolation(&self, id: &str, by: &str, isolation: Option<Isolation>) -> Result<Task> {
        let text = match isolation {
            Some(Isolation::Worktree) => "отдельная копия",
            Some(Isolation::Here) => "здесь",
            None => "авто",
        };
        self.update(id, by, "isolation", text, |task, _| {
            task.meta.isolation = isolation;
            Ok(())
        })
    }

    /// Choose the model `provider` runs this task on (`None` = settings default).
    pub fn set_model(&self, id: &str, by: &str, provider: Provider, model: Option<String>) -> Result<Task> {
        let model = model.filter(|m| !m.is_empty());
        let text = format!("{}: {}", provider.label(), model.as_deref().unwrap_or("по умолчанию"));
        self.update(id, by, "model", &text, |task, _| {
            match provider {
                Provider::Claude => task.meta.claude_model = model,
                Provider::Codex => task.meta.codex_model = model,
            }
            Ok(())
        })
    }

    /// Record the task's copy (or that it is gone; a kept branch stays in `branch`).
    pub fn set_worktree(&self, id: &str, by: &str, path: Option<PathBuf>, branch: Option<String>, text: &str) -> Result<Task> {
        self.update(id, by, "worktree", text, |task, _| {
            task.meta.worktree = path;
            task.meta.branch = branch;
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
            .filter_map(|t| self.release(&t.id, "stepan_verify", "аренда истекла").ok().map(|t| t.id))
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
            eprintln!("stepan_verify: journal: {err:#}");
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

/// A plan can be written or approved only while the task is at its plan phase.
fn check_planning(task: &Task) -> Result<()> {
    match task.phase() {
        None => bail!("у задачи нет этапа плана — включи «Сначала план»"),
        Some(Phase::Implement) => bail!("план уже утверждён — задача на реализации"),
        Some(Phase::Plan) => Ok(()),
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
        let root = std::env::temp_dir().join(format!("stepan_verify-board-{}-{:?}", std::process::id(), std::thread::current().id()));
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
    fn plan_first_workflow_and_journal() {
        let (board, root) = board();
        let direct = board.create(NewTask { title: "d".into(), status: TaskStatus::Todo, ..Default::default() }, "me").unwrap();
        assert_eq!(direct.phase(), None);
        assert!(board.plan_ready(&direct.id, "claude", 1, "").is_err(), "direct tasks have no plan");

        let t = board.create(NewTask { title: "p".into(), status: TaskStatus::Todo, plan: true, ..Default::default() }, "me").unwrap();
        assert_eq!(t.phase(), Some(Phase::Plan));
        board.claim(&t.id, "claude", Duration::from_secs(60)).unwrap();
        let t2 = board.plan_ready(&t.id, "claude", 1, "три этапа").unwrap();
        assert_eq!((t2.meta.status, t2.phase(), t2.meta.claimed_by.clone()), (TaskStatus::Review, Some(Phase::Plan), None));
        // Remarks: the agent takes it again and writes iteration 2.
        board.claim(&t.id, "claude", Duration::from_secs(60)).unwrap();
        board.plan_ready(&t.id, "claude", 2, "").unwrap();
        let t3 = board.approve_plan(&t.id, "me", 2).unwrap();
        assert_eq!((t3.meta.status, t3.phase(), t3.meta.plan_approved), (TaskStatus::Todo, Some(Phase::Implement), Some(2)));
        assert!(t3.is_ready(&board.tasks()));
        assert!(board.plan_ready(&t.id, "claude", 3, "").is_err(), "approved plans stay");
        assert!(board.approve_plan(&t.id, "me", 3).is_err());

        // Off and on again keeps the approved phase; a fresh task starts at the plan.
        assert_eq!(board.set_plan(&t.id, "me", false).unwrap().phase(), None);
        assert_eq!(board.set_plan(&t.id, "me", true).unwrap().phase(), Some(Phase::Implement));
        assert_eq!(board.set_plan(&direct.id, "me", true).unwrap().phase(), Some(Phase::Plan));

        let journal: Vec<(String, String)> = board.events(Some(&t.id)).into_iter().map(|e| (e.kind, e.text)).collect();
        let kinds: Vec<&str> = journal.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(kinds, ["create", "claim", "plan_ready", "claim", "plan_ready", "plan_approved", "workflow", "workflow"]);
        assert_eq!(journal[2].1, "итерация #1 · три этапа");
        assert_eq!(journal[5].1, "итерация #2");
        // Survives a reload from disk.
        let saved = board.get(&t.id).unwrap();
        assert!(saved.meta.plan && saved.meta.plan_approved == Some(2));
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
