use std::{path::PathBuf, time::Duration};

use gpui_kit::component::{
    ActiveTheme, IconName,
    button::ButtonVariant, Sizable as _, WindowExt as _,
    notification::Notification,
    resizable::{h_resizable, resizable_panel},
    switch::Switch,
    Icon, ThemeMode,
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use crate::zoom::px;
use crate::ui::PressButton;
use stepan_verify_core::{Board, Home, Provider, SessionSummary, Task as BoardTask, TaskStatus};
use stepan_verify_core::worktree::{self, Cleanup, Isolation};

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
    Flows,
    Docs,
    Memory,
    Skills,
    Settings,
    Session(usize),
    /// The plan of a plan-first task (`AppView::plan_view`).
    Plan,
}

/// Command-line launch options:
/// `stepan_verify [--new] [--open <session-id>] [--start claude|codex --cwd <dir>] [--send <text>]`.
#[derive(Default)]
pub struct Launch {
    open: Option<String>,
    send: Option<String>,
    new: bool,
    /// Open «Новая задача» at start.
    new_task: bool,
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
    /// `--project <dir>`: the project «Флоу» opens on.
    project: Option<PathBuf>,
    /// `--build-flows`: start the flow-map agent for that project right away.
    build_flows: bool,
    /// `--plan <task-id>`: open that task's plan.
    plan: Option<String>,
    sync_flows: bool,
}

