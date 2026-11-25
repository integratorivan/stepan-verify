use std::{path::PathBuf, time::Duration};

use gpui_kit::component::{
    ActiveTheme, IconName, Sizable as _, WindowExt as _,
    notification::Notification,
    resizable::{h_resizable, resizable_panel},
    switch::Switch,
    Icon, ThemeMode,
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use crate::ui::PressButton;
use stepan_verify_core::{Board, Home, Provider, SessionSummary, Task as BoardTask, TaskStatus};

use crate::{
    dashboard,
    docs::DocsView,
    new_session::{NewSessionForm, StartSession},
    new_task::{CreateTask, NewTaskForm},
    preview::{PreviewEvent, PreviewView},
    project_picker::ProjectPicker,
    session::{Activity, SessionEvent, SessionView, Status},
    ui::{self, Palette},
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Dashboard,
    Tasks,
    Docs,
    Skills,
    Settings,
    Session(usize),
}

/// Command-line launch options:
/// `stepan_verify [--new] [--open <session-id>] [--start claude|codex --cwd <dir>] [--send <text>]`.
#[derive(Default)]
pub struct Launch {
    open: Option<String>,
    send: Option<String>,
    new: bool,
    start: Option<Provider>,
    cwd: Option<PathBuf>,
    /// Keep the dashboard in front after `--start`.
    stay: bool,
    /// With `--open`: show the «Изменения» tab (builds the report if missing).
    changes: bool,
    page: Option<Page>,
    autopilot: bool,
    /// `--preview <path>`: open a file in the preview panel.
    preview: Option<PathBuf>,
    /// `--doc <rel>`: open «Документы» on this document (path inside docs/).
    doc: Option<String>,
}

impl Launch {
    pub fn from_args(mut args: impl Iterator<Item = String>) -> Self {
        let mut launch = Self::default();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--open" => launch.open = args.next(),
                "--send" => launch.send = args.next(),
                "--new" => launch.new = true,
                "--start" => {
                    launch.start = match args.next().as_deref() {
                        Some("codex") => Some(Provider::Codex),
                        _ => Some(Provider::Claude),
                    }
                }
                "--cwd" => launch.cwd = args.next().map(PathBuf::from),
                "--stay" => launch.stay = true,
                "--changes" => launch.changes = true,
                "--autopilot" => launch.autopilot = true,
                "--preview" => launch.preview = args.next().map(|p| std::fs::canonicalize(&p).unwrap_or_else(|_| p.into())),
                "--doc" => {
                    launch.doc = args.next();
                    launch.page = Some(Page::Docs);
                }
                "--page" => {
                    launch.page = match args.next().as_deref() {
                        Some("tasks") => Some(Page::Tasks),
                        Some("docs") => Some(Page::Docs),
                        Some("skills") => Some(Page::Skills),
                        Some("settings") => Some(Page::Settings),
                        _ => Some(Page::Dashboard),
                    }
                }
                _ => {}
            }
        }
        launch
    }
}

/// An open session as the dashboard sees it.
pub struct LiveSession {
    pub ix: usize,
    pub provider: Provider,
    pub title: SharedString,
    pub project: SharedString,
    pub activity: Activity,
    pub card: crate::live_card::Card,
    pub elapsed: Option<u64>,
}

/// A session running outside stepan_verify (terminal, Codex Desktop…), seen from its file.
pub struct ExternalLive {
    pub summary: SessionSummary,
    pub card: crate::live_card::Card,
    pub running: bool,
}

/// A file touched this recently means the agent is active.
const ACTIVE_SECS: u64 = 120;
/// Without explicit turn markers (Claude files), a fresh write means it is mid-turn.
const RUNNING_SECS: u64 = 20;

/// Window root. Owns the data snapshots every page reads from.
pub struct AppView {
    pub home: Home,
    page: Page,
    pub sessions: Vec<SessionSummary>,
    pub sessions_loading: bool,
    pub tasks: Vec<BoardTask>,
    pub board: Board,
    /// Task shown in the detail panel of the board.
    pub selected_task: Option<String>,
    /// Pick up ready tasks automatically (up to the configured limit at once).
    pub autopilot: bool,
    pub skills: Vec<String>,
    docs: Entity<DocsView>,
    skills_view: Entity<crate::skills::SkillsView>,
    /// Text and code font pickers on the settings page, made on first visit.
    pub(crate) font_pickers: Option<[Entity<crate::settings_page::FontPicker>; 2]>,
    /// Sessions opened in this window, in opening order.
    pub(crate) open_sessions: Vec<Entity<SessionView>>,
    /// Project pickers in open dialogs, refreshed when the scan finishes.
    pickers: Vec<WeakEntity<ProjectPicker>>,
    launch: Launch,
    /// File preview panel on the right, opened from session attachments.
    preview: Option<Entity<PreviewView>>,
    /// Active sessions outside stepan_verify, refreshed by the tick while the dashboard is shown.
    pub external: Vec<ExternalLive>,
    /// Sessions put away: hidden from «Сейчас» and «Недавние».
    pub archive: stepan_verify_core::archive::Archive,
    _tick: Task<()>,
}

