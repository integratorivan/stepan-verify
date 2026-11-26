use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    time::{Duration, Instant, SystemTime},
};

use gpui_kit::component::{
    ActiveTheme, Icon, IconName, Sizable, Theme,
    input::{InputEvent, Textarea, TextareaState},
    message_scroller::{MessageScroller, MessageScrollerState},
    spinner::Spinner,
    text::TextView,
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use crate::zoom::px;
use crate::ui::PressButton;
use stepan_verify_agents::{AgentEvent, AgentHandle, Approval, Attachment, AttachmentKind, FileDiff, Item, ItemKind, ItemStatus, SessionSpec};
use stepan_verify_core::{Provider, SessionSummary, Task as BoardTask};

use crate::attach::{self, Attachments, AttachmentsEvent, image_cache};
use crate::ui::{self, Palette, Tone};

/// Diff lines shown before "show all".
const DIFF_PREVIEW_LINES: usize = 14;
const DIFF_MAX_LINES: usize = 400;
/// The conversation column, like a document page.
const FEED_WIDTH: f32 = 720.;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Loading,
    Idle,
    Running,
    Exited,
}

/// What the session is doing, for the dashboard.
pub struct Activity {
    pub status: Status,
    pub approvals: usize,
    /// Current step while running, last answer while idle.
    pub line: String,
    /// The agent's process died mid-turn.
    pub interrupted: bool,
}

pub enum SessionEvent {
    Close,
    /// A turn started (you sent a message or the agent resumed).
    Busy,
    NeedsApproval(String),
    TurnFinished { error: Option<String> },
    /// Show a file or folder in the preview panel; `root` is the session's project.
    OpenFile { path: PathBuf, root: PathBuf },
    /// The agent just made a `/show-me` HTML view: show it if you are looking at this session.
    ShowMe { path: PathBuf, root: PathBuf },
}

impl EventEmitter<SessionEvent> for SessionView {}

/// A transcript row: one item, or a run of tool calls folded together.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Row {
    Item(usize),
    Group { start: usize, end: usize },
}