impl Launch {
    pub fn from_args(mut args: impl Iterator<Item = String>) -> Self {
        let mut launch = Self::default();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--open" => launch.open = args.next(),
                "--send" => launch.send = args.next(),
                "--new" => launch.new = true,
                "--new-task" => launch.new_task = true,
                "--start" => {
                    launch.start = match args.next().as_deref() {
                        Some("codex") => Some(Provider::Codex),
                        _ => Some(Provider::Claude),
                    }
                }
                "--cwd" => launch.cwd = args.next().map(PathBuf::from),
                "--stay" => launch.stay = true,
                "--changes" => launch.changes = true,
                "--plan" => launch.plan = args.next(),
                "--autopilot" => launch.autopilot = true,
                "--preview" => launch.preview = args.next().map(|p| std::fs::canonicalize(&p).unwrap_or_else(|_| p.into())),
                "--build-flows" => launch.build_flows = true,
                "--sync-flows" => launch.sync_flows = true,
                "--project" => launch.project = args.next().map(|p| std::fs::canonicalize(&p).unwrap_or_else(|_| p.into())),
                "--doc" => {
                    launch.doc = args.next();
                    launch.page = Some(Page::Docs);
                }
                "--page" => {
                    launch.page = match args.next().as_deref() {
                        Some("tasks") => Some(Page::Tasks),
                        Some("flows") => Some(Page::Flows),
                        Some("docs") => Some(Page::Docs),
                        Some("memory") => Some(Page::Memory),
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
    /// A background `rescan` is in flight.
    rescanning: bool,
    /// What was last written to the store as the window's open tabs and page.
    saved_workspace: Option<crate::workspace::Workspace>,
    pub tasks: Vec<BoardTask>,
    pub board: Board,
    /// Task shown in the detail panel of the board.
    pub selected_task: Option<String>,
    /// Pick up ready tasks automatically (up to the configured limit at once).
    pub autopilot: bool,
    /// Tasks whose separate copy is being made; their session starts after.
    pub(crate) preparing: std::collections::HashSet<String>,
    pub skills: Vec<String>,
    docs: Entity<DocsView>,
    flows: Entity<crate::flows::FlowsView>,
    skills_view: Entity<crate::skills::SkillsView>,
    memory_view: Entity<crate::memory::MemoryView>,
    /// Text and code font pickers on the settings page, made on first visit.
    pub(crate) font_pickers: Option<[Entity<crate::settings_page::FontPicker>; 2]>,
    /// Sessions opened in this window, in opening order.
    pub(crate) open_sessions: Vec<Entity<SessionView>>,
    /// The plan on screen: (task id, its map).
    plan_view: Option<(String, Entity<crate::changes::ChangesView>)>,
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

/// Sessions this recent are listed.
const SESSION_AGE: Duration = Duration::from_secs(14 * 86_400);

/// Scan for sessions, through the index when the store is there.
async fn scan(store: Option<stepan_verify_core::Store>) -> Vec<SessionSummary> {
    match store {
        Some(store) => stepan_verify_core::sessions::scan_indexed(&store, SESSION_AGE),
        None => stepan_verify_core::sessions::scan_all(SESSION_AGE),
    }
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
                DocsEvent::SendToSummary { summary, text } => {
                    this.open_session(summary, window, cx);
                    let view = this.open_sessions.iter().find(|s| s.read(cx).native_id.as_deref() == Some(summary.id.as_str())).cloned();
                    if let Some(view) = view {
                        view.update(cx, |v, cx| v.send_text(text.clone(), cx));
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
        let memory_view = cx.new(|cx| crate::memory::MemoryView::new(home.clone(), launch.project.as_deref().and_then(stepan_verify_core::memory::project_of), window, cx));
        cx.subscribe_in(&skills_view, window, |this, _, event: &crate::skills::SkillsEvent, window, cx| match event {
            crate::skills::SkillsEvent::OpenSession(summary) => this.open_session(summary, window, cx),
            crate::skills::SkillsEvent::OpenFile(path) => this.open_preview(path.clone(), None, window, cx),
        })
        .detach();
        let flows = cx.new(|cx| crate::flows::FlowsView::new(home.clone(), launch.project.clone(), window, cx));
        cx.subscribe_in(&flows, window, |this, _, event: &crate::flows::FlowsEvent, window, cx| match event {
            crate::flows::FlowsEvent::OpenSession(id) => {
                if let Some(summary) = this.sessions.iter().find(|s| &s.id == id).cloned() {
                    this.open_session(&summary, window, cx);
                }
            }
            crate::flows::FlowsEvent::LagChanged => cx.notify(),
        })
        .detach();
        if launch.build_flows {
            flows.update(cx, |f, cx| f.build(cx));
        }
        if launch.sync_flows {
            flows.update(cx, |f, cx| f.sync(cx));
        }
        cx.set_global(crate::preview::Opener(cx.entity().downgrade()));
        let mut this = Self {
            tasks: BoardTask::load_all(&home.tasks()),
            board: Board::new(home.clone()),
            selected_task: None,
            autopilot: false,
            preparing: Default::default(),
            skills: home.list_skills(),
            home,
            page: Page::Dashboard,
            sessions: Vec::new(),
            sessions_loading: true,
            rescanning: false,
            saved_workspace: None,
            docs,
            flows,
            font_pickers: None,
            skills_view,
            memory_view,
            open_sessions: Vec::new(),
            plan_view: None,
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
        // Last scan's list right away; the scan below only brings it up to date.
        if let Some(store) = crate::workspace::store(cx) {
            this.sessions = stepan_verify_core::sessions::cached(&store, SESSION_AGE);
        }
        this.restore_workspace(window, cx);
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
        let store = crate::workspace::store(cx);
        self.tasks = self.board.tasks();
        self.skills = self.home.list_skills();
        cx.spawn_in(window, async move |this, cx| {
            let sessions = cx.background_spawn(scan(store)).await;
            this.update_in(cx, |this, window, cx| {
                this.sessions = sessions;
                this.sessions_loading = false;
                let dirs = this.project_dirs();
                this.pickers.retain(|p| p.upgrade().is_some());
                for picker in this.pickers.iter().filter_map(|p| p.upgrade()) {
                    picker.update(cx, |p, cx| p.set_projects(dirs.clone(), cx));
                }
                this.flows.update(cx, |f, cx| f.set_projects(dirs.clone(), cx));
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
        if std::mem::take(&mut self.launch.new_task) {
            self.open_new_task_dialog(window, cx);
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
        if let Some(id) = self.launch.plan.take() {
            self.open_plan(&id, window, cx);
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
                // A `/show-me` view replaces the preview only for the session you look at;
                // elsewhere it waits as a chip in that session's transcript.
                SessionEvent::ShowMe { path, root } if this.page == Page::Session(ix) => this.open_preview(path.clone(), Some(root.clone()), window, cx),
                SessionEvent::ShowMe { .. } => {
                    window.push_notification(Notification::info("Готова визуализация /show-me — открой её в ленте сессии").title(title), cx);
                }
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
        self.persist_workspace(cx);
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

    /// Claim a task for an agent and start a session on it — in a separate
    /// copy of the project when the task asks for one or someone already
    /// works in the project (see `worktree::resolve`).
    pub fn run_task(&mut self, id: &str, provider: Provider, window: &mut Window, cx: &mut Context<Self>) {
        let agent = provider.label().to_lowercase();
        let task = match self.board.claim(id, &agent, LEASE) {
            Ok(task) => task,
            Err(err) => return Self::toast_error(window, err, cx),
        };
        self.reload_tasks(cx);
        match worktree::resolve(&task, &self.tasks, &self.live_cwds(cx)) {
            Isolation::Here => self.start_task_session(task, provider, window, cx),
            Isolation::Worktree => self.prepare_worktree(task, provider, window, cx),
        }
    }

    fn start_task_session(&mut self, task: BoardTask, provider: Provider, window: &mut Window, cx: &mut Context<Self>) {
        let view = cx.new(|cx| SessionView::start_task(provider, &task, window, cx));
        self.adopt(view, window, cx);
        window.push_notification(Notification::info(format!("{} взял задачу", provider.label())).title(task.meta.title.clone()), cx);
        self.reload_tasks(cx);
    }

    /// Folders where agents work right now: open sessions here and live ones outside.
    fn live_cwds(&self, cx: &App) -> Vec<PathBuf> {
        let open = self.open_sessions.iter().map(|v| v.read(cx)).filter(|v| v.status != Status::Exited).map(|v| v.cwd().to_owned());
        open.chain(self.external.iter().filter_map(|e| e.summary.cwd.clone())).collect()
    }

    /// Make the task's copy off the UI thread, then start its session there.
    /// A failure never falls back to the main checkout: the task waits for you.
    fn prepare_worktree(&mut self, task: BoardTask, provider: Provider, window: &mut Window, cx: &mut Context<Self>) {
        let Some(cwd) = task.meta.cwd.clone() else { return self.start_task_session(task, provider, window, cx) };
        let (home, id) = (self.home.clone(), task.id.clone());
        self.preparing.insert(id.clone());
        window.push_notification(Notification::info("Готовлю отдельную копию проекта…").title(task.meta.title.clone()), cx);
        cx.spawn_in(window, async move |this, cx| {
            let made = cx
                .background_spawn({
                    let id = id.clone();
                    async move { worktree::create(&home, &cwd, &id) }
                })
                .await;
            this.update_in(cx, |this, window, cx| this.worktree_ready(&id, provider, made, window, cx)).ok();
        })
        .detach();
        cx.notify();
    }

    fn worktree_ready(&mut self, id: &str, provider: Provider, made: anyhow::Result<worktree::Created>, window: &mut Window, cx: &mut Context<Self>) {
        self.preparing.remove(id);
        let created = match made {
            Ok(created) => created,
            Err(err) => {
                let question = format!("Не получилось сделать отдельную копию: {err:#}. Выбери «Здесь» в задаче или поправь причину — и верни в работу.");
                if let Err(err) = self.board.block(id, "stepan_verify", &question) {
                    eprintln!("stepan_verify: board: {err:#}");
                }
                window.push_notification(Notification::error(format!("Отдельная копия: {err:#}")), cx);
                return self.reload_tasks(cx);
            }
        };
        let text = format!("{} · {}", created.path.display(), created.notes.join(" · "));
        let task = self.board.set_worktree(id, "stepan_verify", Some(created.path.clone()), Some(created.branch.clone()), &text);
        if let Some(failed) = &created.setup_failed {
            let _ = self.board.log(id, "stepan_verify", failed);
            window.push_notification(Notification::warning(failed.clone()).title("Подготовка копии"), cx);
        }
        match task {
            // You may have returned or closed the task while the copy was made.
            Ok(task) if task.meta.status == TaskStatus::InProgress => self.start_task_session(task, provider, window, cx),
            Ok(_) => self.reload_tasks(cx),
            Err(err) => Self::toast_error(window, err, cx),
        }
    }

    pub fn set_task_status(&mut self, id: &str, status: TaskStatus, window: &mut Window, cx: &mut Context<Self>) {
        match self.board.set_status(id, "me", status) {
            Ok(task) if status == TaskStatus::Done && task.meta.worktree.is_some() => self.offer_worktree_cleanup(id, true, window, cx),
            Ok(_) => {}
            Err(err) => Self::toast_error(window, err, cx),
        }
        self.reload_tasks(cx);
    }

    pub fn set_task_model(&mut self, id: &str, provider: Provider, model: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(err) = self.board.set_model(id, "me", provider, model) {
            Self::toast_error(window, err, cx);
        }
        self.reload_tasks(cx);
    }

    pub fn set_task_isolation(&mut self, id: &str, isolation: Option<Isolation>, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(err) = self.board.set_isolation(id, "me", isolation) {
            Self::toast_error(window, err, cx);
        }
        self.reload_tasks(cx);
    }

    /// See what removing the task's copy would lose, then ask. `done`: the
    /// task was just accepted (quiet when the copy is already gone).
    pub fn offer_worktree_cleanup(&mut self, id: &str, done: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Ok(task) = self.board.get(id) else { return };
        cx.spawn_in(window, async move |this, cx| {
            let state = cx
                .background_spawn({
                    let task = task.clone();
                    async move { worktree::cleanup_state(&task) }
                })
                .await;
            this.update_in(cx, |this, window, cx| this.ask_cleanup(task, state, done, window, cx)).ok();
        })
        .detach();
    }

    /// Ask before removing: what goes depends on what the copy holds, and
    /// with uncommitted changes «Оставить» is the safe answer.
    fn ask_cleanup(&mut self, task: BoardTask, state: anyhow::Result<Cleanup>, done: bool, window: &mut Window, cx: &mut Context<Self>) {
        let path = task.meta.worktree.as_deref().map(|p| p.display().to_string()).unwrap_or_default();
        let branch = task.meta.branch.clone().unwrap_or_default();
        let lead = if done { "Задача принята. " } else { "" };
        let (title, text, ok, force, delete_branch) = match state {
            Err(err) => return Self::toast_error(window, err, cx),
            Ok(Cleanup::Gone) => {
                let _ = self.board.set_worktree(&task.id, "me", None, task.meta.branch.clone(), "копии уже нет на диске");
                if !done {
                    window.push_notification(Notification::info("Копии уже нет на диске"), cx);
                }
                return self.reload_tasks(cx);
            }
            Ok(Cleanup::Clean) => (
                "Удалить копию?",
                format!("{lead}В копии нет несохранённых правок, ветка {branch} влита или пустая. Удалим папку {path} и ветку."),
                "Удалить копию",
                false,
                true,
            ),
            Ok(Cleanup::Unmerged(n)) => (
                "Удалить копию, ветку оставить?",
                format!("{lead}В ветке {branch} {n} {} не влиты в основную ветку. Папку {path} удалим, ветка останется в репозитории.", commits_word(n)),
                "Удалить копию",
                false,
                false,
            ),
            Ok(Cleanup::Dirty(n)) => (
                "В копии есть несохранённые правки",
                format!("{lead}{n} {} в {path} изменены или не добавлены в git. Если удалить копию, эти правки пропадут; ветка {branch} останется.", files_word(n)),
                "Удалить с правками",
                true,
                false,
            ),
        };
        let this = cx.entity().downgrade();
        let id = task.id.clone();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (this, id) = (this.clone(), id.clone());
            alert
                .title(title)
                .description(text.clone())
                .confirm()
                .ok_text(ok)
                .ok_variant(if force { ButtonVariant::Danger } else { ButtonVariant::Primary })
                .cancel_text("Оставить")
                .on_ok(move |_, window, cx| {
                    this.update(cx, |this, cx| this.remove_worktree(&id, force, delete_branch, window, cx)).ok();
                    true
                })
        });
    }

    fn remove_worktree(&mut self, id: &str, force: bool, delete_branch: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Ok(task) = self.board.get(id) else { return };
        cx.spawn_in(window, async move |this, cx| {
            let removed = cx
                .background_spawn({
                    let task = task.clone();
                    async move { worktree::remove(&task, force, delete_branch) }
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                let branch = task.meta.branch.clone().filter(|_| !delete_branch);
                let note = match &branch {
                    Some(b) => format!("копия удалена, ветка {b} осталась"),
                    None => "копия и ветка удалены".to_owned(),
                };
                match removed.and_then(|()| this.board.set_worktree(&task.id, "me", None, branch, &note)) {
                    Ok(_) => window.push_notification(Notification::success(note).title(task.meta.title.clone()), cx),
                    Err(err) => Self::toast_error(window, err, cx),
                }
                this.reload_tasks(cx);
            })
            .ok();
        })
        .detach();
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
        match self.resume_task_session(id, window, cx) {
            Some(ix) => self.show(Page::Session(ix), cx),
            None => window.push_notification(Notification::warning("Сессия этой задачи не найдена — обнови список сессий"), cx),
        }
    }

    /// Open the task's last linked session in the background; its index in `open_sessions`.
    fn resume_task_session(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) -> Option<usize> {
        let linked = self.tasks.iter().find(|t| t.id == id).and_then(|t| t.meta.sessions.last().cloned());
        let mut summary = linked
            .and_then(|s| s.split_once(':').map(|(_, native)| native.to_owned()))
            .and_then(|native| self.sessions.iter().find(|s| s.id == native).cloned())?;
        // A worktree task resumes in its copy, wherever the index thought it ran.
        if let Some(cwd) = self.tasks.iter().find(|t| t.id == id).filter(|t| t.meta.worktree.is_some()).and_then(worktree::session_cwd) {
            summary.cwd = Some(cwd);
        }
        let existing = self.open_sessions.iter().position(|s| s.read(cx).native_id.as_deref() == Some(summary.id.as_str()));
        let ix = match existing {
            Some(ix) => ix,
            None => {
                let view = cx.new(|cx| SessionView::resume(&summary, window, cx));
                self.adopt(view, window, cx)
            }
        };
        self.open_sessions[ix].update(cx, |v, _| v.task_id = Some(id.to_owned()));
        Some(ix)
    }

    // ------------------------------------------------------------- plans

    pub fn set_task_plan(&mut self, id: &str, plan: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(err) = self.board.set_plan(id, "me", plan) {
            Self::toast_error(window, err, cx);
        }
        self.reload_tasks(cx);
    }

    /// «Открыть план»: the task's plan map, drawn like «Изменения».
    pub fn open_plan(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.plan_view.as_ref().is_some_and(|(t, _)| t == id) {
            return self.show(Page::Plan, cx);
        }
        let Some(task) = self.tasks.iter().find(|t| t.id == id).cloned() else { return };
        let provider = self.task_provider(&task, cx);
        let input = crate::changes::ChangesInput {
            key: task.id.clone(),
            provider: match provider {
                Provider::Claude => stepan_verify_agents::Provider::Claude,
                Provider::Codex => stepan_verify_agents::Provider::Codex,
            },
            title: task.meta.title.clone(),
            cwd: worktree::session_cwd(&task).unwrap_or_else(dirs_home),
            requests: Vec::new(),
            final_answer: String::new(),
            patches: Vec::new(),
            commands: Vec::new(),
            screenshots: Vec::new(),
            log: Vec::new(),
            plan: None,
        };
        let task_id = task.id.clone();
        let view = cx.new(|cx| crate::changes::ChangesView::plan(input, task_id.clone(), cx));
        cx.subscribe_in(&view, window, move |this, _, event: &crate::changes::ChangesEvent, window, cx| {
            use crate::changes::ChangesEvent;
            match event {
                ChangesEvent::Return(text) => this.send_plan_remarks(&task_id, text.clone(), window, cx),
                ChangesEvent::ApprovePlan(n) => this.approve_plan(&task_id, *n, window, cx),
                ChangesEvent::OpenSession => this.open_task_session(&task_id, window, cx),
                ChangesEvent::Accept => {}
            }
        })
        .detach();
        self.plan_view = Some((task.id, view));
        self.show(Page::Plan, cx);
    }

    /// The agent that works on a task: its open session, its last one, else the default.
    fn task_provider(&self, task: &BoardTask, cx: &App) -> Provider {
        let last = match task.meta.sessions.last().and_then(|s| s.split_once(':')).map(|(who, _)| who) {
            Some("codex") => Some(Provider::Codex),
            Some("claude") => Some(Provider::Claude),
            _ => None,
        };
        self.task_session(&task.id, cx)
            .map(|(_, v)| v.read(cx).provider)
            .or(last)
            .unwrap_or_else(|| crate::settings::config(cx).agents.default_provider.into())
    }

    /// Remarks on the plan go to the agent that writes it (resumed if closed);
    /// it rewrites the plan as a new iteration.
    fn send_plan_remarks(&mut self, id: &str, text: String, window: &mut Window, cx: &mut Context<Self>) {
        let ix = match self.task_session(id, cx) {
            Some((ix, _)) => Some(ix),
            None => self.resume_task_session(id, window, cx),
        };
        match ix {
            Some(ix) => {
                self.open_sessions[ix].update(cx, |v, cx| v.send_text(text, cx));
                window.push_notification(Notification::info("Замечания ушли агенту — он перепишет план новой итерацией"), cx);
            }
            None => window.push_notification(Notification::warning("Сессия, где писался план, не найдена — обнови список сессий и отправь ещё раз"), cx),
        }
        self.reload_tasks(cx);
    }

    /// «Утвердить план»: the task moves to implementation and an agent starts on it.
    pub fn approve_plan(&mut self, id: &str, run: usize, window: &mut Window, cx: &mut Context<Self>) {
        let task = match self.board.approve_plan(id, "me", run) {
            Ok(task) => task,
            Err(err) => return Self::toast_error(window, err, cx),
        };
        let provider = self.task_provider(&task, cx);
        window.push_notification(Notification::success(format!("План утверждён (итерация #{run}) — {} делает его по этапам", provider.label())).title(task.meta.title.clone()), cx);
        self.reload_tasks(cx);
        self.run_task(id, provider, window, cx);
    }

    /// Keep the board in step with what the agent does in the session.
    fn on_task_session_event(&mut self, view: &Entity<SessionView>, event: &SessionEvent, cx: &mut Context<Self>) {
        let (task_id, agent, activity) = {
            let v = view.read(cx);
            let Some(id) = v.task_id.clone() else { return };
            (id, v.provider.label().to_lowercase(), v.activity())
        };
        // A plan-first task has a planning and an implementation session; only
        // the newest open one moves the board.
        if self.task_session(&task_id, cx).is_some_and(|(_, newest)| newest != view) {
            return;
        }
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
        // A copy being prepared (a long `pnpm install`) keeps its claim.
        for id in &self.preparing {
            if let Some(by) = self.tasks.iter().find(|t| &t.id == id).and_then(|t| t.meta.claimed_by.clone()) {
                let _ = self.board.renew(id, &by, LEASE);
            }
        }
        self.board.release_expired();
        self.tasks = self.board.tasks();
        if self.autopilot {
            self.autopilot_step(window, cx);
        }
        self.rescan(window, cx);
        self.persist_workspace(cx);
        if self.page == Page::Dashboard {
            self.refresh_external(cx);
        }
        cx.notify();
    }

    /// Pick up sessions started elsewhere since the last scan (a new `claude`
    /// in a terminal) without re-reading every transcript.
    fn rescan(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.rescanning || self.sessions_loading {
            return;
        }
        self.rescanning = true;
        let store = crate::workspace::store(cx);
        cx.spawn_in(window, async move |this, cx| {
            let sessions = cx.background_spawn(scan(store)).await;
            this.update_in(cx, |this, _, cx| {
                this.rescanning = false;
                if this.sessions_loading {
                    return; // a full scan is on its way
                }
                let before = this.project_dirs();
                this.sessions = sessions;
                let dirs = this.project_dirs();
                if dirs != before {
                    this.pickers.retain(|p| p.upgrade().is_some());
                    for picker in this.pickers.iter().filter_map(|p| p.upgrade()) {
                        picker.update(cx, |p, cx| p.set_projects(dirs.clone(), cx));
                    }
                    this.flows.update(cx, |f, cx| f.set_projects(dirs.clone(), cx));
                }
                if this.page == Page::Dashboard {
                    this.refresh_external(cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
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
        if page != Page::Settings {
            crate::corner::silence(cx);
        }
        // The docs reader may hold a native web view: it must hide off-page.
        self.docs.update(cx, |d, cx| d.set_visible(page == Page::Docs, cx));
        if page == Page::Skills {
            self.skills_view.update(cx, |v, cx| v.refresh_if_stale(cx));
        }
        self.persist_workspace(cx);
        cx.notify();
    }

    /// Open tabs and the page as they are now.
    fn workspace(&self, cx: &App) -> crate::workspace::Workspace {
        use crate::workspace::{OpenTab, Workspace};
        let (page, session) = match self.page {
            Page::Dashboard => ("dashboard", None),
            Page::Tasks => ("tasks", None),
            Page::Flows => ("flows", None),
            Page::Docs => ("docs", None),
            Page::Memory => ("memory", None),
            Page::Skills => ("skills", None),
            Page::Settings => ("settings", None),
            Page::Session(ix) => ("session", Some(ix)),
            Page::Plan => ("tasks", None),
        };
        let mut open = Vec::new();
        let mut session_at = None;
        for (ix, view) in self.open_sessions.iter().enumerate() {
            let v = view.read(cx);
            // A session that has no id yet cannot be resumed.
            let Some(id) = v.native_id.clone() else { continue };
            if session == Some(ix) {
                session_at = Some(open.len());
            }
            open.push(OpenTab {
                provider: v.provider,
                id,
                title: v.title.to_string(),
                cwd: Some(v.cwd().to_owned()),
                task_id: v.task_id.clone(),
                changes: v.showing_changes(),
            });
        }
        let page = if page == "session" && session_at.is_none() { "dashboard" } else { page };
        Workspace { page: page.into(), session: session_at, open, doc: self.docs.read(cx).selected().map(str::to_owned) }
    }

    /// Write the open tabs and page to the store when they changed.
    fn persist_workspace(&mut self, cx: &App) {
        let now = self.workspace(cx);
        if self.saved_workspace.as_ref() != Some(&now) {
            crate::workspace::save(&now, cx);
            self.saved_workspace = Some(now);
        }
    }

    /// Reopen what was open last time: session tabs (with last time's
    /// transcript until the agent re-reads it), the «Изменения» tab, the page.
    fn restore_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let saved = crate::workspace::load(cx);
        for tab in &saved.open {
            let summary = self.sessions.iter().find(|s| s.provider == tab.provider && s.id == tab.id).cloned().or_else(|| {
                let file = match tab.provider {
                    Provider::Claude => stepan_verify_core::sessions::claude_transcript(&tab.id)?,
                    Provider::Codex => PathBuf::new(),
                };
                Some(SessionSummary {
                    provider: tab.provider,
                    id: tab.id.clone(),
                    title: tab.title.clone(),
                    cwd: tab.cwd.clone(),
                    updated_at: chrono::Utc::now(),
                    file,
                })
            });
            // Its transcript is gone: nothing to reopen.
            let Some(summary) = summary else { continue };
            self.open_session(&summary, window, cx);
            if let Some(view) = self.open_sessions.last() {
                view.update(cx, |v, cx| {
                    if v.task_id.is_none() {
                        v.task_id = tab.task_id.clone();
                    }
                    v.show_snapshot(cx);
                    if tab.changes {
                        v.open_changes(cx);
                    }
                });
            }
        }
        if let Some(rel) = saved.doc.filter(|_| self.launch.doc.is_none()) {
            self.docs.update(cx, |d, _| d.open_on_load(rel));
        }
        let page = match (saved.page.as_str(), saved.session) {
            ("session", Some(i)) if i < self.open_sessions.len() => Page::Session(i),
            ("tasks", _) => Page::Tasks,
            ("flows", _) => Page::Flows,
            ("docs", _) => Page::Docs,
            ("memory", _) => Page::Memory,
            ("skills", _) => Page::Skills,
            ("settings", _) => Page::Settings,
            _ => Page::Dashboard,
        };
        // An explicit `--page` / `--doc` wins over last time.
        self.show(self.launch.page.unwrap_or(page), cx);
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
            // A session in a worktree (a task's copy) stands for its main checkout.
            let cwd = &worktree::project_root(cwd);
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
            + self.preparing.len()
    }

    fn nav_item(&self, label: &'static str, icon: impl Into<Icon>, page: Page, count: Option<(String, bool)>, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let p = Palette::of(&theme);
        // A task's plan lives under «Задачи».
        let active = self.page == page || (page == Page::Tasks && self.page == Page::Plan);
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
        let memories = self.memory_view.read(cx).count();
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
                    .child(self.nav_item("Флоу", Icon::empty().path("icons/route.svg"), Page::Flows, self.flows.read(cx).lag_badge().map(|n| (n, true)), cx))
                    .child(self.nav_item("Документы", IconName::FileText, Page::Docs, (docs > 0).then(|| (docs.to_string(), false)), cx))
                    .child(self.nav_item("Память", Icon::empty().path("icons/brain.svg"), Page::Memory, (memories > 0).then(|| (memories.to_string(), false)), cx))
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
            Page::Flows => self.flows.clone().into_any_element(),
            Page::Memory => self.memory_view.clone().into_any_element(),
            Page::Skills => self.skills_view.clone().into_any_element(),
            Page::Settings => {
                crate::settings_page::ensure_pickers(self, window, cx);
                crate::settings_page::render(self, window, cx).into_any_element()
            }
            Page::Session(ix) => match self.open_sessions.get(ix) {
                Some(view) => view.clone().into_any_element(),
                None => dashboard::render(self, window, cx).into_any_element(),
            },
            Page::Plan => match &self.plan_view {
                Some((_, view)) => view.clone().into_any_element(),
                None => crate::board::render(self, window, cx).into_any_element(),
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
            format!("Stepan {}", r.version),
            format!("обновление · у тебя {}", update::VERSION),
            Some(if update::running_bundle().is_some() && r.zip.is_some() { "Обновить" } else { "Скачать" }),
        ),
        State::Installing(r) => (format!("Stepan {}", r.version), "ставлю, перезапущусь сам".into(), None),
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

fn commits_word(n: usize) -> &'static str {
    match (n % 10, n % 100) {
        (1, r) if r != 11 => "коммит",
        (2..=4, r) if !(12..=14).contains(&r) => "коммита",
        _ => "коммитов",
    }
}

fn files_word(n: usize) -> &'static str {
    match (n % 10, n % 100) {
        (1, r) if r != 11 => "файл",
        (2..=4, r) if !(12..=14).contains(&r) => "файла",
        _ => "файлов",
    }
}