/// How long a claim holds without renewal.
const LEASE: Duration = Duration::from_secs(15 * 60);

impl AppView {
    pub fn new(home: Home, launch: Launch, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let archive = stepan_verify_core::archive::Archive::load(&home);
        let app_handle = cx.weak_entity();
        let docs = cx.new(|cx| DocsView::new(home.clone(), app_handle, window, cx));
        cx.subscribe_in(&docs, window, |this, _, event: &crate::docs::DocsEvent, window, cx| {
            use crate::docs::DocsEvent;
            match event {
                DocsEvent::OpenSession(summary) => this.open_session(summary, window, cx),
                DocsEvent::OpenFile(path) => this.open_preview(path.clone(), None, window, cx),
                DocsEvent::SendToSession { ix, text } => {
                    if let Some(view) = this.open_sessions.get(*ix).cloned() {
                        view.update(cx, |v, cx| v.send_text(text.clone(), cx));
                        this.show(Page::Session(*ix), cx);
                    }
                }
                DocsEvent::NewSession { provider, cwd, prompt } => {
                    let start = StartSession { provider: *provider, cwd: cwd.clone(), prompt: prompt.clone(), images: Vec::new() };
                    this.start_session(&start, window, cx);
                }
                DocsEvent::TaskChanged => this.reload_tasks(cx),
                DocsEvent::ShowTasks => this.show(Page::Tasks, cx),
            }
        })
        .detach();
        let skills_view = cx.new(|cx| crate::skills::SkillsView::new(home.clone(), window, cx));
        cx.subscribe_in(&skills_view, window, |this, _, event: &crate::skills::SkillsEvent, window, cx| match event {
            crate::skills::SkillsEvent::OpenSession(summary) => this.open_session(summary, window, cx),
            crate::skills::SkillsEvent::OpenFile(path) => this.open_preview(path.clone(), None, window, cx),
        })
        .detach();
        cx.set_global(crate::preview::Opener(cx.entity().downgrade()));
        let mut this = Self {
            tasks: BoardTask::load_all(&home.tasks()),
            board: Board::new(home.clone()),
            selected_task: None,
            autopilot: false,
            skills: home.list_skills(),
            home,
            page: Page::Dashboard,
            sessions: Vec::new(),
            sessions_loading: true,
            docs,
            font_pickers: None,
            skills_view,
            open_sessions: Vec::new(),
            pickers: Vec::new(),
            launch,
            preview: None,
            external: Vec::new(),
            archive,
            _tick: Task::ready(()),
        };
        if let Some(page) = this.launch.page {
            this.page = page;
        }
        if let Some(rel) = this.launch.doc.take() {
            this.docs.update(cx, |d, _| d.open_on_load(rel));
        }
        this.autopilot = this.launch.autopilot;
        this.refresh_sessions(window, cx);
        // Board heartbeat: pick up file changes, renew and expire leases, run the autopilot.
        this._tick = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(3)).await;
                if this.update_in(cx, |this, window, cx| this.tick(window, cx)).is_err() {
                    break;
                }
            }
        });
        this
    }

    pub fn refresh_sessions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sessions_loading = true;
        self.tasks = self.board.tasks();
        self.skills = self.home.list_skills();
        cx.spawn_in(window, async move |this, cx| {
            let sessions = cx
                .background_spawn(async { stepan_verify_core::sessions::scan_all(Duration::from_secs(14 * 86400)) })
                .await;
            this.update_in(cx, |this, window, cx| {
                this.sessions = sessions;
                this.sessions_loading = false;
                let dirs = this.project_dirs();
                this.pickers.retain(|p| p.upgrade().is_some());
                for picker in this.pickers.iter().filter_map(|p| p.upgrade()) {
                    picker.update(cx, |p, cx| p.set_projects(dirs.clone(), cx));
                }
                this.run_launch_options(window, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn run_launch_options(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.launch.new) {
            self.open_new_session_dialog(window, cx);
        }
        if let (Some(provider), Some(cwd), Some(prompt)) =
            (self.launch.start.take(), self.launch.cwd.take(), self.launch.send.clone())
        {
            self.launch.send = None;
            self.start_session(&StartSession { provider, cwd, prompt, images: Vec::new() }, window, cx);
            if self.launch.stay {
                self.show(Page::Dashboard, cx);
            }
        }
        if let Some(id) = self.launch.open.take() {
            if let Some(summary) = self.sessions.iter().find(|s| s.id == id).cloned() {
                self.open_session(&summary, window, cx);
                if let (Some(text), Some(view)) = (self.launch.send.take(), self.open_sessions.last()) {
                    view.update(cx, |view, cx| view.send_text(text, cx));
                }
                if self.launch.changes {
                    if let Some(view) = self.open_sessions.last() {
                        view.update(cx, |view, cx| view.open_changes(cx));
                    }
                }
            }
        }
        if let Some(path) = self.launch.preview.take() {
            self.open_preview(path, None, window, cx);
        }
    }

    /// Show a session inside the app, reusing the tab if it is already open.
    pub fn open_session(&mut self, summary: &SessionSummary, window: &mut Window, cx: &mut Context<Self>) {
        let existing = self
            .open_sessions
            .iter()
            .position(|s| s.read(cx).native_id.as_deref() == Some(summary.id.as_str()));
        let ix = match existing {
            Some(ix) => ix,
            None => {
                let view = cx.new(|cx| SessionView::resume(summary, window, cx));
                self.adopt(view, window, cx)
            }
        };
        self.show(Page::Session(ix), cx);
    }

    fn start_session(&mut self, start: &StartSession, window: &mut Window, cx: &mut Context<Self>) {
        let (provider, cwd, prompt, images) = (start.provider, start.cwd.clone(), start.prompt.clone(), start.images.clone());
        let view = cx.new(|cx| SessionView::start(provider, cwd, prompt, images, window, cx));
        let ix = self.adopt(view, window, cx);
        self.show(Page::Session(ix), cx);
    }

    /// Wire a session into the window: re-render on change, close on request,
    /// and toast when it needs you while you are looking elsewhere.
    fn adopt(&mut self, view: Entity<SessionView>, window: &mut Window, cx: &mut Context<Self>) -> usize {
        cx.observe(&view, |_, _, cx| cx.notify()).detach();
        cx.subscribe_in(&view, window, |this, view, event: &SessionEvent, window, cx| {
            let Some(ix) = this.open_sessions.iter().position(|v| v == view) else { return };
            let title = view.read(cx).title.clone();
            let watching = this.page == Page::Session(ix) && window.is_window_active();
            this.on_task_session_event(view, event, cx);
            match event {
                SessionEvent::Close => this.close_session(ix, cx),
                SessionEvent::OpenFile { path, root } => this.open_preview(path.clone(), Some(root.clone()), window, cx),
                SessionEvent::NeedsApproval(what) if !watching => {
                    window.push_notification(
                        Notification::warning(format!("Ждёт подтверждения: {what}")).title(title),
                        cx,
                    );
                }
                SessionEvent::TurnFinished { error } if !watching => {
                    let note = match error {
                        Some(err) => Notification::error(err.clone()),
                        None => Notification::success("Агент закончил и ждёт тебя"),
                    };
                    window.push_notification(note.title(title), cx);
                }
                _ => {}
            }
        })
        .detach();
        self.open_sessions.push(view);
        self.open_sessions.len() - 1
    }

    /// Close an open session (its agent process stops). The page you look at
    /// stays put unless it was this session.
    pub(crate) fn close_session(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix >= self.open_sessions.len() {
            return;
        }
        self.open_sessions.remove(ix);
        let page = match self.page {
            Page::Session(j) if j == ix => Page::Dashboard,
            Page::Session(j) if j > ix => Page::Session(j - 1),
            page => page,
        };
        self.show(page, cx);
    }

    /// Hide a session from the dashboard (or bring it back).
    pub(crate) fn set_archived(&mut self, provider: Provider, id: &str, archived: bool, window: &mut Window, cx: &mut Context<Self>) {
        match self.archive.set(provider, id, archived) {
            Ok(()) => {
                if archived {
                    self.external.retain(|e| !(e.summary.provider == provider && e.summary.id == id));
                }
                let text = if archived { "Сессия в архиве · вернуть можно в «Недавние → Архив»" } else { "Сессия вернулась из архива" };
                window.push_notification(Notification::info(text), cx);
            }
            Err(err) => window.push_notification(Notification::error(format!("Архив: {err:#}")), cx),
        }
        cx.notify();
    }

    /// Show a file in the panel on the right, reusing the panel if it is open.
    pub(crate) fn open_preview(&mut self, path: PathBuf, root: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(preview) = &self.preview {
            preview.update(cx, |p, cx| p.open(path, root, window, cx));
        } else {
            let preview = cx.new(|cx| PreviewView::new(path, root, window, cx));
            cx.subscribe(&preview, |this, _, PreviewEvent::Close, cx| {
                this.preview = None;
                cx.notify();
            })
            .detach();
            self.preview = Some(preview);
        }
        cx.notify();
    }

    // ------------------------------------------------------------- board

    pub fn reload_tasks(&mut self, cx: &mut Context<Self>) {
        self.tasks = self.board.tasks();
        cx.notify();
    }

    fn toast_error(window: &mut Window, err: anyhow::Error, cx: &mut Context<Self>) {
        window.push_notification(Notification::error(format!("{err:#}")), cx);
    }

    pub fn create_task(&mut self, new: stepan_verify_core::NewTask, window: &mut Window, cx: &mut Context<Self>) {
        match self.board.create(new, "me") {
            Ok(task) => self.selected_task = Some(task.id),
            Err(err) => Self::toast_error(window, err, cx),
        }
        self.reload_tasks(cx);
    }

    /// Claim a task for an agent and start a session on it.
    pub fn run_task(&mut self, id: &str, provider: Provider, window: &mut Window, cx: &mut Context<Self>) {
        let agent = provider.label().to_lowercase();
        let task = match self.board.claim(id, &agent, LEASE) {
            Ok(task) => task,
            Err(err) => return Self::toast_error(window, err, cx),
        };
        let view = cx.new(|cx| SessionView::start_task(provider, &task, window, cx));
        self.adopt(view, window, cx);
        window.push_notification(Notification::info(format!("{} взял задачу", provider.label())).title(task.meta.title.clone()), cx);
        self.reload_tasks(cx);
    }

    pub fn set_task_status(&mut self, id: &str, status: TaskStatus, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(err) = self.board.set_status(id, "me", status) {
            Self::toast_error(window, err, cx);
        }
        self.reload_tasks(cx);
    }

    /// The open session working on a task, if any.
    pub fn task_session(&self, id: &str, cx: &App) -> Option<(usize, &Entity<SessionView>)> {
        self.open_sessions.iter().enumerate().rev().find(|(_, v)| v.read(cx).task_id.as_deref() == Some(id))
    }

    /// Jump to the task's session: the open one, else resume the last linked one.
    pub fn open_task_session(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((ix, _)) = self.task_session(id, cx) {
            return self.show(Page::Session(ix), cx);
        }
        let linked = self.tasks.iter().find(|t| t.id == id).and_then(|t| t.meta.sessions.last().cloned());
        let summary = linked
            .and_then(|s| s.split_once(':').map(|(_, native)| native.to_owned()))
            .and_then(|native| self.sessions.iter().find(|s| s.id == native).cloned());
        match summary {
            Some(summary) => {
                self.open_session(&summary, window, cx);
                if let Some(view) = self.open_sessions.last() {
                    view.update(cx, |v, _| v.task_id = Some(id.to_owned()));
                }
            }
            None => window.push_notification(Notification::warning("Сессия этой задачи не найдена — обнови список сессий"), cx),
        }
    }

    /// Keep the board in step with what the agent does in the session.
    fn on_task_session_event(&mut self, view: &Entity<SessionView>, event: &SessionEvent, cx: &mut Context<Self>) {
        let (task_id, agent, activity) = {
            let v = view.read(cx);
            let Some(id) = v.task_id.clone() else { return };
            (id, v.provider.label().to_lowercase(), v.activity())
        };
        let Some(task) = self.tasks.iter().find(|t| t.id == task_id) else { return };
        let result = match event {
            // You continued the conversation: the agent is on it again.
            SessionEvent::Busy if task.meta.status != TaskStatus::InProgress => self.board.claim(&task_id, &agent, LEASE).map(drop),
            // The agent stopped and waits: hand over for review unless it already completed or blocked.
            SessionEvent::TurnFinished { error } if task.meta.status == TaskStatus::InProgress => {
                let summary = match error {
                    Some(err) => format!("Ошибка: {err}"),
                    None => activity.line.clone(),
                };
                self.board.complete(&task_id, &agent, &summary).map(drop)
            }
            SessionEvent::Close if task.meta.status == TaskStatus::InProgress => {
                self.board.release(&task_id, &agent, "сессия закрыта").map(drop)
            }
            _ => Ok(()),
        };
        if let Err(err) = result {
            eprintln!("stepan_verify: board: {err:#}");
        }
        self.reload_tasks(cx);
    }

    /// Every few seconds: link sessions, renew leases of live agents, expire
    /// abandoned ones, pick up files changed by agents, run the autopilot.
    fn tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for view in &self.open_sessions {
            let v = view.read(cx);
            let Some(id) = v.task_id.clone() else { continue };
            let agent = v.provider.label().to_lowercase();
            if let Some(native) = &v.native_id {
                let key = format!("{agent}:{native}");
                let linked = self.tasks.iter().any(|t| t.id == id && t.meta.sessions.contains(&key));
                if !linked {
                    let _ = self.board.link_session(&id, &key);
                }
            }
            if matches!(v.status, Status::Running | Status::Loading) {
                let _ = self.board.renew(&id, &agent, LEASE);
            }
        }
        self.board.release_expired();
        self.tasks = self.board.tasks();
        if self.autopilot {
            self.autopilot_step(window, cx);
        }
        if self.page == Page::Dashboard {
            self.refresh_external(cx);
        }
        cx.notify();
    }

    /// Peek at sessions whose transcript changed in the last minutes and that
    /// are not open here: the tail of the file says what the agent is doing.
    fn refresh_external(&mut self, cx: &App) {
        let open: Vec<String> = self.open_sessions.iter().filter_map(|v| v.read(cx).native_id.clone()).collect();
        let now = std::time::SystemTime::now();
        let recent = chrono::Utc::now() - chrono::Duration::minutes(30);
        self.external = self
            .sessions
            .iter()
            .filter(|s| s.updated_at > recent && !open.contains(&s.id) && !self.archive.contains(s.provider, &s.id) && !crate::dashboard::is_internal(&s.title))
            .filter_map(|s| {
                let mtime = std::fs::metadata(&s.file).and_then(|m| m.modified()).ok()?;
                let age = now.duration_since(mtime).unwrap_or_default().as_secs();
                (age <= ACTIVE_SECS).then_some((s, age))
            })
            .map(|(s, age)| {
                let peek = match s.provider {
                    Provider::Claude => stepan_verify_agents::peek::peek_claude(&s.file),
                    Provider::Codex => stepan_verify_agents::peek::peek_codex(&s.file),
                };
                let running = peek.running.unwrap_or(age <= RUNNING_SECS);
                let card = crate::live_card::build(&peek.items, running, &[], &|_| None);
                ExternalLive { summary: s.clone(), card, running }
            })
            .collect();
    }

    fn autopilot_step(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let busy = self.autopilot_busy(cx);
        let ready: Vec<(String, Provider)> = self
            .tasks
            .iter()
            .filter(|t| t.is_ready(&self.tasks) && t.meta.cwd.is_some())
            .map(|t| {
                let codex = t.meta.labels.iter().any(|l| l == "agent=codex");
                let provider = if codex { Provider::Codex } else { crate::settings::config(cx).agents.default_provider.into() };
                (t.id.clone(), provider)
            })
            .take(crate::settings::config(cx).agents.autopilot_limit.saturating_sub(busy))
            .collect();
        for (id, provider) in ready {
            self.run_task(&id, provider, window, cx);
        }
    }

    pub fn open_new_task_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let loading = self.sessions_loading;
        let form = cx.new(|cx| NewTaskForm::new(self.project_dirs(), loading, window, cx));
        self.pickers.push(form.read(cx).picker.downgrade());
        cx.subscribe_in(&form, window, |this, _, CreateTask(new), window, cx| {
            window.close_dialog(cx);
            this.create_task(new.clone(), window, cx);
        })
        .detach();
        window.open_dialog(cx, move |dialog, _, _| dialog.title("Новая задача").w(px(640.)).child(form.clone()));
    }

    /// «Пауза» on a dashboard card: stop the agent's current turn.
    pub fn interrupt_session(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(view) = self.open_sessions.get(ix) {
            view.read(cx).interrupt();
        }
    }

    pub fn show(&mut self, page: Page, cx: &mut Context<Self>) {
        self.page = page;
        // The docs reader may hold a native web view: it must hide off-page.
        self.docs.update(cx, |d, cx| d.set_visible(page == Page::Docs, cx));
        if page == Page::Skills {
            self.skills_view.update(cx, |v, cx| v.refresh_if_stale(cx));
        }
        cx.notify();
    }

    pub fn live_sessions(&self, cx: &App) -> Vec<LiveSession> {
        self.open_sessions
            .iter()
            .enumerate()
            .map(|(ix, view)| {
                let s = view.read(cx);
                LiveSession {
                    ix,
                    provider: s.provider,
                    title: s.title.clone(),
                    project: s.project.clone(),
                    activity: s.activity(),
                    card: s.card(),
                    elapsed: s.turn_elapsed(),
                }
            })
            .collect()
    }

    /// Working directories worth offering for a new session, most recent first.
    fn project_dirs(&self) -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = Vec::new();
        for s in &self.sessions {
            let Some(cwd) = &s.cwd else { continue };
            // Scratch places, not projects: temp dirs, t3code worktrees, Codex Desktop chats.
            let codex_chats = dirs_home().join("Documents/Codex");
            let skip = cwd.starts_with("/private/tmp")
                || cwd.starts_with("/tmp")
                || cwd.starts_with(&codex_chats)
                || cwd.components().any(|c| c.as_os_str() == ".t3");
            if !skip && !dirs.contains(cwd) && cwd.is_dir() {
                dirs.push(cwd.clone());
            }
        }
        dirs
    }

    pub fn open_new_session_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let loading = self.sessions_loading;
        let form = cx.new(|cx| NewSessionForm::new(self.project_dirs(), loading, window, cx));
        self.pickers.push(form.read(cx).picker.downgrade());
        cx.subscribe_in(&form, window, |this, _, start: &StartSession, window, cx| {
            window.close_dialog(cx);
            this.start_session(start, window, cx);
        })
        .detach();
        window.open_dialog(cx, move |dialog, _, _| dialog.title("Новая сессия").w(px(620.)).child(form.clone()));
    }

    /// Open sessions an autopilot task occupies right now.
    fn autopilot_busy(&self, cx: &App) -> usize {
        self.open_sessions
            .iter()
            .filter(|v| {
                let v = v.read(cx);
                v.task_id.is_some() && matches!(v.status, Status::Running | Status::Loading)
            })
            .count()
    }

    fn nav_item(&self, label: &'static str, icon: IconName, page: Page, count: Option<(String, bool)>, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let p = Palette::of(&theme);
        let active = self.page == page;
        h_flex()
            .id(SharedString::from(format!("nav:{label}")))
            .h(px(31.))
            .px(px(8.))
            .gap(px(9.))
            .rounded(px(6.))
            .border_1()
            .border_color(transparent_black())
            .text_size(px(13.5))
            .font_weight(FontWeight::MEDIUM)
            .text_color(p.fg)
            .cursor_pointer()
            .map(|this| {
                if active {
                    this.bg(p.surface).border_color(p.border).shadow(ui::edge_shadow(p.border_strong, 1.)).font_weight(FontWeight::SEMIBOLD)
                } else {
                    this.hover(|s| s.bg(p.ink.opacity(0.05)))
                }
            })
            .on_click(cx.listener(move |this, _, _, cx| this.show(page, cx)))
            .child(Icon::new(icon).small().text_color(if active { p.fg } else { p.muted }))
            .child(div().flex_1().min_w_0().truncate().child(label))
            .when_some(count, |this, (n, hot)| {
                this.child(if hot {
                    ui::badge(n, p.amber).font_family(theme.mono_font_family.clone()).text_size(px(11.))
                } else {
                    ui::mono(n, &theme).text_size(px(11.)).text_color(p.muted)
                })
            })
    }

    /// An open session: title plus a mono "agent · state" line.
    fn session_item(&self, s: LiveSession, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let p = Palette::of(&theme);
        let a = &s.activity;
        let asks = s.card.steps.iter().any(|st| st.state == crate::live_card::StepState::NeedsAnswer);
        let (icon, color, state): (IconName, Hsla, String) = match (a.status, a.approvals) {
            (_, n) if n > 0 => (IconName::TriangleAlert, p.red.solid, "подтверждение".into()),
            (Status::Running, _) => (IconName::LoaderCircle, p.blue.solid, s.card.progress.to_lowercase()),
            (Status::Loading, _) => (IconName::Loader, p.faint, "загрузка".into()),
            (Status::Exited, _) => (IconName::CircleX, p.red.solid, "завершён".into()),
            (Status::Idle, _) if asks => (IconName::Info, p.amber.solid, "вопрос".into()),
            (Status::Idle, _) => (IconName::CircleCheck, p.muted, "ждёт тебя".into()),
        };
        let state_color = match (a.status, a.approvals) {
            (_, n) if n > 0 => p.red.text,
            (Status::Idle, _) if asks => p.amber.text,
            _ => p.faint,
        };
        let (agent_color, agent) = match s.provider {
            Provider::Claude => (p.claude.1, "claude"),
            Provider::Codex => (p.fg, "codex"),
        };
        let ix = s.ix;
        let active = self.page == Page::Session(ix);
        h_flex()
            .id(SharedString::from(format!("nav-session:{ix}")))
            .px(px(8.))
            .py(px(6.))
            .gap(px(9.))
            .items_start()
            .rounded(px(6.))
            .border_1()
            .border_color(transparent_black())
            .cursor_pointer()
            .map(|this| {
                if active {
                    this.bg(p.surface).border_color(p.border).shadow(ui::edge_shadow(p.border_strong, 1.))
                } else {
                    this.hover(|st| st.bg(p.ink.opacity(0.05)))
                }
            })
            .group(SharedString::from("nav-session"))
            .on_click(cx.listener(move |this, _, _, cx| this.show(Page::Session(ix), cx)))
            .child(div().mt(px(2.)).child(Icon::new(icon).small().text_color(color)))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.))
                    .child(div().truncate().text_size(px(13.)).font_weight(if active { FontWeight::SEMIBOLD } else { FontWeight::MEDIUM }).text_color(p.fg).child(s.title))
                    .child(
                        h_flex()
                            .gap(px(5.))
                            .font_family(theme.mono_font_family.clone())
                            .text_size(px(10.5))
                            .child(div().font_weight(FontWeight::SEMIBOLD).text_color(agent_color).child(agent))
                            .child(div().text_color(p.faint).child("·"))
                            .child(div().min_w_0().truncate().text_color(state_color).child(state)),
                    ),
            )
            // Close: appears on hover; stops the agent process of this window.
            .child(
                div()
                    .id(SharedString::from(format!("nav-session-close:{ix}")))
                    .flex_shrink_0()
                    .size(px(20.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(5.))
                    .opacity(0.)
                    .group_hover("nav-session", |s| s.opacity(1.))
                    .hover(|s| s.bg(p.ink.opacity(0.08)))
                    .cursor_pointer()
                    .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new("Закрыть сессию").build(window, cx))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.close_session(ix, cx);
                    }))
                    .child(Icon::new(IconName::Close).xsmall().text_color(p.muted)),
            )
    }

    fn nav_label(text: &'static str, count: Option<usize>, p: &Palette, theme: &gpui_kit::component::Theme) -> impl IntoElement {
        h_flex()
            .px(px(8.))
            .pt(px(6.))
            .pb(px(6.))
            .justify_between()
            .font_family(theme.mono_font_family.clone())
            .text_size(px(10.5))
            .text_color(p.faint)
            .child(text.to_uppercase())
            .when_some(count, |this, n| this.child(n.to_string()))
    }

    fn render_sidebar(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let p = Palette::of(&theme);
        let waiting = self.tasks.iter().filter(|t| matches!(t.meta.status, TaskStatus::Review | TaskStatus::Blocked)).count();
        let open_tasks = self.tasks.iter().filter(|t| t.meta.status != TaskStatus::Done).count();
        let tasks_count = if waiting > 0 { Some((waiting.to_string(), true)) } else { (open_tasks > 0).then(|| (open_tasks.to_string(), false)) };
        let docs = self.docs.read(cx).count();
        let skills = self.skills_view.read(cx).count().unwrap_or(self.skills.len());
        let live = self.live_sessions(cx);
        let live_count = live.len();
        let limit = crate::settings::config(cx).agents.autopilot_limit;
        let busy = self.autopilot_busy(cx);
        let dark = theme.is_dark();
        let home = {
            let root = self.home.root.display().to_string();
            let h = dirs_home().display().to_string();
            match root.strip_prefix(&h) {
                Some(rest) if !h.is_empty() => format!("~{rest}"),
                _ => root,
            }
        };
        let _ = window;

        v_flex()
            .id("sidebar")
            .relative()
            .w(px(236.))
            .h_full()
            .flex_shrink_0()
            .bg(p.sunken)
            .border_r_1()
            .border_color(p.border)
            // Room for the traffic lights of the transparent title bar.
            .pt(px(38.))
            .child(
                div()
                    .id("titlebar-drag")
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .h(px(36.))
                    .on_mouse_down(MouseButton::Left, |e, window, _| {
                        if e.click_count >= 2 {
                            window.titlebar_double_click();
                        } else {
                            window.start_window_move();
                        }
                    }),
            )
            .px(px(10.))
            .pb(px(12.))
            .gap(px(14.))
            // Brand
            .child(
                h_flex()
                    .px(px(4.))
                    .gap(px(10.))
                    .child(
                        img(crate::icons::LOGO).size(px(34.)).flex_shrink_0(),
                    )
                    .child(
                        v_flex()
                            .gap(px(3.))
                            .child(div().text_size(px(19.)).line_height(px(19.)).font_weight(FontWeight::BOLD).text_color(p.ink).child("Stepan"))
                            .child(ui::mono("AGENT CONTROL", &theme).text_size(px(10.5)).text_color(p.muted)),
                    ),
            )
            .child(
                PressButton::new("new-session")
                    .w_full()
                    .primary()
                    .icon(IconName::Plus)
                    .label("Новая сессия")
                    .on_click(cx.listener(|this, _, window, cx| this.open_new_session_dialog(window, cx))),
            )
            // Pages
            .child(
                v_flex()
                    .gap(px(2.))
                    .child(Self::nav_label("Рабочее", None, &p, &theme))
                    .child(self.nav_item("Дашборд", IconName::LayoutDashboard, Page::Dashboard, None, cx))
                    .child(self.nav_item("Задачи", IconName::Inbox, Page::Tasks, tasks_count, cx))
                    .child(self.nav_item("Документы", IconName::FileText, Page::Docs, (docs > 0).then(|| (docs.to_string(), false)), cx))
                    .child(self.nav_item("Скиллы", IconName::BookOpen, Page::Skills, (skills > 0).then(|| (skills.to_string(), false)), cx))
                    .child(self.nav_item("Настройки", IconName::Settings2, Page::Settings, None, cx)),
            )
            // Open sessions
            .when(live_count > 0, |this| {
                let items: Vec<AnyElement> = live.into_iter().map(|s| self.session_item(s, cx).into_any_element()).collect();
                this.child(
                    v_flex()
                        .id("open-sessions")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .gap(px(2.))
                        .child(Self::nav_label("Открытые сессии", Some(live_count), &p, &theme))
                        .children(items),
                )
            })
            .when(live_count == 0, |this| this.child(div().flex_1()))
            .children(update_card(&p, &theme, cx))
            // Footer: autopilot and theme
            .child(
                h_flex()
                    .px(px(10.))
                    .py(px(9.))
                    .gap(px(8.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(p.border)
                    .bg(p.surface)
                    .child(Icon::new(IconName::Bot).small().text_color(p.muted))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_size(px(12.5)).font_weight(FontWeight::SEMIBOLD).text_color(p.fg).child("Автопилот"))
                            .child(
                                ui::mono(if self.autopilot { format!("занято {busy} из {limit}") } else { "выключен".to_owned() }, &theme)
                                    .text_size(px(10.5))
                                    .text_color(p.muted),
                            ),
                    )
                    .child(Switch::new("sidebar-autopilot").checked(self.autopilot).on_click(cx.listener(|this, on: &bool, _, cx| {
                        this.autopilot = *on;
                        cx.notify();
                    }))),
            )
            .child(
                h_flex()
                    .px(px(4.))
                    .gap_2()
                    .child(ui::mono(home, &theme).flex_1().min_w_0().truncate().text_size(px(10.5)).text_color(p.faint))
                    .child(
                        h_flex()
                            .p(px(2.))
                            .gap(px(2.))
                            .rounded(px(6.))
                            .border_1()
                            .border_color(p.border)
                            .bg(p.surface)
                            .child(theme_button("theme-light", IconName::Sun, !dark, ThemeMode::Light, &p))
                            .child(theme_button("theme-dark", IconName::Moon, dark, ThemeMode::Dark, &p)),
                    ),
            )
    }
}