/// One agent conversation: transcript, live stream, approvals, composer.
pub struct SessionView {
    pub provider: Provider,
    pub title: SharedString,
    pub project: SharedString,
    pub native_id: Option<String>,
    /// Board task this session works on.
    pub task_id: Option<String>,
    pub status: Status,
    /// Replaying the transcript file of a session that runs elsewhere (a
    /// terminal); the first message sent from here takes it over.
    pub following: bool,
    /// Last time the other process wrote to the transcript.
    external_at: Option<SystemTime>,
    cwd: PathBuf,
    handle: AgentHandle,
    items: Vec<Item>,
    index: HashMap<String, usize>,
    rows: Vec<Row>,
    /// Item ids of expanded tool outputs, full diffs and groups (keyed by first item).
    expanded: HashSet<String>,
    approvals: Vec<Approval>,
    error: Option<String>,
    /// Messages written while the agent works: they go when its turn ends.
    queue: Vec<stepan_verify_agents::Outgoing>,
    /// «Отправить сейчас»: goes as soon as the interrupted turn stops.
    send_now: Option<stepan_verify_agents::Outgoing>,
    _send_now_timer: Task<()>,
    /// The agent's process died mid-turn; where it stopped.
    interrupted: Option<String>,
    scroller: Entity<MessageScrollerState>,
    composer: Entity<TextareaState>,
    /// Images attached to the next message (paste, drop, screenshot, «+»).
    attachments: Entity<Attachments>,
    /// When the current turn started, for the elapsed timer.
    turn_started: Option<Instant>,
    /// Re-renders once a second while a turn runs.
    _ticker: Task<()>,
    /// «Изменения» report, created the first time you open the tab.
    changes: Option<Entity<crate::changes::ChangesView>>,
    show_changes: bool,
    /// item id → (first seen, finished) for live items; history has no timing.
    timing: HashMap<String, (Instant, Option<Instant>)>,
    /// `/show-me` HTML files already handed to the preview, so each opens once.
    shown: HashSet<PathBuf>,
    _events: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl SessionView {
    pub fn resume(summary: &SessionSummary, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let spec = SessionSpec {
            provider: agent_provider(summary.provider),
            native_id: Some(summary.id.clone()),
            transcript: (summary.provider == Provider::Claude).then(|| summary.file.clone()),
            cwd: summary.cwd.clone().unwrap_or_else(home_dir),
            mode: crate::settings::agent_mode(cx),
            image_cache: Some(image_cache()),
            env: Vec::new(),
            model: None,
            instructions: docs_brief(summary.provider, summary.cwd.as_deref(), Some(&summary.id), cx),
        };
        // Claude history comes from the same file either way, so always follow;
        // a Codex rollout carries less than `thread/read`, so only while it is fresh.
        let written = std::fs::metadata(&summary.file).and_then(|m| m.modified()).ok();
        // Our own host still runs it (a window before this one started it): talk to it, not watch the file.
        let hosted = stepan_verify_agents::host::running(agent_provider(summary.provider), &summary.id);
        let follow = !hosted && match summary.provider {
            Provider::Claude => true,
            Provider::Codex => written.is_some_and(|t| t.elapsed().unwrap_or_default() < Duration::from_secs(30 * 60)),
        };
        let mut this = Self::new(summary.provider, summary.title.clone(), summary.project(), spec, follow.then(|| summary.file.clone()), window, cx);
        this.external_at = written;
        this
    }

    /// Start a fresh session in `cwd` and send `prompt` (with `images`) as the first message.
    pub fn start(
        provider: Provider,
        cwd: PathBuf,
        prompt: String,
        images: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let project = stepan_verify_core::worktree::project_name(&cwd).unwrap_or_default();
        let title = first_line(&prompt, 70);
        let spec = SessionSpec {
            provider: agent_provider(provider),
            native_id: None,
            transcript: None,
            instructions: docs_brief(provider, Some(cwd.as_path()), None, cx),
            cwd,
            mode: crate::settings::agent_mode(cx),
            image_cache: Some(image_cache()),
            env: Vec::new(),
            model: crate::models::settings_default(provider, cx),
        };
        let mut this = Self::new(provider, title, project, spec, None, window, cx);
        this.send_message(stepan_verify_agents::Outgoing { text: prompt, images }, cx);
        this
    }

    /// Work on a board task: the agent gets the task as its first message and
    /// `STEPAN_VERIFY_TASK` / `STEPAN_VERIFY_AGENT` plus `stepan_verify` on PATH, so it can log and complete.
    /// Runs in the task's own copy when it has one.
    pub fn start_task(provider: Provider, task: &BoardTask, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let cwd = stepan_verify_core::worktree::session_cwd(task).unwrap_or_else(home_dir);
        let project = task.project_name().unwrap_or_default();
        let mut env = vec![
            ("STEPAN_VERIFY_TASK".to_owned(), task.id.clone()),
            ("STEPAN_VERIFY_AGENT".to_owned(), provider.label().to_lowercase()),
        ];
        if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.to_owned())) {
            let path = std::env::var("PATH").unwrap_or_default();
            env.push(("PATH".to_owned(), format!("{}:{path}", dir.display())));
        }
        let spec = SessionSpec {
            provider: agent_provider(provider),
            native_id: None,
            transcript: None,
            instructions: docs_brief(provider, Some(cwd.as_path()), None, cx),
            cwd,
            mode: crate::settings::agent_mode(cx),
            image_cache: Some(image_cache()),
            env,
            model: crate::models::for_task(task, provider, cx),
        };
        // A planning agent writes its plan here (see `changes::plan`).
        if task.phase() == Some(stepan_verify_core::Phase::Plan) {
            let _ = std::fs::create_dir_all(stepan_verify_core::plans::draft(&stepan_verify_core::Home::locate(), &task.id));
        }
        let mut this = Self::new(provider, task.meta.title.clone(), project, spec, None, window, cx);
        this.task_id = Some(task.id.clone());
        // Attached images go with the message; other files are listed in it.
        let images = task.meta.attachments.iter().filter(|p| stepan_verify_agents::is_image_path(p)).cloned().collect();
        this.send_message(stepan_verify_agents::Outgoing { text: task_prompt(task), images }, cx);
        this
    }

    fn new(
        provider: Provider,
        title: String,
        project: String,
        spec: SessionSpec,
        follow: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut spec = spec;
        // Memory written from a worktree belongs to the main project, not to the task id folder.
        if !spec.env.iter().any(|(k, _)| k == "STEPAN_VERIFY_PROJECT") {
            if let Some(name) = docs_project(Some(&spec.cwd)) {
                spec.env.push(("STEPAN_VERIFY_PROJECT".to_owned(), name));
            }
        }
        let native_id = spec.native_id.clone();
        let cwd = spec.cwd.clone();
        let following = follow.is_some();
        let (handle, events) = match follow {
            Some(file) => stepan_verify_agents::open_following(spec, file),
            None => stepan_verify_agents::open(spec),
        };

        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 10)
                .submit_on_enter(true)
                .placeholder("Сообщение агенту… (Enter — отправить, Shift+Enter — новая строка)")
        });
        let submit = cx.subscribe_in(&composer, window, |this, _, event, window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                this.submit(window, cx);
            }
        });
        let attachments = cx.new(|_| Attachments::new("composer-attachment", true));
        let attached = cx.subscribe_in(&attachments, window, |this, _, event, window, cx| match event {
            AttachmentsEvent::NotImages(paths) => this.mention_paths(paths, window, cx),
            AttachmentsEvent::Changed => cx.notify(),
        });

        let scroller = cx.new(|cx| MessageScrollerState::new(0, cx));

        // Drain agent events in batches so a burst of deltas costs one render.
        let pump = cx.spawn(async move |this, cx| {
            while let Ok(first) = events.recv().await {
                let mut batch = vec![first];
                while let Ok(more) = events.try_recv() {
                    batch.push(more);
                }
                let alive = this.update(cx, |this, cx| this.apply_batch(batch, cx));
                if alive.is_err() {
                    break;
                }
            }
        });

        Self {
            provider,
            title: title.into(),
            project: project.into(),
            native_id,
            task_id: None,
            status: Status::Loading,
            following,
            external_at: None,
            cwd,
            handle,
            items: Vec::new(),
            index: HashMap::new(),
            rows: Vec::new(),
            expanded: HashSet::new(),
            approvals: Vec::new(),
            error: None,
            queue: Vec::new(),
            send_now: None,
            _send_now_timer: Task::ready(()),
            interrupted: None,
            scroller,
            composer,
            attachments,
            turn_started: None,
            _ticker: Task::ready(()),
            changes: None,
            show_changes: false,
            timing: HashMap::new(),
            shown: HashSet::new(),
            _events: pump,
            _subscriptions: vec![submit, attached],
        }
    }

    /// Steps for the dashboard card.
    pub fn card(&self) -> crate::live_card::Card {
        let approvals: Vec<String> = self.approvals.iter().map(|a| a.title.clone()).collect();
        let timing = |id: &str| {
            self.timing.get(id).map(|(start, end)| end.unwrap_or_else(Instant::now).duration_since(*start).as_secs())
        };
        crate::live_card::build(&self.items, self.status == Status::Running, &approvals, &timing)
    }

    /// Answer the oldest pending approval (from the dashboard card).
    pub fn decide_first(&mut self, allow: bool) {
        if let Some(a) = self.approvals.first() {
            self.handle.approve(a.id.clone(), allow);
        }
    }

    /// Seconds since the current turn started.
    pub fn turn_elapsed(&self) -> Option<u64> {
        self.turn_started.map(|t| t.elapsed().as_secs())
    }

    pub fn interrupt(&self) {
        if !self.following {
            self.handle.interrupt();
        }
    }

    /// The other process is mid-turn: messages from here would race it.
    pub fn busy_elsewhere(&self) -> bool {
        self.following && self.status == Status::Running
    }

    /// Show the «runs in a terminal» notice: following and written lately.
    fn live_elsewhere(&self) -> bool {
        self.following
            && (self.status == Status::Running
                || self.external_at.is_some_and(|t| t.elapsed().unwrap_or_default() < Duration::from_secs(10 * 60)))
    }

    pub fn activity(&self) -> Activity {
        let line = match self.status {
            Status::Running => self
                .items
                .iter()
                .rev()
                .find(|i| i.status == ItemStatus::Running)
                .map(|i| match i.kind {
                    ItemKind::Assistant => "Пишет ответ…".to_owned(),
                    _ => i.title.clone(),
                })
                .unwrap_or_else(|| "Думает…".into()),
            _ if self.interrupted.is_some() => format!("Прервана: {}", self.interrupted.as_deref().unwrap_or_default()),
            _ => self
                .items
                .iter()
                .rev()
                .find(|i| i.kind == ItemKind::Assistant)
                .map(|i| first_line(&strip_markdown(&i.body), 140))
                .unwrap_or_default(),
        };
        Activity { status: self.status, approvals: self.approvals.len(), line, interrupted: self.interrupted.is_some() }
    }

    fn apply_batch(&mut self, batch: Vec<AgentEvent>, cx: &mut Context<Self>) {
        let mut dirty: HashSet<usize> = HashSet::new();
        let mut reset = false;
        for event in batch {
            self.apply(event, &mut dirty, &mut reset, cx);
        }
        self.sync_rows(reset, &dirty, cx);
        cx.notify();
    }

    /// A finished tool call that produced a fresh `/show-me` HTML file opens it
    /// in the preview (once); older files (a replayed transcript) only stay as chips.
    fn reveal_show_me(&mut self, item: &Item, cx: &mut Context<Self>) {
        let fresh = |p: &std::path::Path| {
            std::fs::metadata(p).and_then(|m| m.modified()).is_ok_and(|t| t.elapsed().is_ok_and(|age| age < Duration::from_secs(120)))
        };
        let html = item.attachments.iter().map(|a| &a.path).find(|p| {
            stepan_verify_agents::is_show_me(p)
                && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("html") || e.eq_ignore_ascii_case("htm"))
                && !self.shown.contains(*p)
                && fresh(p)
        });
        if let Some(path) = html.cloned() {
            self.shown.insert(path.clone());
            cx.emit(SessionEvent::ShowMe { path, root: self.cwd.clone() });
        }
    }

    fn apply(&mut self, event: AgentEvent, dirty: &mut HashSet<usize>, reset: &mut bool, cx: &mut Context<Self>) {
        match event {
            AgentEvent::History(items) => {
                self.index = items.iter().enumerate().map(|(i, it)| (it.id.clone(), i)).collect();
                self.items = items;
                *reset = true;
                if self.status == Status::Loading {
                    self.status = Status::Idle;
                }
                if self.queue.is_empty() {
                    self.queue = self.load_queue(cx);
                }
                // Debug hook: STEPAN_VERIFY_DEBUG_QUEUE="first|second" puts messages in the queue.
                if let (true, Ok(texts)) = (self.queue.is_empty(), std::env::var("STEPAN_VERIFY_DEBUG_QUEUE")) {
                    self.queue = texts.split('|').map(|t| stepan_verify_agents::Outgoing { text: t.to_owned(), images: Vec::new() }).collect();
                }
                // «Изменения» was requested before the transcript was here.
                if self.show_changes && self.changes.is_none() {
                    self.open_changes(cx);
                }
                self.save_snapshot(cx);
            }
            AgentEvent::SessionId(id) => {
                self.native_id = Some(id);
                self.save_queue(cx);
            }
            AgentEvent::Interrupted { step } => self.interrupted = Some(step),
            AgentEvent::TurnStarted => {
                self.interrupted = None;
                if let Some(key) = &self.native_id {
                    crate::distill::turn_started(key, cx);
                }
                self.begin_turn(cx);
                self.error = None;
            }
            AgentEvent::Upsert(item) => {
                if self.following {
                    self.external_at = Some(SystemTime::now());
                }
                // Step timing for the dashboard card: first seen → first not running.
                let now = Instant::now();
                let timing = self.timing.entry(item.id.clone()).or_insert((now, None));
                if item.status != ItemStatus::Running && timing.1.is_none() {
                    timing.1 = Some(now);
                }
                if !self.following && item.status == ItemStatus::Done {
                    self.reveal_show_me(&item, cx);
                }
                match self.index.get(&item.id).copied() {
                    Some(ix) => {
                        self.items[ix] = item;
                        dirty.insert(ix);
                    }
                    None => {
                        self.index.insert(item.id.clone(), self.items.len());
                        self.items.push(item);
                    }
                }
            }
            AgentEvent::Append { id, text } => {
                if let Some(&ix) = self.index.get(&id) {
                    self.items[ix].body.push_str(&text);
                    dirty.insert(ix);
                }
            }
            AgentEvent::ApprovalRequested(approval) => {
                cx.emit(SessionEvent::NeedsApproval(approval.title.clone()));
                self.approvals.push(approval);
            }
            AgentEvent::ApprovalResolved(id) => self.approvals.retain(|a| a.id != id),
            AgentEvent::TurnCompleted { error } => {
                self.status = Status::Idle;
                self.turn_started = None;
                self.approvals.clear();
                for (ix, item) in self.items.iter_mut().enumerate() {
                    if item.status == ItemStatus::Running {
                        item.status = ItemStatus::Done;
                        dirty.insert(ix);
                    }
                }
                cx.emit(SessionEvent::TurnFinished { error: error.clone() });
                self.error = error;
                self.changes_progressed(cx);
                if self.native_id.is_some() {
                    crate::distill::turn_finished(self.changes_input(), docs_project(Some(self.cwd.as_path())), cx);
                }
                self.save_snapshot(cx);
                self.flush_queue(cx);
            }
            AgentEvent::Error(err) => {
                self.error = Some(err);
                if self.status == Status::Loading {
                    self.status = Status::Idle;
                }
            }
            AgentEvent::Exited => {
                self.status = Status::Exited;
                self.turn_started = None;
            }
        }
    }

    /// Rebuild rows and patch the virtual list: only the tail changes while
    /// streaming, so splice from the first differing row.
    fn sync_rows(&mut self, reset: bool, dirty: &HashSet<usize>, cx: &mut Context<Self>) {
        let rows = build_rows(&self.items);
        if reset {
            let len = rows.len();
            self.rows = rows;
            self.scroller.update(cx, |s, cx| {
                s.reset(len, cx);
                s.scroll_to_end(cx);
            });
            return;
        }
        let common = self.rows.iter().zip(&rows).take_while(|(a, b)| a == b).count();
        let changed_rows: Vec<usize> = rows
            .iter()
            .enumerate()
            .take(common)
            .filter(|(_, row)| row_items(**row).any(|ix| dirty.contains(&ix)))
            .map(|(r, _)| r)
            .collect();
        let (old_len, new_len) = (self.rows.len(), rows.len());
        self.rows = rows;
        self.scroller.update(cx, |s, cx| {
            if common < old_len || common < new_len {
                s.splice(common..old_len, new_len - common, cx);
            }
            for r in changed_rows {
                s.remeasure_items(r..r + 1, cx);
            }
        });
    }

    fn remeasure_row_of(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(&ix) = self.index.get(key) else { return };
        if let Some(r) = self.rows.iter().position(|row| row_items(*row).any(|i| i == ix)) {
            self.scroller.update(cx, |s, cx| {
                s.remeasure_items(r..r + 1, cx);
            });
        }
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).value().trim().to_owned();
        if (text.is_empty() && self.attachments.read(cx).is_empty()) || self.status == Status::Exited || self.busy_elsewhere() {
            return;
        }
        self.composer.update(cx, |c, cx| c.set_value("", window, cx));
        let images = self.attachments.update(cx, |a, cx| a.take(cx));
        stepan_verify_core::usage::hit("sessions.send");
        let message = stepan_verify_agents::Outgoing { text, images };
        // The agent is mid-turn: the message waits for its turn to end.
        if self.status == Status::Running && !self.following {
            stepan_verify_core::usage::hit("sessions.queue");
            self.queue.push(message);
            self.save_queue(cx);
            cx.notify();
            return;
        }
        self.send_message(message, cx);
    }

    /// The turn ended: what you pressed «Отправить сейчас» on, else everything queued as one message.
    fn flush_queue(&mut self, cx: &mut Context<Self>) {
        if self.status == Status::Exited {
            return;
        }
        if let Some(message) = self.send_now.take() {
            self._send_now_timer = Task::ready(());
            self.send_message(message, cx);
            return;
        }
        if self.queue.is_empty() {
            return;
        }
        let queued = std::mem::take(&mut self.queue);
        let text = queued.iter().map(|m| m.text.trim()).filter(|t| !t.is_empty()).collect::<Vec<_>>().join("\n\n");
        let images = queued.into_iter().flat_map(|m| m.images).collect();
        self.save_queue(cx);
        self.send_message(stepan_verify_agents::Outgoing { text, images }, cx);
    }

    /// «Отправить сейчас»: stop the current step and send this one first.
    fn send_queued_now(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix >= self.queue.len() {
            return;
        }
        let message = self.queue.remove(ix);
        self.save_queue(cx);
        stepan_verify_core::usage::hit("sessions.send_now");
        if self.status != Status::Running {
            self.send_message(message, cx);
            return;
        }
        self.send_now = Some(message);
        self.handle.interrupt();
        // An agent that does not stop in time still gets the message.
        self._send_now_timer = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(8)).await;
            let _ = this.update(cx, |this, cx| {
                if let Some(message) = this.send_now.take() {
                    this.send_message(message, cx);
                }
            });
        });
        cx.notify();
    }

    fn drop_queued(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix < self.queue.len() {
            self.queue.remove(ix);
            self.save_queue(cx);
            cx.notify();
        }
    }

    /// The queue outlives the window: kept in the store under the session id.
    fn queue_key(&self) -> Option<String> {
        self.native_id.as_ref().map(|id| format!("queue:{id}"))
    }

    fn save_queue(&self, cx: &mut Context<Self>) {
        let (Some(key), Some(store)) = (self.queue_key(), crate::workspace::store(cx)) else { return };
        let rows: Vec<(String, Vec<PathBuf>)> = self.queue.iter().map(|m| (m.text.clone(), m.images.clone())).collect();
        let _ = store.put(&key, &rows);
    }

    fn load_queue(&self, cx: &mut Context<Self>) -> Vec<stepan_verify_agents::Outgoing> {
        let (Some(key), Some(store)) = (self.queue_key(), crate::workspace::store(cx)) else { return Vec::new() };
        store
            .get::<Vec<(String, Vec<PathBuf>)>>(&key)
            .unwrap_or_default()
            .into_iter()
            .map(|(text, images)| stepan_verify_agents::Outgoing { text, images })
            .collect()
    }

    /// Queue a message; the agent picks it up after history has loaded.
    pub fn send_text(&mut self, text: String, cx: &mut Context<Self>) {
        self.send_message(stepan_verify_agents::Outgoing { text, images: Vec::new() }, cx);
    }

    fn send_message(&mut self, message: stepan_verify_agents::Outgoing, cx: &mut Context<Self>) {
        if self.busy_elsewhere() {
            return;
        }
        // From here on the session runs in stepan_verify.
        self.following = false;
        self.interrupted = None;
        self.handle.send_message(message);
        self.begin_turn(cx);
        self.scroller.update(cx, |s, cx| s.scroll_to_end(cx));
        cx.notify();
    }

    /// Images go to the attachment strip; anything else into the text as a
    /// path, which both agents can open themselves.
    fn attach_paths(&mut self, paths: &[PathBuf], _: &mut Window, cx: &mut Context<Self>) {
        self.attachments.update(cx, |a, cx| a.add(paths, cx));
    }

    fn mention_paths(&mut self, paths: &[PathBuf], window: &mut Window, cx: &mut Context<Self>) {
        self.composer.update(cx, |c, cx| {
            let text = attach::append_mentions(&c.value(), paths);
            c.set_value(text, window, cx);
        });
    }

    /// Mark the session busy and tick the elapsed timer until the turn ends.
    fn begin_turn(&mut self, cx: &mut Context<Self>) {
        self.status = Status::Running;
        if self.turn_started.is_some() {
            return;
        }
        cx.emit(SessionEvent::Busy);
        self.turn_started = Some(Instant::now());
        // «Было» of the before/after pass: the work tree before the agent's first turn here.
        if let Some(key) = self.native_id.clone().or_else(|| self.task_id.clone()) {
            crate::changes::media::turn_started(&key, &self.cwd);
        }
        self._ticker = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let running = this.update(cx, |this, cx| {
                    cx.notify();
                    this.status == Status::Running
                });
                if !matches!(running, Ok(true)) {
                    break;
                }
            }
        });
    }

    fn toggle(&mut self, key: &str, cx: &mut Context<Self>) {
        if !self.expanded.remove(key) {
            self.expanded.insert(key.to_owned());
        }
        self.remeasure_row_of(key.split('#').next().unwrap_or(key), cx);
        cx.notify();
    }

    // ------------------------------------------------------------- rendering

    /// Notice above the composer while the session runs in another process.
    fn render_elsewhere(&self, theme: &Theme) -> impl IntoElement {
        let p = Palette::of(theme);
        let (text, note) = if self.status == Status::Running {
            ("Агент работает в терминале", "шаги появляются здесь сами · ответить можно после конца хода")
        } else {
            ("Сессия запущена в терминале", "сообщение отсюда продолжит её в Stepan · в терминале её лучше закрыть")
        };
        h_flex()
            .gap_2()
            .items_center()
            .px(px(10.))
            .py(px(6.))
            .rounded(px(6.))
            .bg(p.blue.soft)
            .child(ui::dot(p.blue.solid))
            .child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).text_color(p.blue.text).child(text))
            .child(ui::mono(note, theme).text_color(p.muted).truncate())
    }

    fn render_row(&self, r: usize, weak: &WeakEntity<Self>, cx: &App) -> AnyElement {
        let row = match self.rows.get(r).copied() {
            Some(Row::Item(ix)) => self.render_item(ix, weak, cx),
            Some(Row::Group { start, end }) => self.render_group(start, end, weak, cx),
            None => return div().into_any_element(),
        };
        // The conversation reads as one centered column.
        h_flex().w_full().px_6().justify_center().child(div().w_full().max_w(px(FEED_WIDTH)).child(row)).into_any_element()
    }

    /// Wall time of a run of items: first start to last end (or now).
    fn span_secs(&self, items: &[Item]) -> Option<u64> {
        let spans: Vec<&(Instant, Option<Instant>)> = items.iter().filter_map(|i| self.timing.get(&i.id)).collect();
        let start = spans.iter().map(|(s, _)| *s).min()?;
        let end = spans.iter().map(|(_, e)| e.unwrap_or_else(Instant::now)).max()?;
        Some(end.saturating_duration_since(start).as_secs())
    }

    fn render_group(&self, start: usize, end: usize, weak: &WeakEntity<Self>, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let p = Palette::of(theme);
        let key = self.items[start].id.clone();
        let open = self.expanded.contains(&key);
        let tools = self.items[start..end].iter().filter(|i| i.kind == ItemKind::Tool).count();
        let failed = self.items[start..end].iter().filter(|i| i.status == ItemStatus::Failed).count();
        let running = self.items[start..end].iter().rev().find(|i| i.status == ItemStatus::Running);
        let latest = running
            .or_else(|| self.items[start..end].iter().rev().find(|i| i.kind == ItemKind::Tool))
            .map(|i| self.relative_title(&i.title))
            .unwrap_or_default();
        let secs = self.span_secs(&self.items[start..end]);

        let toggle_key = key.clone();
        let weak_toggle = weak.clone();
        v_flex()
            .id(SharedString::from(format!("group:{key}")))
            .w_full()
            .gap_2()
            .child(
                h_flex()
                    .id(SharedString::from(format!("group-toggle:{key}")))
                    .w_full()
                    .px_1()
                    .py_1()
                    .gap_2()
                    .items_center()
                    .rounded(px(6.))
                    .text_size(px(13.))
                    .font_weight(FontWeight::BOLD)
                    .text_color(p.fg)
                    .cursor_pointer()
                    .hover(|s| s.bg(p.sunken))
                    .on_click(move |_, _, cx| {
                        if let Some(view) = weak_toggle.upgrade() {
                            view.update(cx, |this, cx| this.toggle(&toggle_key, cx));
                        }
                    })
                    .child(if running.is_some() {
                        Spinner::new().xsmall().color(p.blue.solid).into_any_element()
                    } else {
                        Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight })
                            .xsmall()
                            .text_color(p.muted)
                            .into_any_element()
                    })
                    .child(div().flex_shrink_0().child(format!("{tools} {}", actions_word(tools))))
                    .when(failed > 0, |this| {
                        this.child(div().flex_shrink_0().font_weight(FontWeight::MEDIUM).text_color(p.red.text).child(format!("· {failed} с ошибкой")))
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .when(!open, |d| d.child(ui::mono(latest, theme).truncate().font_weight(FontWeight::NORMAL).text_color(p.faint))),
                    )
                    .when_some(secs, |this, s| this.child(ui::mono(mmss(s), theme).font_weight(FontWeight::NORMAL).text_color(p.faint))),
            )
            .when(open, |this| {
                this.child(
                    v_flex()
                        .ml(px(9.))
                        .pl(px(14.))
                        .gap_2()
                        .border_l_1()
                        .border_color(p.border)
                        .children((start..end).map(|ix| self.render_item(ix, weak, cx))),
                )
            })
            .into_any_element()
    }

    fn render_item(&self, ix: usize, weak: &WeakEntity<Self>, cx: &App) -> AnyElement {
        let Some(item) = self.items.get(ix) else { return div().into_any_element() };
        let theme = cx.theme();
        let p = Palette::of(theme);
        let row_id = SharedString::from(format!("item:{}", item.id));
        let markdown = |item: &Item| {
            TextView::markdown(SharedString::from(format!("md:{}", item.id)), item.body.clone()).selectable(true)
        };
        match item.kind {
            ItemKind::User => v_flex()
                .id(row_id)
                .w_full()
                .items_end()
                .gap(px(6.))
                .child(ui::mono("ты", theme).text_color(p.faint))
                .when(!item.attachments.is_empty(), |this| {
                    this.child(h_flex().w_full().justify_end().child(render_attachments(&item.id, &item.attachments, weak, theme)))
                })
                .when(!item.body.is_empty(), |this| {
                    this.child(
                        div()
                            .max_w(relative(0.85))
                            .px(px(14.))
                            .py(px(10.))
                            .rounded(px(12.))
                            .border_1()
                            .border_color(p.border)
                            .bg(p.surface)
                            .shadow(ui::card_shadow())
                            .text_size(px(14.))
                            .text_color(p.fg)
                            .child(markdown(item)),
                    )
                })
                .into_any_element(),
            ItemKind::Assistant => {
                // The agent's name opens each answer to your message.
                let opens_turn = ix == 0 || self.items.get(ix - 1).is_some_and(|prev| prev.kind == ItemKind::User);
                v_flex()
                    .id(row_id)
                    .w_full()
                    .gap(px(6.))
                    .when(opens_turn, |this| {
                        this.child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(ui::provider_chip(self.provider, &p, theme))
                                .when(item.status == ItemStatus::Running, |d| d.child(Spinner::new().xsmall().color(p.blue.solid))),
                        )
                    })
                    .child(div().w_full().text_size(px(14.)).text_color(p.fg).child(markdown(item)))
                    .into_any_element()
            }
            ItemKind::Reasoning => {
                const PREVIEW: usize = 280;
                let long = item.body.chars().count() > PREVIEW;
                let open = self.expanded.contains(&item.id);
                let text = if long && !open { format!("{}…", item.body.chars().take(PREVIEW).collect::<String>()) } else { item.body.clone() };
                let (id, weak) = (item.id.clone(), weak.clone());
                v_flex()
                    .id(row_id)
                    .w_full()
                    .pl_3()
                    .gap_1()
                    .border_l_2()
                    .border_color(p.border)
                    .text_size(px(12.5))
                    .text_color(p.muted)
                    .child(div().italic().child(text))
                    .when(long, |this| {
                        this.child(
                            div()
                                .id(SharedString::from(format!("think-more:{}", item.id)))
                                .font_family(theme.mono_font_family.clone())
                                .text_size(px(11.5))
                                .text_color(p.faint)
                                .cursor_pointer()
                                .hover(|s| s.text_color(p.ink))
                                .on_click(move |_, _, cx| {
                                    if let Some(view) = weak.upgrade() {
                                        view.update(cx, |this, cx| this.toggle(&id, cx));
                                    }
                                })
                                .child(if open { "свернуть" } else { "показать размышления целиком" }),
                        )
                    })
                    .into_any_element()
            }
            ItemKind::Notice => h_flex()
                .id(row_id)
                .w_full()
                .gap_3()
                .items_center()
                .child(div().flex_1().h(px(1.)).bg(p.border))
                .child(ui::mono(item.body.clone(), theme).text_color(p.faint))
                .child(div().flex_1().h(px(1.)).bg(p.border))
                .into_any_element(),
            ItemKind::Plan => render_plan(row_id, &item.body, &p, theme),
            ItemKind::Tool => self.render_tool(item, row_id, weak, theme),
        }
    }

    fn render_tool(&self, item: &Item, row_id: SharedString, weak: &WeakEntity<Self>, theme: &Theme) -> AnyElement {
        let p = Palette::of(theme);
        let open = self.expanded.contains(&item.id);
        let has_output = !item.body.trim().is_empty();
        let card = v_flex()
            .id(row_id)
            .w_full()
            .rounded(px(8.))
            .border_1()
            .border_color(if item.status == ItemStatus::Failed { ui::mix(p.red.solid, p.border, 0.45) } else { p.border })
            .bg(p.surface)
            .overflow_hidden();

        // An edit is its diff: the diff header names the file and the counts.
        if !item.diffs.is_empty() {
            return card
                .children(item.diffs.iter().enumerate().map(|(n, diff)| self.render_diff(&item.id, n, diff, weak, theme)))
                .when(item.status == ItemStatus::Failed && has_output, |this| {
                    this.child(
                        div()
                            .px(px(10.))
                            .py(px(6.))
                            .border_t_1()
                            .border_color(p.border)
                            .bg(p.red.soft)
                            .font_family(theme.mono_font_family.clone())
                            .text_size(px(11.5))
                            .text_color(p.red.text)
                            .child(first_line(&item.body, 300)),
                    )
                })
                .into_any_element();
        }

        let icon: AnyElement = match item.status {
            ItemStatus::Running => Spinner::new().xsmall().color(p.blue.solid).into_any_element(),
            ItemStatus::Done => Icon::new(IconName::CircleCheck).xsmall().text_color(p.faint).into_any_element(),
            ItemStatus::Failed => Icon::new(IconName::CircleX).xsmall().text_color(p.red.solid).into_any_element(),
        };
        let id = item.id.clone();
        let weak_toggle = weak.clone();
        card.child(
            h_flex()
                .id(SharedString::from(format!("toggle:{}", item.id)))
                .px(px(10.))
                .py(px(6.))
                .gap_2()
                .items_center()
                .font_family(theme.mono_font_family.clone())
                .text_size(px(12.))
                .text_color(p.fg)
                .when(item.status == ItemStatus::Running, |d| d.bg(p.surface2))
                .when(has_output, |this| {
                    this.cursor_pointer().hover(|s| s.bg(p.surface2)).on_click(move |_, _, cx| {
                        if let Some(view) = weak_toggle.upgrade() {
                            view.update(cx, |this, cx| this.toggle(&id, cx));
                        }
                    })
                })
                .child(icon)
                .child(div().flex_1().min_w_0().truncate().child(self.relative_title(&item.title)))
                .when_some(self.timing.get(&item.id).filter(|_| item.status != ItemStatus::Running), |this, (s, e)| {
                    let secs = e.unwrap_or_else(Instant::now).saturating_duration_since(*s).as_secs();
                    this.child(div().flex_shrink_0().text_size(px(11.5)).text_color(p.faint).child(mmss(secs)))
                })
                .when(has_output, |this| {
                    this.child(Icon::new(if open { IconName::ChevronUp } else { IconName::ChevronDown }).xsmall().text_color(p.faint))
                }),
        )
        .when(!item.attachments.is_empty(), |this| {
            this.child(
                div()
                    .px(px(10.))
                    .py_2()
                    .border_t_1()
                    .border_color(p.border)
                    .child(render_attachments(&item.id, &item.attachments, weak, theme)),
            )
        })
        .when(open && has_output, |this| {
            this.child(
                div()
                    .px(px(12.))
                    .py(px(8.))
                    .border_t_1()
                    .border_color(p.border)
                    .bg(p.surface2)
                    .max_h(px(360.))
                    .overflow_hidden()
                    .font_family(theme.mono_font_family.clone())
                    .text_size(px(11.5))
                    .text_color(p.muted)
                    .child(item.body.clone()),
            )
        })
        .into_any_element()
    }

    /// Tool titles carry absolute paths; show them relative to the project.
    fn relative_title(&self, title: &str) -> String {
        let mut out = title.to_owned();
        let mut roots = vec![self.cwd.clone()];
        if let Ok(canonical) = self.cwd.canonicalize() {
            roots.push(canonical);
        }
        for root in roots {
            let prefix = format!("{}/", root.display());
            if prefix.len() > 1 {
                out = out.replace(&prefix, "");
            }
        }
        out
    }

    fn render_diff(&self, item_id: &str, n: usize, diff: &FileDiff, weak: &WeakEntity<Self>, theme: &Theme) -> AnyElement {
        let p = Palette::of(theme);
        let key = format!("{item_id}#diff{n}");
        let full = self.expanded.contains(&key);
        let lines: Vec<&str> = diff
            .patch
            .lines()
            .filter(|l| !l.starts_with("---") && !l.starts_with("+++") && !l.starts_with("\\ No newline"))
            .collect();
        let total = lines.len();
        let limit = if full { DIFF_MAX_LINES } else { DIFF_PREVIEW_LINES };
        let hidden = total.saturating_sub(limit);
        let (added, removed) = diff.stats();
        let weak_toggle = weak.clone();
        let abs = {
            let path = std::path::Path::new(&diff.path);
            if path.is_absolute() { path.to_owned() } else { self.cwd.join(path) }
        };
        let weak_open = weak.clone();

        // Line numbers follow the hunk headers: `@@ -old,n +new,m @@`.
        let (mut old_n, mut new_n) = (0usize, 0usize);
        let mut rows = Vec::new();
        for (i, line) in lines.into_iter().take(limit).enumerate() {
            let text = line.replace('\t', "    ");
            let (sign, body) = match text.chars().next() {
                Some(c @ ('+' | '-' | ' ')) => (Some(c), text[1..].to_owned()),
                _ => (None, text.clone()),
            };
            let (old, new, bg, sign_color) = if text.starts_with("@@") {
                let mut parts = text.split_whitespace().skip(1);
                let start = |s: Option<&str>| s.and_then(|s| s[1..].split(',').next()?.parse::<usize>().ok()).unwrap_or(0);
                old_n = start(parts.next());
                new_n = start(parts.next());
                (String::new(), String::new(), Some(p.blue.soft), p.blue.text)
            } else {
                match sign {
                    Some('+') => {
                        new_n += 1;
                        (String::new(), (new_n - 1).to_string(), Some(p.green.soft), p.green.text)
                    }
                    Some('-') => {
                        old_n += 1;
                        ((old_n - 1).to_string(), String::new(), Some(p.red.soft), p.red.text)
                    }
                    _ => {
                        old_n += 1;
                        new_n += 1;
                        ((old_n - 1).to_string(), (new_n - 1).to_string(), None, p.faint)
                    }
                }
            };
            let hunk = text.starts_with("@@");
            rows.push(
                h_flex()
                    .id(SharedString::from(format!("{key}:{i}")))
                    .w_full()
                    .when_some(bg, |this, bg| this.bg(bg))
                    .child(div().w(px(34.)).flex_shrink_0().pr(px(6.)).text_right().text_color(p.faint).child(old))
                    .child(div().w(px(34.)).flex_shrink_0().pr(px(6.)).text_right().text_color(p.faint).child(new))
                    .child(
                        div()
                            .w(px(14.))
                            .flex_shrink_0()
                            .text_color(sign_color)
                            .child(match sign {
                                Some('+') => "+",
                                Some('-') => "-",
                                _ => " ",
                            }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .pr_2()
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_color(if hunk { p.blue.text } else { p.fg })
                            .child(SharedString::from(if hunk { text.clone() } else if body.is_empty() { " ".to_owned() } else { body })),
                    ),
            );
        }

        v_flex()
            .w_full()
            .when(n > 0, |this| this.border_t_1().border_color(p.border))
            .font_family(theme.mono_font_family.clone())
            .text_size(px(11.5))
            .child(
                h_flex()
                    .px(px(10.))
                    .py(px(6.))
                    .gap_2()
                    .items_center()
                    .bg(p.surface2)
                    .border_b_1()
                    .border_color(p.border)
                    .child(Icon::new(IconName::FileText).xsmall().text_color(p.muted))
                    .child(div().min_w_0().truncate().text_size(px(12.)).font_weight(FontWeight::MEDIUM).text_color(p.ink).child(display_path(&diff.path, &self.cwd)))
                    .child(div().flex_shrink_0().text_color(p.green.text).child(format!("+{added}")))
                    .child(div().flex_shrink_0().text_color(p.red.text).child(format!("−{removed}")))
                    .child(div().flex_1())
                    .child(
                        h_flex()
                            .id(SharedString::from(format!("{key}:open")))
                            .flex_shrink_0()
                            .gap_1()
                            .items_center()
                            .px(px(6.))
                            .rounded(px(4.))
                            .font_family(theme.font_family.clone())
                            .text_size(px(12.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(p.muted)
                            .cursor_pointer()
                            .hover(|s| s.bg(p.sunken).text_color(p.ink))
                            .on_click(move |_, _, cx| open_path(&weak_open, &abs, cx))
                            .child(Icon::new(IconName::Eye).xsmall())
                            .child("Открыть"),
                    ),
            )
            .child(v_flex().py(px(4.)).children(rows))
            .when(hidden > 0 || full, |this| {
                let label = if full { "свернуть".to_owned() } else { format!("показать все {total} строк") };
                this.child(
                    div()
                        .id(SharedString::from(format!("{key}:more")))
                        .px(px(10.))
                        .py(px(6.))
                        .border_t_1()
                        .border_color(p.border)
                        .text_color(p.muted)
                        .cursor_pointer()
                        .hover(|s| s.bg(p.surface2).text_color(p.ink))
                        .on_click(move |_, _, cx| {
                            if let Some(view) = weak_toggle.upgrade() {
                                view.update(cx, |this, cx| this.toggle(&key, cx));
                            }
                        })
                        .child(label),
                )
            })
            .into_any_element()
    }

    /// «Диалог | Изменения». The report is built only when you open it.
    fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let p = Palette::of(&theme);
        let files = self.changed_files().len();
        let tab = |id: &'static str, icon: IconName, label: &'static str, count: Option<usize>, active: bool| {
            h_flex()
                .id(id)
                .gap(px(6.))
                .items_center()
                .pt(px(2.))
                .pb(px(9.))
                .border_b_2()
                .border_color(if active { p.accent.solid } else { transparent_black() })
                .text_size(px(13.5))
                .text_color(if active { p.ink } else { p.muted })
                .when(active, |d| d.font_weight(FontWeight::BOLD))
                .cursor_pointer()
                .hover(|s| s.text_color(p.ink))
                .child(Icon::new(icon).xsmall())
                .child(label)
                .when_some(count, |d, n| d.child(ui::chip(n.to_string(), &p, &theme).font_weight(FontWeight::NORMAL)))
        };
        h_flex()
            .gap_5()
            .child(tab("tab-chat", IconName::Bot, "Диалог", None, !self.show_changes).on_click(cx.listener(|this, _, _, cx| {
                this.show_changes = false;
                cx.notify();
            })))
            .child(
                tab("tab-changes", IconName::Network, "Изменения", (files > 0).then_some(files), self.show_changes)
                    .tooltip(|window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new("Отчёт об изменениях: схема зависимостей, UI было/стало, риски").build(window, cx)
                    })
                    .on_click(cx.listener(|this, _, _, cx| this.open_changes(cx))),
            )
    }

    /// Files edited in this session, in first-touched order.
    fn changed_files(&self) -> Vec<&str> {
        let mut files: Vec<&str> = Vec::new();
        for d in self.items.iter().flat_map(|i| &i.diffs) {
            if !files.contains(&d.path.as_str()) {
                files.push(&d.path);
            }
        }
        files
    }

    pub fn cwd(&self) -> &std::path::Path {
        &self.cwd
    }

    /// Whether «Изменения» is the tab in front.
    pub fn showing_changes(&self) -> bool {
        self.show_changes
    }

    /// Keep this transcript for the next start (see `workspace`).
    fn save_snapshot(&self, cx: &App) {
        if let (Some(id), false) = (&self.native_id, self.items.is_empty()) {
            crate::workspace::save_snapshot(self.provider, id, &self.items, cx);
        }
    }

    /// Show the transcript saved last time until the real one arrives, so a
    /// restored tab is not empty while a long transcript is re-read.
    pub fn show_snapshot(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.native_id.clone() else { return };
        let load = crate::workspace::load_snapshot(self.provider, &id, cx);
        cx.spawn(async move |this, cx| {
            let Some(items) = load.await else { return };
            this.update(cx, |this, cx| {
                if this.status != Status::Loading || !this.items.is_empty() {
                    return; // the real transcript won
                }
                this.index = items.iter().enumerate().map(|(i, it)| (it.id.clone(), i)).collect();
                this.items = items;
                this.sync_rows(true, &HashSet::new(), cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The report needs the transcript: while history is still loading, the tab
    /// shows a placeholder and the report starts once history arrives.
    pub fn open_changes(&mut self, cx: &mut Context<Self>) {
        self.show_changes = true;
        if self.changes.is_none() && self.status != Status::Loading {
            self.make_changes(cx);
        }
        cx.notify();
    }

    /// The «Изменения» view, wired back to this session: its remarks go to the agent.
    fn make_changes(&mut self, cx: &mut Context<Self>) -> Entity<crate::changes::ChangesView> {
        let input = self.changes_input();
        let view = cx.new(|cx| crate::changes::ChangesView::new(input, cx));
        cx.subscribe(&view, |this, _, event: &crate::changes::ChangesEvent, cx| match event {
            crate::changes::ChangesEvent::Return(text) => this.send_text(text.clone(), cx),
            // TODO: a session tied to a board task could complete the task here.
            crate::changes::ChangesEvent::Accept | crate::changes::ChangesEvent::ApprovePlan(_) | crate::changes::ChangesEvent::OpenSession => {}
        })
        .detach();
        self.changes = Some(view.clone());
        view
    }

    /// A turn ended: a watched map gets the new part of the session as a batch,
    /// and UI edits get a before/after pass (pr-before-after).
    fn changes_progressed(&mut self, cx: &mut Context<Self>) {
        if self.native_id.is_some() || self.task_id.is_some() {
            crate::changes::media::turn_finished(self.changes_input(), cx);
        }
        let view = match &self.changes {
            Some(view) => view.clone(),
            None => {
                let key = self.native_id.clone().or_else(|| self.task_id.clone());
                if !key.is_some_and(|k| crate::changes::follows(&k)) {
                    return;
                }
                self.make_changes(cx)
            }
        };
        let input = self.changes_input();
        view.update(cx, |v, cx| v.session_progressed(input, cx));
    }

    fn changes_input(&self) -> crate::changes::ChangesInput {
        let text = |kind: ItemKind| self.items.iter().filter(move |i| i.kind == kind).map(|i| i.body.clone());
        crate::changes::ChangesInput {
            provider: agent_provider(self.provider),
            key: self
                .native_id
                .clone()
                .or_else(|| self.task_id.clone())
                .unwrap_or_else(|| format!("unsaved-{}", std::process::id())),
            title: self.title.to_string(),
            cwd: self.cwd.clone(),
            requests: text(ItemKind::User).filter(|t| !t.trim().is_empty()).collect(),
            final_answer: text(ItemKind::Assistant).last().unwrap_or_default(),
            patches: self.items.iter().flat_map(|i| &i.diffs).map(|d| (d.path.clone(), d.patch.clone())).collect(),
            commands: self
                .items
                .iter()
                .filter(|i| i.kind == ItemKind::Tool && i.title.starts_with("$ "))
                .map(|i| i.title.trim_start_matches("$ ").to_owned())
                .collect(),
            screenshots: self
                .items
                .iter()
                .flat_map(|i| &i.attachments)
                .filter(|a| a.kind == AttachmentKind::Image)
                .map(|a| a.path.clone())
                .collect(),
            log: self
                .items
                .iter()
                .flat_map(|i| {
                    use crate::changes::LogItem;
                    let mut out = Vec::new();
                    match i.kind {
                        ItemKind::User if !i.body.trim().is_empty() => out.push(LogItem::Request(i.body.clone())),
                        ItemKind::Assistant if !i.body.trim().is_empty() => out.push(LogItem::Answer(i.body.clone())),
                        ItemKind::Tool if i.title.starts_with("$ ") => out.push(LogItem::Command(i.title.trim_start_matches("$ ").to_owned())),
                        _ => {}
                    }
                    out.extend(i.diffs.iter().map(|d| LogItem::Edit { path: d.path.clone(), patch: d.patch.clone() }));
                    out
                })
                .collect(),
            plan: self.task_id.as_deref().and_then(approved_plan),
        }
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let p = Palette::of(&theme);
        let (label, tone, spin) = match (self.status, self.approvals.is_empty()) {
            (_, false) => ("ждёт подтверждения", Some(p.red), false),
            (Status::Loading, _) => ("загрузка", None, true),
            (Status::Idle, _) if self.interrupted.is_some() => ("прервана", Some(p.red), false),
            (Status::Idle, _) => ("ждёт тебя", Some(p.amber), false),
            (Status::Running, _) => ("работает", Some(p.blue), true),
            (Status::Exited, _) => ("процесс завершён", Some(p.red), false),
        };
        let session = self.native_id.as_deref().map(|id| id.chars().take(4).collect::<String>()).unwrap_or_else(|| "новая".into());
        let crumbs = format!("Дашборд / {} / сессия {session}", self.project);
        v_flex()
            .flex_shrink_0()
            .px_6()
            .pt(px(12.))
            .gap(px(4.))
            .border_b_1()
            .border_color(p.border)
            .child(
                h_flex()
                    .gap_2()
                    .child(ui::mono(crumbs, &theme).min_w_0().truncate().text_color(p.faint))
                    .child(div().flex_1())
                    .when_some(self.task_id.clone(), |this, task| this.child(ui::mono(format!("задача: {task}"), &theme).text_color(p.faint))),
            )
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(20.))
                            .line_height(px(28.))
                            .font_weight(FontWeight::BOLD)
                            .text_color(p.ink)
                            .child(self.title.clone()),
                    )
                    .child(ui::provider_chip(self.provider, &p, &theme))
                    .child(div().flex_1())
                    .child(status_pill(label, tone, spin, &p))
                    .child(
                        PressButton::new("close-session")
                            .xsmall()
                            .ghost()
                            .icon(IconName::Close)
                            .tooltip("Закрыть сессию (процесс агента остановится)")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(SessionEvent::Close))),
                    ),
            )
            .child(div().pt(px(6.)).child(self.render_tabs(cx)))
    }

    /// The agent's process died mid-step: say so and offer to go on.
    fn render_interrupted(&self, step: String, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::of(theme);
        let prompt = if step.is_empty() {
            "Продолжай с того места, где остановился: твой процесс прервался посреди хода.".to_owned()
        } else {
            format!("Продолжай с того места, где остановился: твой процесс прервался на шаге «{step}». Проверь, успел ли он выполниться, и иди дальше.")
        };
        h_flex()
            .gap_3()
            .items_center()
            .px(px(12.))
            .py(px(9.))
            .rounded(px(9.))
            .border_1()
            .border_color(ui::mix(p.amber.solid, p.border, 0.5))
            .bg(p.amber.soft)
            .child(Icon::new(IconName::TriangleAlert).small().text_color(p.amber.text))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.))
                    .child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(p.amber.text).child("Ход оборвался"))
                    .child(div().text_size(px(12.5)).text_color(p.fg).child(if step.is_empty() {
                        "Процесс агента завершился посреди хода. Что он успел сделать, осталось на диске.".to_owned()
                    } else {
                        format!("Процесс агента завершился на шаге «{step}». Что он успел сделать, осталось на диске.")
                    })),
            )
            .child(PressButton::new("resume-interrupted").small().primary().icon(IconName::ArrowUp).label("Продолжить").on_click(cx.listener(
                move |this, _, _, cx| {
                    stepan_verify_core::usage::hit("sessions.resume_interrupted");
                    this.send_text(prompt.clone(), cx);
                },
            )))
    }

    /// Messages waiting for the turn to end, each with «Отправить сейчас».
    fn render_queue(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::of(theme);
        v_flex()
            .gap(px(6.))
            .child(
                h_flex()
                    .gap_2()
                    .child(ui::eyebrow(format!("в очереди · {}", self.queue.len()), &p, theme))
                    .child(div().text_size(px(12.)).text_color(p.faint).child(if self.status == Status::Running {
                        "уйдёт агенту, когда он закончит ход"
                    } else {
                        "агент свободен: «Отправить сейчас» отправит сразу"
                    })),
            )
            .children(self.queue.iter().enumerate().map(|(ix, m)| {
                let text = first_line(&m.text, 160);
                let images = m.images.len();
                h_flex()
                    .id(("queued", ix))
                    .gap_2()
                    .items_center()
                    .px(px(12.))
                    .py(px(7.))
                    .rounded(px(9.))
                    .border_1()
                    .border_dashed()
                    .border_color(p.border_strong)
                    .bg(p.sunken)
                    .child(div().flex_1().min_w_0().truncate().text_size(px(13.)).text_color(p.fg).child(if text.is_empty() { "картинка".to_owned() } else { text }))
                    .when(images > 0, |d| d.child(ui::mono(format!("+{images} img"), theme).text_color(p.faint)))
                    .child(
                        PressButton::new(("send-now", ix))
                            .xsmall()
                            .outline()
                            .icon(IconName::ArrowUp)
                            .label("Отправить сейчас")
                            .tooltip("Остановит текущий шаг агента и отправит это сообщение сразу")
                            .on_click(cx.listener(move |this, _, _, cx| this.send_queued_now(ix, cx))),
                    )
                    .child(
                        PressButton::new(("drop-queued", ix))
                            .xsmall()
                            .ghost()
                            .icon(IconName::Close)
                            .tooltip("Убрать из очереди")
                            .on_click(cx.listener(move |this, _, _, cx| this.drop_queued(ix, cx))),
                    )
            }))
    }

    fn render_approvals(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let p = Palette::of(&theme);
        v_flex().gap_2().children(self.approvals.iter().map(|a| {
            let (allow_id, deny_id) = (a.id.clone(), a.id.clone());
            let command = a.title.strip_prefix("$ ").map(str::to_owned);
            let kind = if command.is_some() { "команда" } else { "действие" };
            let line = command.clone().unwrap_or_else(|| self.relative_title(&a.title));
            v_flex()
                .id(SharedString::from(format!("approval:{}", a.id)))
                .p(px(14.))
                .gap(px(10.))
                .rounded(px(12.))
                .border_1()
                .border_color(ui::mix(p.red.solid, p.border, 0.45))
                .bg(ui::mix(p.red.soft, p.surface, 0.45))
                .shadow(ui::card_shadow())
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(Icon::new(IconName::TriangleAlert).small().text_color(p.red.solid))
                        .child(div().text_size(px(14.)).font_weight(FontWeight::BOLD).text_color(p.ink).child("Агент просит разрешение"))
                        .child(div().flex_1())
                        .child(ui::eyebrow(kind, &p, &theme)),
                )
                .child(
                    h_flex()
                        .px(px(10.))
                        .py(px(8.))
                        .gap_2()
                        .items_start()
                        .rounded(px(6.))
                        .border_1()
                        .border_color(p.border)
                        .bg(p.surface)
                        .font_family(theme.mono_font_family.clone())
                        .text_size(px(12.5))
                        .text_color(p.ink)
                        .when(command.is_some(), |d| d.child(div().text_color(p.faint).child("$")))
                        .child(div().flex_1().min_w_0().child(line)),
                )
                .when(!a.detail.is_empty(), |this| this.child(div().text_size(px(13.)).text_color(p.muted).child(a.detail.clone())))
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            PressButton::new(SharedString::from(format!("allow:{}", a.id)))
                                .small()
                                .primary()
                                .icon(IconName::Check)
                                .label("Разрешить")
                                .on_click(cx.listener(move |this, _, _, _| this.handle.approve(allow_id.clone(), true))),
                        )
                        .child(
                            PressButton::new(SharedString::from(format!("deny:{}", a.id)))
                                .small()
                                .outline()
                                .label("Отклонить")
                                .on_click(cx.listener(move |this, _, _, _| this.handle.approve(deny_id.clone(), false))),
                        ),
                )
        }))
    }
}

impl Render for SessionView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let p = Palette::of(&theme);
        let weak = cx.entity().downgrade();
        let running = self.status == Status::Running;

        let transcript = MessageScroller::new("transcript", self.scroller.clone(), move |r, _, cx| {
            weak.upgrade()
                .map(|view| view.read(cx).render_row(r, &view.downgrade(), cx))
                .unwrap_or_else(|| div().into_any_element())
        });

        v_flex()
            .id("session")
            .size_full()
            .bg(theme.background)
            // Drop images/files anywhere on the session to attach them.
            .drag_over::<ExternalPaths>({
                let ring = p.accent.solid;
                move |style, _, _, _| style.border_2().border_color(ring)
            })
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                this.attach_paths(paths.paths(), window, cx);
            }))
            .child(self.render_header(cx))
            .child(div().flex_1().min_h_0().map(|this| match (&self.changes, self.status) {
                (Some(changes), _) if self.show_changes => this.child(changes.clone()),
                (None, _) if self.show_changes => this.child(crate::loading::transcript(&theme)),
                // A restored tab shows last time's transcript while the agent re-reads it.
                (_, Status::Loading) if self.items.is_empty() => this.child(crate::loading::transcript(&theme)),
                _ => this.child(transcript),
            }))
            .child(
                h_flex().w_full().flex_shrink_0().px_6().pt(px(8.)).pb(px(14.)).justify_center().child(
                    v_flex()
                        .w_full()
                        .max_w(px(FEED_WIDTH))
                        .gap_2()
                        .when_some(self.error.clone(), |this, err| {
                            this.child(
                                div()
                                    .px(px(10.))
                                    .py(px(6.))
                                    .rounded(px(6.))
                                    .bg(p.red.soft)
                                    .font_family(theme.mono_font_family.clone())
                                    .text_size(px(11.5))
                                    .text_color(p.red.text)
                                    .child(err),
                            )
                        })
                        .when(running && self.approvals.is_empty(), |this| {
                            let elapsed = self.turn_started.map(|t| t.elapsed().as_secs()).unwrap_or(0);
                            this.child(crate::loading::working("session-working", self.activity().line, elapsed, &theme))
                        })
                        .child(self.render_approvals(cx))
                        .when_some(self.interrupted.clone().filter(|_| !running && !self.show_changes), |this, step| {
                            this.child(self.render_interrupted(step, &theme, cx))
                        })
                        .when(!self.queue.is_empty() && !self.show_changes, |this| this.child(self.render_queue(&theme, cx)))
                        .when(self.live_elsewhere() && !self.show_changes, |this| this.child(self.render_elsewhere(&theme)))
                        // The report is for reading: no composer over it.
                        .when(!self.show_changes, |this| this.child(
                            v_flex()
                                .key_context(attach::CONTEXT)
                                .on_action({
                                    let tray = self.attachments.clone();
                                    move |_: &attach::TakeScreenshot, _, cx| tray.update(cx, |t, cx| t.screenshot(cx))
                                })
                                .px(px(10.))
                                .pt(px(8.))
                                .pb(px(8.))
                                .gap_1()
                                .rounded(px(12.))
                                .border_1()
                                .border_color(p.border_strong)
                                .bg(p.surface)
                                .shadow(ui::card_shadow())
                                .child(self.attachments.clone())
                                .child(Textarea::new(&self.composer).appearance(false).on_paste({
                                    let tray = self.attachments.clone();
                                    move |item, _, cx| tray.update(cx, |t, cx| t.paste(item, cx))
                                }))
                                .child(
                                    h_flex()
                                        .gap_1()
                                        .items_center()
                                        .child(attach::attach_button(
                                            "attach",
                                            &self.attachments,
                                            "Прикрепить картинку или файл · можно вставить Cmd+V или перетащить в окно",
                                        ))
                                        .child(attach::screenshot_button("screenshot", &self.attachments, cx))
                                        .child(div().flex_1())
                                        .child(
                                            ui::mono(
                                                if self.busy_elsewhere() {
                                                    "ход идёт в терминале"
                                                } else if running {
                                                    "агент работает · ⏎ в очередь"
                                                } else {
                                                    "⏎ отправить"
                                                },
                                                &theme,
                                            )
                                            .text_color(p.faint)
                                            .mr_2(),
                                        )
                                        .child(if self.busy_elsewhere() {
                                            PressButton::new("send")
                                                .small()
                                                .primary()
                                                .icon(IconName::ArrowUp)
                                                .label("Отправить")
                                                .disabled(true)
                                                .tooltip("Дождитесь конца хода: агент сейчас работает в терминале")
                                        } else if running {
                                            PressButton::new("stop")
                                                .small()
                                                .outline()
                                                .icon(IconName::Square)
                                                .label("Стоп")
                                                .on_click(cx.listener(|this, _, _, _| this.handle.interrupt()))
                                        } else {
                                            PressButton::new("send")
                                                .small()
                                                .primary()
                                                .icon(IconName::ArrowUp)
                                                .label("Отправить")
                                                .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx)))
                                        }),
                                ),
                        )),
                ),
            )
    }
}