fn theme_button(id: &'static str, icon: IconName, active: bool, mode: ThemeMode, p: &Palette) -> impl IntoElement {
    div()
        .id(id)
        .size(px(22.))
        .rounded(px(4.))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .when(active, |this| this.bg(p.sunken))
        .hover(|s| s.bg(p.surface2))
        .on_click(move |_, window, cx| gpui_kit::component::Theme::change(mode, Some(window), cx))
        .child(Icon::new(icon).xsmall().text_color(if active { p.fg } else { p.faint }))
}

impl Render for AppView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content: AnyElement = match self.page {
            Page::Dashboard => dashboard::render(self, window, cx).into_any_element(),
            Page::Tasks => crate::board::render(self, window, cx).into_any_element(),
            Page::Docs => self.docs.clone().into_any_element(),
            Page::Skills => self.skills_view.clone().into_any_element(),
            Page::Settings => {
                crate::settings_page::ensure_pickers(self, window, cx);
                crate::settings_page::render(self, window, cx).into_any_element()
            }
            Page::Session(ix) => match self.open_sessions.get(ix) {
                Some(view) => view.clone().into_any_element(),
                None => dashboard::render(self, window, cx).into_any_element(),
            },
        };
        let main = v_flex().size_full().min_w_0().child(content);
        let main: AnyElement = match &self.preview {
            // Session on the left, file on the right; the divider drags.
            Some(preview) => {
                let width = (window.viewport_size().width - px(240.)) * 0.45;
                h_resizable("preview-split")
                    .child(resizable_panel().size_range(px(360.)..Pixels::MAX).child(main))
                    .child(resizable_panel().size(width).size_range(px(320.)..Pixels::MAX).child(preview.clone()))
                    .into_any_element()
            }
            None => main.into_any_element(),
        };
        h_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .font_family(cx.theme().font_family.clone())
            .child(self.render_sidebar(window, cx))
            .child(v_flex().flex_1().min_w_0().h_full().child(main))
    }
}