/// Fold runs of tool calls (and the reasoning between them) into one row.
/// File edits stay visible on their own so diffs are never hidden.
fn build_rows(items: &[Item]) -> Vec<Row> {
    let foldable = |i: &Item| match i.kind {
        ItemKind::Tool => i.diffs.is_empty() && i.attachments.is_empty(),
        ItemKind::Reasoning => true,
        _ => false,
    };
    let mut rows = Vec::new();
    let mut ix = 0;
    while ix < items.len() {
        if !foldable(&items[ix]) {
            rows.push(Row::Item(ix));
            ix += 1;
            continue;
        }
        let start = ix;
        while ix < items.len() && foldable(&items[ix]) {
            ix += 1;
        }
        let tools = items[start..ix].iter().filter(|i| i.kind == ItemKind::Tool).count();
        if tools >= 2 {
            rows.push(Row::Group { start, end: ix });
        } else {
            rows.extend((start..ix).map(Row::Item));
        }
    }
    rows
}

fn row_items(row: Row) -> std::ops::Range<usize> {
    match row {
        Row::Item(ix) => ix..ix + 1,
        Row::Group { start, end } => start..end,
    }
}

fn actions_word(n: usize) -> &'static str {
    match (n % 10, n % 100) {
        (1, r) if r != 11 => "действие",
        (2..=4, r) if !(12..=14).contains(&r) => "действия",
        _ => "действий",
    }
}

/// Path relative to the session's project, else with `~` for home.
fn display_path(path: &str, cwd: &std::path::Path) -> String {
    let p = std::path::Path::new(path);
    if let Ok(rel) = p.strip_prefix(cwd) {
        return rel.display().to_string();
    }
    // Agents often report /private/tmp for a /tmp cwd and vice versa.
    if let (Ok(a), Ok(b)) = (p.canonicalize(), cwd.canonicalize()) {
        if let Ok(rel) = a.strip_prefix(&b) {
            return rel.display().to_string();
        }
    }
    let home = std::env::var("HOME").unwrap_or_default();
    match path.strip_prefix(&home) {
        Some(rest) if !home.is_empty() => format!("~{rest}"),
        _ => path.to_owned(),
    }
}

/// Plain-text preview of a markdown answer.
fn strip_markdown(s: &str) -> String {
    s.replace("**", "").replace('`', "").replace("__", "")
}

fn first_line(s: &str, max: usize) -> String {
    let line = s.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    match line.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &line[..i]),
        None => line.to_owned(),
    }
}

/// The document rules plus what the shelf knows about the project in `cwd`,
/// for the agent's system prompt (setting «Документы → Правила агентам»).
fn docs_brief(provider: Provider, cwd: Option<&std::path::Path>, session: Option<&str>, cx: &App) -> Option<String> {
    if !crate::settings::config(cx).docs.brief {
        return None;
    }
    let project = docs_project(cwd);
    Some(stepan_verify_core::docs::agent_brief(&stepan_verify_core::Home::locate(), project.as_deref(), &provider.label().to_lowercase(), session))
}