fn dirs_home() -> PathBuf {
    std::env::var_os("HOME").map(Into::into).unwrap_or_default()
}

/// «Доступна версия …» above the autopilot card, while there is one.
fn update_card(p: &Palette, theme: &gpui_kit::component::Theme, cx: &mut Context<AppView>) -> Option<AnyElement> {
    use crate::update::{self, State};
    let (title, sub, action): (String, String, Option<&'static str>) = match update::state(cx) {
        State::Available(r) => (
            format!("Доступен Stepan {}", r.version),
            format!("у тебя {}", update::VERSION),
            Some(if update::running_bundle().is_some() && r.zip.is_some() { "Обновить" } else { "Скачать" }),
        ),
        State::Installing(r) => (format!("Ставлю Stepan {}…", r.version), "перезапущусь сам".into(), None),
        _ => return None,
    };
    Some(
        h_flex()
            .px(px(10.))
            .py(px(9.))
            .gap(px(8.))
            .rounded(px(8.))
            .border_1()
            .border_color(ui::mix(p.accent.solid, p.border, 0.45))
            .bg(p.accent.soft)
            .child(Icon::new(IconName::ArrowUp).small().text_color(p.accent.text))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().truncate().text_size(px(12.5)).font_weight(FontWeight::SEMIBOLD).text_color(p.fg).child(title))
                    .child(ui::mono(sub, theme).text_size(px(10.5)).text_color(p.muted)),
            )
            .map(|row| match action {
                Some(label) => row.child(
                    PressButton::new("update-install").xsmall().primary().label(label).on_click(|_, _, cx| crate::update::install(cx)),
                ),
                None => row.child(gpui_kit::component::spinner::Spinner::new().xsmall()),
            })
            .into_any_element(),
    )
}