/// The docs folder of a session in `cwd`: its last segment (the main
/// checkout's for a worktree), none for $HOME.
pub(crate) fn docs_project(cwd: Option<&std::path::Path>) -> Option<String> {
    let cwd = cwd.filter(|c| *c != home_dir())?;
    stepan_verify_core::worktree::project_name(cwd)
}

fn agent_provider(p: Provider) -> stepan_verify_agents::Provider {
    match p {
        Provider::Claude => stepan_verify_agents::Provider::Claude,
        Provider::Codex => stepan_verify_agents::Provider::Codex,
    }
}

/// report.json of the plan approved for task `id`, if it has one.
fn approved_plan(id: &str) -> Option<PathBuf> {
    let home = stepan_verify_core::Home::locate();
    let task = stepan_verify_core::Board::new(home.clone()).get(id).ok()?;
    let n = task.meta.plan_approved.filter(|_| task.meta.plan)?;
    Some(stepan_verify_core::plans::report_path(&home, id, n)).filter(|p| p.exists())
}

fn task_prompt(task: &BoardTask) -> String {
    let body = task.body.trim();
    let summary = task.meta.summary.as_deref().map(|s| format!("\n\nПрошлый итог / вопрос по задаче: {s}")).unwrap_or_default();
    // Images travel with the message itself; other files are named by path.
    let files: Vec<String> = task
        .meta
        .attachments
        .iter()
        .filter(|p| !stepan_verify_agents::is_image_path(p))
        .map(|p| format!("- {}", p.display()))
        .collect();
    let files = if files.is_empty() { String::new() } else { format!("\n\nФайлы к задаче:\n{}", files.join("\n")) };
    let head = format!(
        "Задача с доски Stepan: «{title}» (id {id}).\n\n{body}{summary}{files}\n\n",
        title = task.meta.title,
        id = task.id,
        body = if body.is_empty() { "(без описания)" } else { body },
    );
    let home = stepan_verify_core::Home::locate();
    if task.phase() == Some(stepan_verify_core::Phase::Plan) {
        return format!(
            "{head}Ты уже взял эту задачу. Команда `stepan_verify` есть в PATH, id задачи и твоё имя уже в окружении.\n\n{}",
            crate::changes::plan::plan_brief(&home, task)
        );
    }
    let plan = task
        .meta
        .plan_approved
        .filter(|_| task.meta.plan)
        .map(|n| format!("{}\n\n", crate::changes::plan::implement_brief(&stepan_verify_core::plans::report_path(&home, &task.id, n), n)))
        .unwrap_or_default();
    format!(
        "{head}{plan}Ты уже взял эту задачу. Команда `stepan_verify` есть в PATH, id задачи и твоё имя уже в окружении:\n\
         - коротко отмечай важные шаги: `stepan_verify task log \"что сделано\"`\n\
         - когда закончишь: `stepan_verify task complete \"итог; ссылка на PR, если есть\"` — задача уйдёт человеку на ревью\n\
         - если нужен ответ человека: `stepan_verify task block \"вопрос\"` и остановись"
    )
}

/// Ask the window to show a file in the preview panel next to the session.
fn open_path(session: &WeakEntity<SessionView>, path: &std::path::Path, cx: &mut App) {
    if let Some(view) = session.upgrade() {
        view.update(cx, |this, cx| cx.emit(SessionEvent::OpenFile { path: path.to_owned(), root: this.cwd.clone() }));
    }
}

/// Thumbnails for images, chips for files and folders. Clicks open the preview panel.
fn render_attachments(owner: &str, attachments: &[Attachment], session: &WeakEntity<SessionView>, theme: &Theme) -> AnyElement {
    if attachments.is_empty() {
        return div().into_any_element();
    }
    let p = Palette::of(theme);
    let (images, files): (Vec<&Attachment>, Vec<&Attachment>) =
        attachments.iter().partition(|a| a.kind == AttachmentKind::Image);
    v_flex()
        .gap_2()
        .when(!images.is_empty(), |this| {
            this.child(div().flex().flex_wrap().gap_3().children(images.into_iter().enumerate().map(|(i, a)| {
                let (path, session) = (a.path.clone(), session.clone());
                v_flex()
                    .gap(px(4.))
                    .items_end()
                    .child(
                        // Sized from the file header: the virtual list measures rows before images decode.
                        crate::media::picture(SharedString::from(format!("img:{owner}:{i}")), a.path.clone(), 360., 260.)
                            .rounded(px(8.))
                            .border_1()
                            .border_color(p.border)
                            .bg(p.surface)
                            .shadow(ui::card_shadow())
                            .cursor_pointer()
                            .hover(|s| s.border_color(p.accent.solid))
                            .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new("Открыть справа").build(window, cx))
                            .on_click(move |_, _, cx| open_path(&session, &path, cx)),
                    )
                    .child(ui::mono(a.label(), theme).text_color(p.faint))
            })))
        })
        .when(!files.is_empty(), |this| {
            this.child(div().flex().flex_wrap().gap_2().children(files.into_iter().enumerate().map(|(i, a)| {
                let (path, session) = (a.path.clone(), session.clone());
                h_flex()
                    .id(SharedString::from(format!("file:{owner}:{i}")))
                    .px(px(10.))
                    .py(px(6.))
                    .gap(px(6.))
                    .items_center()
                    .rounded(px(8.))
                    .border_1()
                    .border_color(p.border)
                    .bg(p.surface)
                    .text_size(px(12.5))
                    .text_color(p.fg)
                    .cursor_pointer()
                    .hover(|s| s.bg(p.surface2).border_color(p.border_strong))
                    .tooltip({
                        let full = a.path.display().to_string();
                        move |window, cx| gpui_kit::component::tooltip::Tooltip::new(full.clone()).build(window, cx)
                    })
                    .on_click(move |_, _, cx| open_path(&session, &path, cx))
                    .child(
                        Icon::new(if a.kind == AttachmentKind::Folder { IconName::Folder } else { IconName::File })
                            .xsmall()
                            .text_color(p.muted),
                    )
                    .child(a.label())
            })))
        })
        .into_any_element()
}

/// «● работает» in the session header.
fn status_pill(label: &'static str, tone: Option<Tone>, spin: bool, p: &Palette) -> Div {
    let (color, text) = tone.map(|t| (t.solid, t.text)).unwrap_or((p.faint, p.muted));
    h_flex()
        .flex_shrink_0()
        .gap(px(6.))
        .items_center()
        .px(px(9.))
        .py(px(3.))
        .rounded_full()
        .border_1()
        .border_color(ui::mix(color, p.border, 0.45))
        .bg(p.surface)
        .text_size(px(12.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(text)
        .child(if spin { Spinner::new().xsmall().color(color).into_any_element() } else { ui::dot(color).into_any_element() })
        .child(label)
}

/// A plan (TodoWrite / turn plan) as a card: done steps, the current one
/// highlighted with a blue edge, pending ones faint.
fn render_plan(id: SharedString, body: &str, p: &Palette, theme: &Theme) -> AnyElement {
    #[derive(PartialEq)]
    enum St {
        Done,
        Current,
        Pending,
    }
    let steps: Vec<(St, String)> = body
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let (mark, text) = l.split_at(l.char_indices().nth(1).map_or(l.len(), |(i, _)| i));
            let state = match mark {
                "✓" => St::Done,
                "▸" => St::Current,
                _ => St::Pending,
            };
            (state, text.trim().to_owned())
        })
        .collect();
    let total = steps.len();
    let done = steps.iter().filter(|(s, _)| *s == St::Done).count();
    let progress = match steps.iter().position(|(s, _)| *s == St::Current) {
        Some(i) => format!("Шаг {} из {total}", i + 1),
        None => format!("Готово {done} из {total}"),
    };
    ui::inner(p)
        .id(id)
        .w_full()
        .child(
            h_flex()
                .px(px(12.))
                .py(px(8.))
                .gap_2()
                .items_center()
                .border_b_1()
                .border_color(p.border)
                .child(Icon::new(IconName::Menu).xsmall().text_color(p.muted))
                .child(div().text_size(px(13.)).font_weight(FontWeight::BOLD).text_color(p.ink).child("План"))
                .child(div().flex_1())
                .child(ui::mono(progress, theme).text_color(p.faint)),
        )
        .children(steps.into_iter().enumerate().map(|(i, (state, label))| {
            let current = state == St::Current;
            h_flex()
                .px(px(12.))
                .py(px(7.))
                .gap_2()
                .items_center()
                .when(i > 0, |d| d.border_t_1().border_color(p.surface2))
                .when(current, |d| d.bg(p.surface2).border_l_2().border_color(p.blue.solid).pl(px(10.)))
                .child(match state {
                    St::Done => Icon::new(IconName::CircleCheck).xsmall().text_color(p.muted).into_any_element(),
                    St::Current => Spinner::new().xsmall().color(p.blue.solid).into_any_element(),
                    St::Pending => div()
                        .size(px(13.))
                        .flex_shrink_0()
                        .rounded_full()
                        .border_1()
                        .border_dashed()
                        .border_color(p.faint)
                        .into_any_element(),
                })
                .child(
                    div()
                        .min_w_0()
                        .text_size(px(13.))
                        .text_color(match state {
                            St::Done => p.muted,
                            St::Current => p.ink,
                            St::Pending => p.faint,
                        })
                        .when(current, |d| d.font_weight(FontWeight::BOLD))
                        .child(label),
                )
        }))
        .into_any_element()
}

fn mmss(secs: u64) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME").map(Into::into).unwrap_or_else(|| "/".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(id: &str) -> Item {
        Item::tool(id, id, "", ItemStatus::Done)
    }

    #[core::prelude::v1::test]
    fn folds_runs_of_two_or_more_tools() {
        let items = vec![
            Item::message("u", ItemKind::User, "hi"),
            tool("a"),
            Item::message("r", ItemKind::Reasoning, "…"),
            tool("b"),
            Item::message("x", ItemKind::Assistant, "ok"),
            tool("c"),
        ];
        assert_eq!(build_rows(&items), vec![Row::Item(0), Row::Group { start: 1, end: 4 }, Row::Item(4), Row::Item(5)]);
    }

    #[core::prelude::v1::test]
    fn edits_break_groups() {
        let mut edit = tool("e");
        edit.diffs.push(FileDiff { path: "f".into(), patch: "+x".into() });
        let items = vec![tool("a"), edit, tool("b")];
        assert_eq!(build_rows(&items), vec![Row::Item(0), Row::Item(1), Row::Item(2)]);
    }

    #[core::prelude::v1::test]
    fn russian_plurals() {
        assert_eq!(actions_word(1), "действие");
        assert_eq!(actions_word(3), "действия");
        assert_eq!(actions_word(11), "действий");
        assert_eq!(actions_word(22), "действия");
    }

    #[core::prelude::v1::test]
    fn task_prompt_lists_files_but_not_images() {
        let task = BoardTask {
            id: "t1".into(),
            path: "/x/t1.md".into(),
            meta: stepan_verify_core::tasks::TaskMeta {
                title: "Кнопка".into(),
                attachments: vec!["/f/t1.files/shot.png".into(), "/f/t1.files/crash.log".into()],
                ..Default::default()
            },
            body: "уехала".into(),
        };
        let prompt = task_prompt(&task);
        assert!(prompt.contains("Файлы к задаче:\n- /f/t1.files/crash.log"), "{prompt}");
        assert!(!prompt.contains("shot.png"), "{prompt}");
        assert!(!task_prompt(&BoardTask { meta: Default::default(), ..task }).contains("Файлы к задаче"));
    }
}
