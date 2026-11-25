//! «Изменения»: a map of what a session changed, built on demand by a
//! reviewer subagent of the session's own family (Claude or Codex) and drawn
//! natively. Every rebuild and every batch of a long session is its own run
//! (`~/.stepan_verify/state/changes/<session>/runs/<n>/`), so the map keeps its history:
//! you can step through iterations and see which decisions the agent took.

mod canvas;
mod map;
pub mod media;
mod runs;

use std::{
    cell::Cell,
    collections::HashSet,
    rc::Rc,
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use gpui_kit::component::{
    ActiveTheme, Icon, IconName, Sizable as _, Theme,
    input::{Input, InputState},
    skeleton::Skeleton,
    spinner::Spinner,
    switch::Switch,
    text::TextView,
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use stepan_verify_agents::{AgentEvent, AgentHandle, ItemKind, Mode, Provider, SessionSpec};
use serde::Deserialize;

use crate::{
    format::ago,
    ui::{self, Palette, PressButton, Tone},
};
use map::{EdgeKind, Graph, Mode as MapMode, NodeStatus};
use runs::{Meta, Run, RunKind};

// ------------------------------------------------------------------ input

/// One step of the session, in order: what the reviewer reads for a batch.
#[derive(Clone, Debug)]
pub enum LogItem {
    Request(String),
    Answer(String),
    Command(String),
    Edit { path: String, patch: String },
}

/// What the session did, handed to the reviewer.
#[derive(Clone)]
pub struct ChangesInput {
    /// Cache key: the native session id.
    pub key: String,
    /// The session's agent; the report is written by the same agent family.
    pub provider: Provider,
    pub title: String,
    pub cwd: PathBuf,
    pub requests: Vec<String>,
    pub final_answer: String,
    /// (path, unified hunks) for every edit the session made.
    pub patches: Vec<(String, String)>,
    pub commands: Vec<String>,
    pub screenshots: Vec<PathBuf>,
    /// The whole session as a log; batches read the part after the last run.
    pub log: Vec<LogItem>,
}

/// Folder of a session's reports.
fn out_dir(key: &str) -> PathBuf {
    stepan_verify_core::Home::locate().state().join("changes").join(key)
}

/// «Следить за сессией» is on for this session: its map updates after every turn.
pub fn follows(key: &str) -> bool {
    runs::follows(&out_dir(key))
}

// ------------------------------------------------------------------ report

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Report {
    pub title: String,
    pub summary: String,
    pub flow: Option<Diagram>,
    pub structure: Option<Diagram>,
    /// Extra diagrams of whatever type tells the story best: sequence, state, ER, class, …
    pub diagrams: Vec<Diagram>,
    pub code: CodeChanges,
    /// Links the session removed (they existed before, not after).
    pub removed_edges: Vec<EdgeRef>,
    pub ui: Vec<UiChange>,
    pub risks: Vec<Risk>,
    pub check: Vec<Check>,
    /// What the agent decided and why (per batch; a full run: the whole session).
    pub decisions: Vec<Decision>,
    /// Data passed between the nodes: props, arguments, API payloads, events.
    pub dtos: Vec<Dto>,
    /// What the entries do together: user features, helpers, infra (canvas groups).
    pub features: Vec<Feature>,
    /// Tests and the entries they verify.
    pub tests: Vec<TestRef>,
}

/// A group on the canvas: «эти штуки делают функционал X», «эти помогают».
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Feature {
    pub id: String,
    pub title: String,
    pub summary: String,
    /// feature | helper | infra | tests
    pub kind: String,
    /// Entry ids/names in this group.
    pub nodes: Vec<String>,
}

/// A test and what it checks.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct TestRef {
    pub id: String,
    pub title: String,
    pub file: String,
    /// Entry ids/names it verifies.
    pub verifies: Vec<String>,
    /// unit | e2e | snapshot | manual
    pub kind: String,
    /// added | changed | existing
    pub status: String,
    /// pass | fail | not_run
    pub result: String,
}

/// A structure that travels between nodes, with its fields.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Dto {
    pub id: String,
    pub name: String,
    /// props | args | response | request | event | type | state | other
    pub kind: String,
    pub file: String,
    /// added | changed | removed | context
    pub status: String,
    pub note: String,
    pub fields: Vec<DtoField>,
    /// Node ids/names that take or return it.
    pub used_by: Vec<String>,
}

/// `{"name", "type", "status", "note"}` or a plain `"flyToHub?: boolean"`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(from = "FieldRepr")]
pub struct DtoField {
    pub name: String,
    pub ty: String,
    pub status: String,
    pub note: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum FieldRepr {
    Text(String),
    Obj {
        #[serde(default)]
        name: String,
        #[serde(default, rename = "type")]
        ty: String,
        #[serde(default)]
        status: String,
        #[serde(default)]
        note: String,
    },
}

impl From<FieldRepr> for DtoField {
    fn from(r: FieldRepr) -> Self {
        match r {
            FieldRepr::Text(t) => match t.split_once(':') {
                Some((name, ty)) => DtoField { name: name.trim().into(), ty: ty.trim().into(), ..Default::default() },
                None => DtoField { name: t.trim().into(), ..Default::default() },
            },
            FieldRepr::Obj { name, ty, status, note } => DtoField { name, ty, status, note },
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Diagram {
    pub title: String,
    pub image: Option<String>,
    pub mermaid: Option<String>,
    pub caption: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct CodeChanges {
    pub added: Vec<Entry>,
    pub removed: Vec<Entry>,
    pub changed: Vec<Entry>,
    /// Untouched code the changed entries call or are called from: context nodes.
    pub context: Vec<Entry>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Entry {
    /// Short stable id that `uses` can refer to; the name works too.
    pub id: String,
    pub name: String,
    pub kind: String,
    /// component | function | hook | type | dto | config | test | api | store | style | script | other
    pub role: String,
    /// Human name of the thing: «Диалог выбора Джокера».
    pub entity: String,
    pub file: String,
    pub note: String,
    /// Pseudocode of the interface: `takeFlight(card: JokerCard, from: Rect) → Promise<void>`.
    pub signature: String,
    /// The interface before the session, when it changed.
    pub signature_before: String,
    /// DTO ids/names it takes and returns.
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    /// What this entry calls, renders or depends on: ids or names, optionally
    /// with the DTO passed along (`{"to": "id", "via": "dto id"}`).
    pub uses: Vec<UseRef>,
    /// Hunks of this entry: full hunk text, or the `@@ … @@` header to find it.
    pub hunks: Vec<String>,
    pub line: Option<u32>,
    pub risk_ids: Vec<String>,
    pub check_ids: Vec<String>,
}

/// A `uses` link: `"id"` or `{"to": "id", "via": "dto id", "note": "…"}`.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(from = "UseRepr")]
pub struct UseRef {
    pub to: String,
    pub via: String,
    pub note: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum UseRepr {
    Text(String),
    Obj {
        #[serde(default, alias = "id", alias = "name")]
        to: String,
        #[serde(default, alias = "dto", alias = "passes")]
        via: String,
        #[serde(default)]
        note: String,
    },
}

impl From<UseRepr> for UseRef {
    fn from(r: UseRepr) -> Self {
        match r {
            UseRepr::Text(to) => UseRef { to, ..Default::default() },
            UseRepr::Obj { to, via, note } => UseRef { to, via, note },
        }
    }
}

impl From<&str> for UseRef {
    fn from(to: &str) -> Self {
        UseRef { to: to.to_owned(), ..Default::default() }
    }
}

/// `["from", "to"]` or `{"from": …, "to": …}`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(from = "EdgeRepr")]
pub struct EdgeRef {
    pub from: String,
    pub to: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum EdgeRepr {
    Pair(Vec<String>),
    Obj {
        #[serde(default)]
        from: String,
        #[serde(default)]
        to: String,
    },
}

impl From<EdgeRepr> for EdgeRef {
    fn from(r: EdgeRepr) -> Self {
        match r {
            EdgeRepr::Pair(v) => EdgeRef { from: v.first().cloned().unwrap_or_default(), to: v.get(1).cloned().unwrap_or_default() },
            EdgeRepr::Obj { from, to } => EdgeRef { from, to },
        }
    }
}

/// A risk: plain text (old reports) or an object tied to nodes.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(from = "RiskRepr")]
pub struct Risk {
    pub id: String,
    /// high | mid | low, or empty.
    pub severity: String,
    pub title: String,
    pub detail: String,
    pub fix: String,
    pub nodes: Vec<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RiskRepr {
    Text(String),
    Obj(RiskObj),
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RiskObj {
    id: String,
    severity: String,
    title: String,
    detail: String,
    fix: String,
    nodes: Vec<String>,
    node: Option<String>,
}

impl From<RiskRepr> for Risk {
    fn from(r: RiskRepr) -> Self {
        match r {
            RiskRepr::Text(t) => {
                let lower = t.to_lowercase();
                let severity = if lower.contains("высок") || lower.contains("критич") {
                    "high"
                } else if lower.contains("средн") {
                    "mid"
                } else if lower.contains("низк") {
                    "low"
                } else {
                    ""
                };
                Risk { severity: severity.into(), title: t, ..Default::default() }
            }
            RiskRepr::Obj(o) => {
                let mut nodes = o.nodes;
                nodes.extend(o.node);
                Risk { id: o.id, severity: o.severity, title: o.title, detail: o.detail, fix: o.fix, nodes }
            }
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(from = "CheckRepr")]
pub struct Check {
    pub id: String,
    pub title: String,
    pub how: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum CheckRepr {
    Text(String),
    Obj {
        #[serde(default)]
        id: String,
        #[serde(default)]
        title: String,
        #[serde(default)]
        how: String,
    },
}

impl From<CheckRepr> for Check {
    fn from(r: CheckRepr) -> Self {
        match r {
            CheckRepr::Text(title) => Check { title, ..Default::default() },
            CheckRepr::Obj { id, title, how } => Check { id, title, how },
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Decision {
    pub title: String,
    pub why: String,
    pub nodes: Vec<String>,
    /// When in the session, as the reviewer saw it ("14:22", "после теста").
    pub at: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct UiChange {
    pub title: String,
    pub before: Option<String>,
    pub after: Option<String>,
    pub note: String,
}

// ------------------------------------------------------------------ view

pub enum ChangesEvent {
    /// «Вернуть с замечанием»: send this text to the session's agent.
    Return(String),
    /// «Принять».
    Accept,
}

struct Running {
    n: usize,
    kind: RunKind,
    started: Instant,
    model: String,
    steps: Vec<String>,
    handle: AgentHandle,
    _events: Task<()>,
}

#[derive(Clone, PartialEq)]
enum Panel {
    Node(String),
    Dto(String),
    Group(String),
    Checks,
    Return,
}

pub struct ChangesView {
    input: ChangesInput,
    out: PathBuf,
    runs: Vec<Run>,
    /// Index into `runs` of the iteration on screen; None: the running one.
    selected: Option<usize>,
    running: Option<Running>,
    /// A batch was asked for while another run was going.
    pending: bool,
    follow: bool,
    mode: MapMode,
    hovered: Option<String>,
    /// DTO under the pointer on the map: its users light up.
    hovered_dto: Option<String>,
    panel: Option<Panel>,
    /// Show the added / changed / removed columns instead of the map.
    show_list: bool,
    /// Ticked rows of «Проверь руками».
    checked: HashSet<usize>,
    /// Node keys put into the remark for the agent.
    remarks: Vec<String>,
    accepted: bool,
    /// git.patch of each run (by run number), parsed once.
    git_patches: std::collections::HashMap<usize, Vec<(String, String)>>,
    /// Cached `has_news()`: the session moved on since the newest map.
    news: bool,
    /// The work tree diff now, for runs from before git.patch existed.
    live_git: Option<Vec<(String, String)>>,
    /// Canvas pan and zoom.
    view: canvas::View,
    /// What the view was last fitted to ("run-mode"); a change refits.
    fit_key: Option<String>,
    drag: Option<canvas::Drag>,
    /// Viewport size of the last fit, and whether you moved the view since:
    /// an untouched view refits when the window resizes.
    fit_size: (f32, f32),
    touched: bool,
    /// The last press moved the canvas: the click that ends it is not a click.
    dragged: bool,
    /// Canvas viewport on screen, written while painting.
    viewport: Rc<Cell<Option<Bounds<Pixels>>>>,
    _ticker: Task<()>,
}

impl EventEmitter<ChangesEvent> for ChangesView {}

impl ChangesView {
    /// Shows the latest report, or starts building one.
    pub fn new(input: ChangesInput, cx: &mut Context<Self>) -> Self {
        let out = out_dir(&input.key);
        if let Err(err) = runs::migrate(&out) {
            eprintln!("stepan_verify: changes: migrate {}: {err}", out.display());
        }
        let follow = runs::follows(&out);
        let mut this = Self {
            input,
            out,
            runs: Vec::new(),
            selected: None,
            running: None,
            pending: false,
            follow,
            mode: MapMode::Diff,
            hovered: None,
            hovered_dto: None,
            panel: None,
            show_list: false,
            checked: HashSet::new(),
            remarks: Vec::new(),
            accepted: false,
            git_patches: Default::default(),
            news: false,
            live_git: None,
            view: canvas::View::default(),
            fit_key: None,
            drag: None,
            fit_size: (0., 0.),
            touched: false,
            dragged: false,
            viewport: Rc::new(Cell::new(None)),
            _ticker: Task::ready(()),
        };
        media::observe(cx);
        this.reload(None);
        if this.runs.is_empty() {
            this.start(cx);
        } else {
            // Debug hook: STEPAN_VERIFY_CHANGES_RUN=full|batch starts a run right away.
            match std::env::var("STEPAN_VERIFY_CHANGES_RUN").as_deref() {
                Ok("full") => this.launch(RunKind::Full, cx),
                Ok("batch") => this.launch(RunKind::Batch, cx),
                _ => {}
            }
        }
        this
    }

    /// Reread runs from disk; select `select` (a run number) or the newest good one.
    fn reload(&mut self, select: Option<usize>) {
        self.runs = runs::load_all(&self.out);
        for run in &self.runs {
            if !self.git_patches.contains_key(&run.n) {
                if let Ok(text) = fs::read_to_string(run.dir.join("git.patch")) {
                    self.git_patches.insert(run.n, runs::parse_patch(&text));
                }
            }
        }
        if self.input.patches.is_empty() && self.live_git.is_none() && self.runs.iter().any(|r| !self.git_patches.contains_key(&r.n)) {
            self.live_git = Some(runs::parse_patch(&runs::diff(&self.input.cwd, None, None)));
        }
        self.news = self.has_news();
        let by_n = select.and_then(|n| self.runs.iter().position(|r| r.n == n));
        self.selected = by_n.or_else(|| self.runs.iter().rposition(|r| r.ok().is_some())).or(self.runs.len().checked_sub(1));
    }

    /// «Пересобрать»: a full run over the whole session.
    pub fn start(&mut self, cx: &mut Context<Self>) {
        self.launch(RunKind::Full, cx);
    }

    /// The session moved on (a turn finished). With «Следить» on, a batch
    /// covers what happened since the last run.
    pub fn session_progressed(&mut self, input: ChangesInput, cx: &mut Context<Self>) {
        self.input = input;
        self.news = self.has_news();
        if !self.follow || !self.news {
            cx.notify();
            return;
        }
        if self.running.is_some() {
            self.pending = true;
        } else {
            self.launch(RunKind::Batch, cx);
        }
        cx.notify();
    }

    /// Edits (or work-tree changes) after the newest run.
    fn has_news(&self) -> bool {
        let Some(last) = self.runs.iter().rev().find(|r| r.ok().is_some()) else { return true };
        let delta = self.input.log.get(last.meta.upto..).unwrap_or(&[]);
        if delta.iter().any(|i| matches!(i, LogItem::Edit { .. })) {
            return true;
        }
        let commands = delta.iter().any(|i| matches!(i, LogItem::Command(_)));
        commands && last.meta.git_snapshot.as_deref().is_some_and(|snap| !runs::diff(&self.input.cwd, Some(snap), last.meta.at.map(Into::into)).trim().is_empty())
    }

    fn toggle_follow(&mut self, on: bool, cx: &mut Context<Self>) {
        self.follow = on;
        runs::set_follow(&self.out, on);
        if on && self.running.is_none() && !self.runs.is_empty() && self.has_news() {
            self.launch(RunKind::Batch, cx);
        }
        cx.notify();
    }

    fn launch(&mut self, kind: RunKind, cx: &mut Context<Self>) {
        let base = self.runs.iter().rev().find(|r| r.ok().is_some()).map(|r| (r.n, r.dir.clone(), r.meta.clone()));
        // Without a good earlier map a batch has nothing to extend.
        let kind = if base.is_none() { RunKind::Full } else { kind };
        let (n, dir) = match runs::next_dir(&self.out) {
            Ok(v) => v,
            Err(err) => {
                eprintln!("stepan_verify: changes: {err}");
                return;
            }
        };
        let provider = self.input.provider;
        let config = crate::settings::config(cx).changes.clone();
        let model = match provider {
            Provider::Claude => Some(config.model.clone()),
            Provider::Codex => Some(config.codex_model.clone()).filter(|m| !m.is_empty()),
        };
        let reviewer = reviewer_name(provider, model.as_deref());
        let snapshot = runs::snapshot(&self.input.cwd);
        let mut meta = Meta {
            at: Some(chrono::Utc::now()),
            kind,
            upto: self.input.log.len(),
            base_run: base.as_ref().filter(|_| kind == RunKind::Batch).map(|b| b.0),
            git_snapshot: snapshot,
            edits: 0,
            reviewer: reviewer.clone(),
            error: None,
        };
        let written = match (kind, &base) {
            (RunKind::Batch, Some((_, base_dir, base_meta))) => self.write_batch_context(&dir, base_dir, base_meta),
            _ => self.write_full_context(&dir),
        };
        match written {
            Ok(edits) => meta.edits = edits,
            Err(err) => meta.error = Some(format!("Не удалось подготовить контекст: {err:#}")),
        }
        let _ = runs::save_meta(&dir, &meta);
        if meta.error.is_some() {
            self.reload(Some(n));
            cx.notify();
            return;
        }
        let spec = SessionSpec {
            provider,
            native_id: None,
            transcript: None,
            cwd: self.input.cwd.clone(),
            // The reviewer only reads the project and writes into its own folder,
            // but it runs scripts (git, node) and must not stop to ask.
            mode: Mode::FullAccess,
            image_cache: Some(stepan_verify_core::Home::locate().state().join("images")),
            env: vec![("STEPAN_VERIFY_CHANGES_OUT".into(), dir.display().to_string())],
            model,
            instructions: None,
        };
        let (handle, events) = stepan_verify_agents::open(spec);
        handle.send(match kind {
            RunKind::Full => prompt_full(&dir),
            RunKind::Batch => prompt_batch(&dir),
        });
        let pump = cx.spawn(async move |this, cx| {
            while let Ok(event) = events.recv().await {
                let done = matches!(event, AgentEvent::TurnCompleted { .. } | AgentEvent::Exited);
                let alive = this.update(cx, |this, cx| {
                    this.on_event(event, cx);
                    cx.notify();
                });
                if alive.is_err() || done {
                    break;
                }
            }
        });
        self.running = Some(Running { n, kind, started: Instant::now(), model: reviewer, steps: Vec::new(), handle, _events: pump });
        self.reload(None);
        // A full rebuild is what you asked to see; a batch keeps the current view.
        if kind == RunKind::Full {
            self.selected = None;
        }
        self._ticker = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let running = this.update(cx, |this, cx| {
                    cx.notify();
                    this.running.is_some()
                });
                if !matches!(running, Ok(true)) {
                    break;
                }
            }
        });
        cx.notify();
    }

    fn on_event(&mut self, event: AgentEvent, cx: &mut Context<Self>) {
        let Some(running) = &mut self.running else { return };
        match event {
            AgentEvent::Upsert(item) if item.kind == ItemKind::Tool && !item.title.is_empty() => {
                let dir = runs::runs_dir(&self.out).join(running.n.to_string());
                let step = humanize_step(&item.title, &dir);
                if running.steps.last() != Some(&step) {
                    running.steps.push(step);
                }
            }
            AgentEvent::TurnCompleted { error } => self.finish(error, cx),
            AgentEvent::Exited => self.finish(Some("Ревьюер завершился".into()), cx),
            AgentEvent::Error(err) => self.finish(Some(err), cx),
            _ => {}
        }
    }

    fn finish(&mut self, error: Option<String>, cx: &mut Context<Self>) {
        let Some(running) = self.running.take() else { return };
        let dir = runs::runs_dir(&self.out).join(running.n.to_string());
        let run = runs::load_run(running.n, dir.clone());
        if run.ok().is_none() {
            let mut meta = run.meta.clone();
            meta.error = Some(match (&run.report, error) {
                (Some(Err(e)), _) => e.clone(),
                (_, Some(e)) => e,
                _ => "Ревьюер закончил, но не записал report.json".into(),
            });
            let _ = runs::save_meta(&dir, &meta);
        }
        // Follow the newest map while watching; otherwise stay where you are.
        let viewing_latest = self.selected.is_none_or(|i| i + 1 >= self.runs.len().saturating_sub(1));
        let select = (running.kind == RunKind::Full || viewing_latest).then_some(running.n);
        let keep = self.selected.and_then(|i| self.runs.get(i)).map(|r| r.n);
        self.reload(select.or(keep));
        if std::mem::take(&mut self.pending) && self.follow && self.has_news() {
            self.launch(RunKind::Batch, cx);
        }
        cx.notify();
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        if let Some(r) = &self.running {
            r.handle.interrupt();
        }
        self.pending = false;
        self.finish(Some("Отменено".into()), cx);
    }

    /// context.md + session.patch (+ git.patch) for a full run. Returns the edit count.
    fn write_full_context(&self, dir: &Path) -> anyhow::Result<usize> {
        let i = &self.input;
        let mut md = format!("# Сессия: {}\n\nПроект: {}\n\n## Что просили\n\n", i.title, i.cwd.display());
        for r in &i.requests {
            md.push_str(&format!("- {}\n", one_paragraph(r, 1500)));
        }
        md.push_str(&format!("\n## Последний ответ агента\n\n{}\n", one_paragraph(&i.final_answer, 3000)));
        let mut files: Vec<&str> = i.patches.iter().map(|(p, _)| p.as_str()).collect();
        files.dedup();
        md.push_str("\n## Файлы, которые правила эта сессия (диффы в session.patch)\n\n");
        if files.is_empty() {
            md.push_str("(правок через инструменты редактирования нет; агент мог менять файлы командами — смотри git.patch)\n");
        }
        for f in &files {
            md.push_str(&format!("- {f}\n"));
        }
        if !i.commands.is_empty() {
            md.push_str("\n## Команды, которые запускал агент (последние)\n\n");
            for c in i.commands.iter().rev().take(60).rev() {
                md.push_str(&format!("- `{}`\n", c.replace('`', "'")));
            }
        }
        if !i.screenshots.is_empty() {
            md.push_str("\n## Скриншоты, снятые в сессии (можно использовать как «Стало»)\n\n");
            for s in &i.screenshots {
                md.push_str(&format!("- {}\n", s.display()));
            }
        }
        fs::write(dir.join("context.md"), md)?;
        let patch: String = i.patches.iter().map(|(p, d)| format!("--- a/{p}\n+++ b/{p}\n{d}\n")).collect();
        fs::write(dir.join("session.patch"), patch)?;
        fs::write(dir.join("git.patch"), runs::diff(&i.cwd, None, None))?;
        Ok(i.patches.len())
    }

    /// The part of the session after the base run, plus the base map.
    fn write_batch_context(&self, dir: &Path, base_dir: &Path, base_meta: &Meta) -> anyhow::Result<usize> {
        let i = &self.input;
        let delta = i.log.get(base_meta.upto..).unwrap_or(&[]);
        let mut md = format!(
            "# Сессия: {}\n\nПроект: {}\n\n## Что произошло с прошлого прогона ({} шагов)\n\n",
            i.title,
            i.cwd.display(),
            delta.len()
        );
        let mut patch = String::new();
        let mut edits = 0;
        for item in delta {
            match item {
                LogItem::Request(t) => md.push_str(&format!("- **Человек:** {}\n", one_paragraph(t, 1500))),
                LogItem::Answer(t) => md.push_str(&format!("- **Агент:** {}\n", one_paragraph(t, 1200))),
                LogItem::Command(c) => md.push_str(&format!("- `$ {}`\n", one_paragraph(&c.replace('`', "'"), 300))),
                LogItem::Edit { path, patch: d } => {
                    edits += 1;
                    md.push_str(&format!("- правка `{path}`\n"));
                    patch.push_str(&format!("--- a/{path}\n+++ b/{path}\n{d}\n"));
                }
            }
        }
        fs::write(dir.join("context.md"), md)?;
        fs::write(dir.join("session.patch"), patch)?;
        let since = base_meta.at.map(Into::into);
        fs::write(dir.join("git.patch"), runs::diff(&i.cwd, base_meta.git_snapshot.as_deref(), since))?;
        // The base map with picture paths made absolute, so a copied field still resolves.
        let text = fs::read_to_string(base_dir.join("report.json"))?;
        let mut v: serde_json::Value = serde_json::from_str(&text)?;
        for key in ["flow", "structure"] {
            if let Some(img) = v.get_mut(key).and_then(|d| d.get_mut("image")) {
                if let Some(p) = img.as_str() {
                    *img = resolve(base_dir, p).display().to_string().into();
                }
            }
        }
        if let Some(items) = v.get_mut("ui").and_then(|u| u.as_array_mut()) {
            for u in items {
                for key in ["before", "after"] {
                    if let Some(img) = u.get_mut(key) {
                        if let Some(p) = img.as_str() {
                            *img = resolve(base_dir, p).display().to_string().into();
                        }
                    }
                }
            }
        }
        fs::write(dir.join("base.json"), serde_json::to_string_pretty(&v)?)?;
        Ok(edits)
    }

    fn report(&self) -> Option<&Report> {
        self.selected.and_then(|i| self.runs.get(i)).and_then(Run::ok)
    }

    fn run_dir(&self) -> PathBuf {
        self.selected.and_then(|i| self.runs.get(i)).map(|r| r.dir.clone()).unwrap_or_else(|| self.out.clone())
    }

    /// Diffs for the map: the session's tool edits, else the work tree (git).
    fn patches(&self) -> Vec<(String, String)> {
        if !self.input.patches.is_empty() {
            return self.input.patches.clone();
        }
        let n = self.selected.and_then(|i| self.runs.get(i)).map(|r| r.n);
        n.and_then(|n| self.git_patches.get(&n)).filter(|p| !p.is_empty()).or(self.live_git.as_ref()).cloned().unwrap_or_default()
    }

    /// A press on the canvas: it may become a pan.
    fn drag_start(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        self.drag = Some(canvas::Drag::new((f32::from(at.x), f32::from(at.y)), self.view));
        self.dragged = false;
        cx.notify();
    }

    /// A window position relative to the canvas viewport.
    fn local(&self, at: Point<Pixels>) -> (f32, f32) {
        let o = self.viewport.get().map(|b| b.origin).unwrap_or_default();
        (f32::from(at.x - o.x), f32::from(at.y - o.y))
    }

    fn zoom_center(&mut self, factor: f32, cx: &mut Context<Self>) {
        let center = self.viewport.get().map_or((400., 300.), |b| (f32::from(b.size.width) / 2., f32::from(b.size.height) / 2.));
        self.view = self.view.zoom_at(factor, center);
        self.touched = true;
        cx.notify();
    }

    fn fit(&mut self, world: (f32, f32), cx: &mut Context<Self>) {
        if let Some(b) = self.viewport.get() {
            self.fit_size = (f32::from(b.size.width), f32::from(b.size.height));
            self.view = canvas::View::fit(world, self.fit_size);
            self.touched = false;
            cx.notify();
        }
    }

    fn remark_text(&self, graph: &Graph, note: &str) -> String {
        let title = self.report().map(|r| r.title.clone()).filter(|t| !t.is_empty()).unwrap_or_else(|| self.input.title.clone());
        let mut text = format!("Ревью изменений «{title}» — верни на доработку:\n");
        for key in &self.remarks {
            if let Some(n) = graph.index(key).map(|i| &graph.nodes[i]) {
                let file = if n.file.is_empty() { String::new() } else { format!(" ({})", n.file) };
                text.push_str(&format!("- {}{file}\n", n.name));
            }
        }
        if let Some(r) = self.report() {
            let open: Vec<&Check> = r.check.iter().enumerate().filter(|(i, _)| !self.checked.contains(i)).map(|(_, c)| c).collect();
            if !open.is_empty() && self.checked.len() > 0 {
                text.push_str("\nЕщё не проверено руками:\n");
                for c in open {
                    text.push_str(&format!("- {}\n", c.title));
                }
            }
        }
        if !note.trim().is_empty() {
            text.push_str(&format!("\n{}\n", note.trim()));
        }
        text
    }
}

/// The pr-before-after skill, wherever it is installed: the shared stepan_verify folder
/// first, then Claude's and the generic agents' skill folders.
fn find_skill() -> Option<PathBuf> {
    let home = dirs_home();
    [stepan_verify_core::Home::locate().skills(), home.join(".claude/skills"), home.join(".agents/skills")]
        .into_iter()
        .map(|dir| dir.join("pr-before-after"))
        .find(|dir| dir.join("scripts/diagram.mjs").exists())
}

const SCHEMA: &str = "{\"title\": \"заголовок изменений\", \"summary\": \"2–4 предложения простым языком: что изменилось для продукта и пользователя\",\n\
 \"flow\": {\"title\": \"…\", \"image\": \"flow.png\", \"mermaid\": \"исходник\", \"caption\": \"одна фраза\"} или null,\n\
 \"structure\": {\"title\": \"…\", \"image\": \"structure.png\", \"caption\": \"…\"} или null,\n\
 \"diagrams\": [{\"title\": \"…\", \"image\": \"lifecycle.png\", \"mermaid\": \"исходник\", \"caption\": \"…\"}],\n\
 \"code\": {\"added\": [{\"id\": \"короткий_id\", \"name\": \"имя как в коде\", \"kind\": \"function|component|hook|endpoint|route|type|table|config|test|file|other\", \
\"role\": \"component|function|hook|type|dto|config|test|api|store|style|script|other\", \"entity\": \"человеческое имя сущности\", \
\"file\": \"путь от корня проекта\", \"line\": 42, \"note\": \"зачем, одной фразой\", \
\"signature\": \"takeFlight(card: JokerCard, from: Rect) → Promise<void>\", \"signature_before\": \"только если интерфейс менялся\", \
\"inputs\": [\"id DTO\"], \"outputs\": [\"id DTO\"], \"uses\": [\"id\", {\"to\": \"id\", \"via\": \"id DTO\", \"note\": \"что и зачем передаёт\"}], \
\"hunks\": [\"@@ -41,19 +46,33 @@\"], \"risk_ids\": [\"r1\"], \"check_ids\": [\"c1\"]}], \"removed\": [], \"changed\": [], \"context\": []},\n\
 \"dtos\": [{\"id\": \"joker_props\", \"name\": \"JokerPickDialogProps\", \"kind\": \"props|args|response|request|event|state|type\", \"file\": \"путь\", \
\"status\": \"added|changed|removed|context\", \"note\": \"что это за данные\", \"fields\": [{\"name\": \"flyToHub\", \"type\": \"boolean?\", \"status\": \"added|changed|removed|context\", \"note\": \"зачем\"}], \
\"used_by\": [\"id узла\"]}],\n\
 \"features\": [{\"id\": \"f1\", \"title\": \"функционал простыми словами\", \"summary\": \"что он даёт пользователю\", \"kind\": \"feature|helper|infra|tests\", \"nodes\": [\"id\"]}],\n\
 \"tests\": [{\"id\": \"t1\", \"title\": \"что проверяет\", \"file\": \"путь\", \"verifies\": [\"id узла\"], \"kind\": \"unit|e2e|snapshot|manual\", \"status\": \"added|changed|existing\", \"result\": \"pass|fail|not_run\"}],\n\
 \"removed_edges\": [[\"id откуда\", \"id куда\"]],\n\
 \"ui\": [{\"title\": \"экран/элемент\", \"before\": null, \"after\": \"путь\", \"note\": \"что поменялось\"}],\n\
 \"risks\": [{\"id\": \"r1\", \"severity\": \"high|mid|low\", \"title\": \"что может сломаться\", \"detail\": \"когда и почему\", \"fix\": \"как поправить\", \"nodes\": [\"id\"]}],\n\
 \"check\": [{\"id\": \"c1\", \"title\": \"что проверить руками\", \"how\": \"как именно\"}],\n\
 \"decisions\": [{\"title\": \"решение агента\", \"why\": \"почему так (по его словам или по коду)\", \"nodes\": [\"id\"], \"at\": \"когда в сессии\"}]}";

const MAP_RULES: &str = "Карта — главное в отчёте: приложение само рисует граф из `code`. Для каждой записи выпиши `uses` (что она вызывает, рендерит, использует — id других записей или имена), \
у удалённых — какими были. Связи, которые сессия убрала у оставшихся записей, перечисли в `removed_edges`. Неизменённый код рядом (кто вызывает новое, что использует изменённое) — в `code.context`, 3–10 узлов. \
`hunks` — заголовки ханков из диффа (строка `@@ … @@`), которые относятся к записи. Риски и проверки привязывай к узлам (`nodes`, `risk_ids`, `check_ids`). \
`signature` — у КАЖДОЙ записи: интерфейс псевдокодом в стиле TypeScript, суть входа и выхода, а не копия кода (компонент: `Name({ a, b? }: Props) → JSX`, функция: `name(x: T, y: U) → R`, эндпоинт: `POST /path (Body) → Response`, конфиг: `NAME: Type = значение`). \
Если интерфейс поменялся — прежний в `signature_before`. `dtos` — все данные, которые передаются между узлами карты: пропсы компонентов, аргументы, тела и ответы API, события, куски состояния; \
у каждой структуры поля с типами, изменённые поля помечай `status`. В `uses` указывай `via` — какой DTO едет по связи; `inputs`/`outputs` у записи — какие DTO она принимает и отдаёт. \
Если session.patch пуст, агент правил файлы командами — бери изменения из git.patch. \
Человек смотрит на карту как на канвас смыслов, а не реализации: сгруппируй записи в `features` по смыслу — функциональность для пользователя (kind `feature`, «эти штуки делают функционал X», 1–5 групп), \
то, что ей помогает (`helper`: утилиты, константы, стили, обёртки, типы), инфраструктура и конфиг (`infra`). Каждая запись — ровно в одной группе; группы называй по-человечески, не по файлам; \
`summary` группы — одна фраза, что она даёт. У каждой записи `role` (кто это: компонент, функция, хук, тип, конфиг…) и `entity` — короткое человеческое имя («Диалог выбора Джокера», «Полёт карты в альбом»). \
Низкоуровневые мелочи не плоди отдельными узлами — сворачивай в helper-группу. `tests` — тесты, которые сессия добавила или поменяла, и существующие, что проверяют изменённое (поищи в проекте рядом с файлами); \
`verifies` — id узлов, которые тест проверяет; `result` — только если видно, что тест запускали. Если тестов нет — оставь `tests` пустым, это тоже сигнал.";

fn prompt_full(out: &Path) -> String {
    let skill = find_skill().unwrap_or_default();
    let renderer = if skill.join("scripts/diagram.mjs").exists() {
        format!(
            "Схемы рендери скриптом скилла pr-before-after (рендерер pele, синтаксис Mermaid, любой тип; каталог «когда какой» — {s}/references/diagram-types.md): \
             `node {s}/scripts/diagram.mjs --src {out}/flow.mmd --out {out}/flow.png --lang ru --theme both --title \"…\"`; \
             было/стало — `--before {out}/before.mmd --after {out}/after.mmd --out {out}/structure.png --lang ru --theme both` (дифф считается для любого типа, если обе стороны одного типа). \
             `--theme both` даёт ещё *-dark.png для тёмной темы; в report.json пиши путь светлой. Если скрипт упал с ошибкой синтаксиса — поправь исходник и запусти снова. Открой PNG и проверь, что подписи читаются.",
            s = skill.display(),
            out = out.display()
        )
    } else {
        "Скрипта рендера диаграмм нет: положи Mermaid-исходник в поле `mermaid`, `image` оставь null.".to_owned()
    };
    format!(
        "[stepan_verify changes] Ты — ревьюер изменений. Задача: показать человеку, что изменила сессия агента, так, чтобы он понял это за минуту, не читая код.\n\n\
         Папка для результата: {out} (дальше $OUT). Там лежат context.md (что просили, что ответил агент, файлы, команды, скриншоты), \
         session.patch (правки инструментами) и git.patch (git diff рабочего дерева). Текущая папка — проект.\n\n\
         Шаги:\n\
         1. Прочитай $OUT/context.md, $OUT/session.patch и $OUT/git.patch.\n\
         2. Что добавлено, удалено и изменено на уровне функций, компонентов, хуков, эндпоинтов, роутов, типов, таблиц, конфигов. Имена — как в коде. {MAP_RULES}\n\
         3. Флоу: схема, как теперь работает то, что делала сессия: 5–12 узлов, подписи по-русски, в $OUT/flow.mmd. Тип выбирай под историю, а не всегда flowchart: \
поток запроса — sequenceDiagram, кто что делает — swimlane-beta, жизненный цикл — stateDiagram-v2. Если поменялась структура модулей — before.mmd/after.mmd (тем же типом), иначе пропусти. \
Если сессия меняла то, что лучше видно другой схемой (схема БД — erDiagram, классы/интерфейсы — classDiagram, статусы — stateDiagram-v2, папки — treeView-beta, план — gantt), \
добавь 0–3 такие в `diagrams` (было/стало, если это изменение, иначе одна). Не рисуй схему ради схемы.\n\
         4. UI: скриншоты из context.md — как «Стало». «Было» — только если это дёшево и честно, иначе null. Никогда не подделывай «Было».\n\
         5. Решения: 3–7 главных решений агента за сессию (`decisions`) — что выбрал и почему, к каким узлам.\n\
         {renderer}\n\
         6. Запиши $OUT/report.json строго по схеме (валидный JSON, пути к картинкам абсолютные или относительно $OUT):\n{SCHEMA}\n\n\
         Ограничения: ничего не меняй в проекте и не коммить; пиши только в $OUT; не оставляй запущенных серверов; уложись примерно в 5 минут. \
         Пиши кратко и по делу. Когда report.json записан, ответь одним словом: готово.",
        out = out.display(),
    )
}

fn prompt_batch(out: &Path) -> String {
    format!(
        "[stepan_verify changes] Ты ведёшь карту изменений длинной сессии агента и обновляешь её очередной порцией.\n\n\
         Папка: {out} ($OUT). $OUT/base.json — карта на прошлом шаге (полный отчёт). $OUT/context.md — что произошло с тех пор: \
         сообщения человека и агента, команды, правки. $OUT/session.patch — новые правки инструментами, $OUT/git.patch — изменения рабочего дерева с прошлого шага. Текущая папка — проект.\n\n\
         Сделай:\n\
         1. Прочитай base.json и дельту. Не перечитывай весь проект: только то, что нужно, чтобы понять новые правки.\n\
         2. Обнови карту: сохрани id и записи из base.json, добавь новые узлы, поменяй статус/описание/связи затронутых, перенеси откаты. {MAP_RULES}\n\
         3. `decisions` — ТОЛЬКО решения из этой порции: что агент выбрал и почему, к каким узлам; `at` — момент в сессии.\n\
         4. title/summary/риски/проверки обнови под итог на сейчас. Поля flow/structure/diagrams/ui скопируй из base.json как есть (схемы не перерисовывай).\n\
         5. Запиши ПОЛНЫЙ обновлённый $OUT/report.json по той же схеме:\n{SCHEMA}\n\n\
         Ничего не меняй в проекте, пиши только в $OUT, уложись в 2–3 минуты. Когда записал — ответь: готово.",
        out = out.display(),
    )
}

/// "$ node …/diagram.mjs …" → "Рисую диаграмму" etc.
fn humanize_step(title: &str, out: &Path) -> String {
    let t = title.replace(&out.display().to_string(), "$OUT");
    let known = [
        ("diagram.mjs", "Рисую диаграмму"),
        ("deps.mjs", "Строю граф зависимостей"),
        ("git diff", "Смотрю git diff"),
        ("git status", "Смотрю git status"),
        ("git log", "Смотрю историю git"),
        ("report.json", "Пишу отчёт"),
        ("base.json", "Читаю прошлую карту"),
        ("context.md", "Читаю, что делала сессия"),
        ("session.patch", "Читаю диффы сессии"),
        ("git.patch", "Читаю изменения рабочего дерева"),
        ("shoot.mjs", "Снимаю скриншот"),
        (".mmd", "Составляю схему"),
    ];
    known
        .iter()
        .find(|(needle, _)| t.contains(needle))
        .map(|(_, label)| (*label).to_owned())
        .unwrap_or(t)
}

fn one_paragraph(s: &str, max: usize) -> String {
    let s = s.trim();
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_owned(),
    }
}

fn dirs_home() -> PathBuf {
    std::env::var_os("HOME").map(Into::into).unwrap_or_default()
}

fn resolve(out: &Path, p: &str) -> PathBuf {
    let path = PathBuf::from(p);
    if path.is_absolute() { path } else { out.join(path) }
}

/// `flow.png` → `flow-dark.png` in a dark theme, when diagram.mjs rendered one (`--theme both`).
fn themed_image(path: &Path, dark: bool) -> PathBuf {
    let dark_twin = path.extension().filter(|_| dark).and_then(|ext| {
        let stem = path.file_stem()?.to_string_lossy();
        Some(path.with_file_name(format!("{stem}-dark.{}", ext.to_string_lossy())))
    });
    dark_twin.filter(|twin| twin.exists()).unwrap_or_else(|| path.to_path_buf())
}

/// Lines added / removed across patches.
fn line_stats(patches: &[(String, String)]) -> (usize, usize) {
    let mut added = 0;
    let mut removed = 0;
    for (_, patch) in patches {
        for line in patch.lines() {
            if line.starts_with('+') && !line.starts_with("+++") {
                added += 1;
            } else if line.starts_with('-') && !line.starts_with("---") {
                removed += 1;
            }
        }
    }
    (added, removed)
}

/// Hunks of one file (each starts at its `@@` line).
fn file_hunks(patches: &[(String, String)], file: &str) -> Vec<String> {
    if file.is_empty() {
        return Vec::new();
    }
    let matches = |p: &str| p == file || p.ends_with(&format!("/{file}")) || file.ends_with(&format!("/{p}"));
    let mut hunks = Vec::new();
    for (_, body) in patches.iter().filter(|(p, _)| matches(p)) {
        let mut cur = String::new();
        for line in body.lines() {
            if line.starts_with("@@") && !cur.is_empty() {
                hunks.push(std::mem::take(&mut cur));
            }
            if line.starts_with("@@") || !cur.is_empty() {
                cur.push_str(line);
                cur.push('\n');
            }
        }
        if !cur.is_empty() {
            hunks.push(cur);
        }
    }
    hunks
}

/// The diff of one node: the hunks the report names, else hunks that mention it.
fn node_hunks(node: &map::MapNode, patches: &[(String, String)]) -> Vec<String> {
    let all = file_hunks(patches, &node.file);
    let mut out: Vec<String> = Vec::new();
    for h in &node.hunks {
        if h.contains('\n') {
            out.push(h.clone());
        } else if let Some(found) = all.iter().find(|x| x.lines().next().is_some_and(|head| head.contains(h.trim()) || h.trim().contains(head.trim()))) {
            out.push(found.clone());
        }
    }
    if out.is_empty() {
        let head = node.name.split(['(', ' ', '/']).next().unwrap_or(&node.name).rsplit("::").next().unwrap_or_default().trim();
        if !head.is_empty() {
            out = all.iter().filter(|h| h.contains(head)).take(3).cloned().collect();
        }
    }
    out
}

/// "Claude Sonnet" for `sonnet`, "Codex · GPT-6.2-Sol" for a Codex slug
/// (no slug: Codex's own default model).
pub fn reviewer_name(provider: Provider, model: Option<&str>) -> String {
    match provider {
        Provider::Claude => {
            let model = model.unwrap_or("sonnet");
            let family = ["opus", "sonnet", "haiku", "fable"].into_iter().find(|f| model.contains(f)).unwrap_or(model);
            let mut chars = family.chars();
            let family: String = chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default();
            format!("Claude {family}")
        }
        Provider::Codex => {
            let slug = model.map(str::to_owned).or_else(stepan_verify_core::config::codex_default_model);
            let name = slug.as_ref().map(|slug| {
                stepan_verify_core::config::codex_models().into_iter().find(|(s, _)| s == slug).map_or_else(|| slug.clone(), |(_, n)| n)
            });
            match name {
                Some(n) => format!("Codex · {n}"),
                None => "Codex".into(),
            }
        }
    }
}

fn kind_label(kind: &str) -> String {
    match kind {
        "function" => "функция",
        "component" => "компонент",
        "hook" => "хук",
        "endpoint" => "API",
        "route" => "роут",
        "type" => "тип",
        "table" => "таблица",
        "config" => "конфиг",
        "test" => "тест",
        "file" => "файл",
        other => other,
    }
    .to_owned()
}

fn status_tone(s: NodeStatus, p: &Palette) -> Option<Tone> {
    match s {
        NodeStatus::Added => Some(p.green),
        NodeStatus::Changed => Some(p.amber),
        NodeStatus::Removed => Some(p.red),
        NodeStatus::Context => None,
    }
}

fn status_label(s: NodeStatus) -> &'static str {
    match s {
        NodeStatus::Added => "новое",
        NodeStatus::Changed => "изменено",
        NodeStatus::Removed => "удалено",
        NodeStatus::Context => "не менялось",
    }
}

fn severity_badge(sev: &str, p: &Palette) -> Option<(&'static str, Tone)> {
    match sev {
        "high" => Some(("ВЫСОКИЙ", p.red)),
        "mid" | "medium" => Some(("СРЕДНИЙ", p.amber)),
        "low" => Some(("НИЗКИЙ", Tone { solid: p.faint, soft: p.surface2, text: p.muted })),
        _ => None,
    }
}

fn hhmm(at: Option<chrono::DateTime<chrono::Utc>>) -> String {
    at.map(|t| t.with_timezone(&chrono::Local).format("%H:%M").to_string()).unwrap_or_default()
}

fn edits_word(n: usize) -> &'static str {
    match (n % 10, n % 100) {
        (1, r) if r != 11 => "правка",
        (2..=4, r) if !(12..=14).contains(&r) => "правки",
        _ => "правок",
    }
}

// ---------------------------------------------------------------- rendering


impl Render for ChangesView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let p = Palette::of(&theme);
        let graph = self.report().map(|r| map::build_report(r));
        // Drop a panel that points at a node this iteration does not have.
        if let (Some(Panel::Node(key)), Some(g)) = (&self.panel, &graph) {
            if g.index(key).is_none() {
                self.panel = None;
            }
        }
        if let (Some(Panel::Dto(key)), Some(g)) = (&self.panel, &graph) {
            if g.dto_index(key).is_none() {
                self.panel = None;
            }
        }
        if let (Some(Panel::Group(key)), Some(g)) = (&self.panel, &graph) {
            if g.group_index(key).is_none() {
                self.panel = None;
            }
        }
        // Fit the canvas to its content when the map on screen changes.
        if let Some(g) = &graph {
            let run = self.selected.and_then(|i| self.runs.get(i)).map_or(0, |r| r.n);
            let key = format!("{run}-{:?}", self.mode);
            let size = self.viewport.get().map(|b| (f32::from(b.size.width), f32::from(b.size.height)));
            let resized = size.is_some_and(|(w, h)| (w - self.fit_size.0).abs() > 1. || (h - self.fit_size.1).abs() > 1.);
            if self.fit_key.as_deref() != Some(key.as_str()) || (resized && !self.touched) {
                match size {
                    Some(size) => {
                        let c = map::canvas_layout(g, self.mode);
                        self.view = canvas::View::fit((c.width, c.height), size);
                        self.fit_key = Some(key);
                        self.fit_size = size;
                        self.touched = false;
                    }
                    // Not painted yet: fit on the next frame, when the size is known.
                    None => cx.on_next_frame(window, |_, _, cx| cx.notify()),
                }
            }
        }
        let body: AnyElement = match (&graph, self.selected.and_then(|i| self.runs.get(i)), &self.running) {
            (Some(g), _, _) => {
                // The canvas takes most of the window height; the page scrolls around it.
                let canvas_h = (f32::from(window.viewport_size().height) - 340.).max(440.);
                self.render_report(g, canvas_h, &p, &theme, cx).into_any_element()
            }
            (None, _, Some(r)) if self.selected.is_none() || self.selected.and_then(|i| self.runs.get(i)).is_some_and(|run| run.n == r.n) => {
                self.render_running(&p, &theme, cx).into_any_element()
            }
            (None, run, _) => self.render_failed(run.and_then(|r| r.meta.error.clone()), &p, &theme, cx).into_any_element(),
        };
        let main = v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(
                div().id("changes").flex_1().min_h_0().overflow_y_scroll().child(
                    v_flex()
                        .w_full()
                        .px(px(24.))
                        .pt(px(20.))
                        .pb(px(32.))
                        .gap(px(22.))
                        .child(self.render_header(&p, &theme, cx))
                        .when(self.runs.len() > 1 || self.running.is_some() && !self.runs.is_empty(), |this| this.child(self.render_iterations(&p, &theme, cx)))
                        .child(body)
                        .child(media::render(&self.input, window, cx)),
                ),
            )
            .when_some(graph.as_ref(), |this, g| this.child(self.render_bar(g, &p, &theme, cx)));
        h_flex()
            .size_full()
            .child(main)
            .when_some(self.panel.clone().zip(graph), |this, (panel, g)| this.child(self.render_panel(panel, &g, &p, &theme, window, cx)))
    }
}

impl ChangesView {
    fn render_header(&self, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let run = self.selected.and_then(|i| self.runs.get(i));
        let r = self.report();
        let reviewer = run.map(|r| r.meta.reviewer.clone()).filter(|s| !s.is_empty()).unwrap_or_else(|| reviewer_name(self.input.provider, None));
        let eyebrow = match run {
            Some(run) if r.is_some() => format!(
                "Карта изменений · {reviewer} · итерация {} · {}",
                run.n,
                run.meta.at.map(ago).unwrap_or_default()
            ),
            _ => format!("Карта изменений · {reviewer}"),
        };
        let out = self.out.clone();
        let can_batch = self.running.is_none() && self.runs.iter().any(|r| r.ok().is_some()) && self.news;
        v_flex()
            .gap(px(8.))
            .child(
                h_flex()
                    .gap(px(8.))
                    .child(div().flex_1().min_w_0().child(ui::eyebrow(eyebrow, p, theme).truncate()))
                    .child(
                        h_flex()
                            .flex_shrink_0()
                            .gap(px(8.))
                            .px(px(10.))
                            .h(px(30.))
                            .rounded(px(8.))
                            .border_1()
                            .border_color(if self.follow { ui::mix(p.blue.solid, p.border, 0.45) } else { p.border })
                            .bg(if self.follow { p.blue.soft } else { p.surface })
                            .child(Switch::new("changes-follow").checked(self.follow).on_click(cx.listener(|this, on: &bool, _, cx| this.toggle_follow(*on, cx))))
                            .child(div().text_size(px(12.5)).font_weight(FontWeight::SEMIBOLD).text_color(if self.follow { p.blue.text } else { p.ink }).child("Следить за сессией"))
                            .when(self.follow, |d| d.child(ui::mono(if self.pending { "· батч в очереди" } else { "· после каждого хода" }, theme).text_size(px(10.5)).text_color(p.blue.text))),
                    )
                    .when(can_batch, |d| {
                        d.child(
                            PressButton::new("changes-batch")
                                .small()
                                .icon(IconName::Plus)
                                .label("Дополнить")
                                .tooltip("Обновить карту только по новым ходам сессии")
                                .on_click(cx.listener(|this, _, _, cx| this.launch(RunKind::Batch, cx))),
                        )
                    })
                    .child(
                        PressButton::new("changes-folder")
                            .small()
                            .ghost()
                            .icon(IconName::FolderOpen)
                            .tooltip("Папка с прогонами")
                            .on_click(move |_, window, cx| crate::preview::open_in_app(out.clone(), None, window, cx)),
                    )
                    .child(
                        PressButton::new("changes-refresh")
                            .small()
                            .icon(IconName::RefreshCw)
                            .label("Пересобрать")
                            .disabled(self.running.is_some())
                            .on_click(cx.listener(|this, _, _, cx| this.start(cx))),
                    ),
            )
            .child(
                div()
                    .text_size(px(24.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .line_height(px(30.))
                    .text_color(p.ink)
                    .child(r.map(|r| r.title.clone()).filter(|t| !t.is_empty()).unwrap_or_else(|| self.input.title.clone())),
            )
    }

    /// One chip per run: the history of the map.
    fn render_iterations(&self, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let chip = |id: SharedString, active: bool| {
            h_flex()
                .id(id)
                .flex_shrink_0()
                .gap(px(6.))
                .px(px(10.))
                .h(px(30.))
                .rounded(px(8.))
                .border_1()
                .cursor_pointer()
                .map(|d| if active { d.bg(p.surface).border_color(p.ink).shadow(ui::card_shadow()) } else { d.bg(p.surface2).border_color(p.border).hover(|s| s.border_color(p.border_strong)) })
        };
        let mono = theme.mono_font_family.clone();
        let running_n = self.running.as_ref().map(|r| r.n);
        v_flex()
            .gap(px(8.))
            .child(ui::section_title("Итерации", Some(self.runs.len().to_string()), p, theme).child(ui::mono("каждый «Пересобрать» и каждый батч — отдельный снимок карты", theme).text_color(p.faint)))
            .child(
                div().id("iterations-scroll").overflow_x_scroll().child(
                    h_flex().gap(px(6.)).pb(px(2.)).children(self.runs.iter().enumerate().map(|(i, run)| {
                        let active = self.selected == Some(i);
                        let is_running = running_n == Some(run.n);
                        let failed = run.ok().is_none() && !is_running;
                        let kind = match run.meta.kind {
                            RunKind::Full => "полный".to_owned(),
                            RunKind::Batch => format!("батч · +{} {}", run.meta.edits, edits_word(run.meta.edits)),
                        };
                        let decisions = run.ok().map_or(0, |r| r.decisions.len());
                        chip(SharedString::from(format!("iter-{}", run.n)), active)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.selected = Some(i);
                                cx.notify();
                            }))
                            .child(div().font_family(mono.clone()).text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).child(format!("#{}", run.n)))
                            .child(div().text_size(px(12.)).text_color(p.fg).child(kind))
                            .child(div().font_family(mono.clone()).text_size(px(11.)).text_color(p.faint).child(hhmm(run.meta.at)))
                            .when(is_running, |d| d.child(Spinner::new().xsmall().color(p.blue.solid)))
                            .when(failed, |d| d.child(ui::badge("ошибка", p.red)))
                            .when(decisions > 0, |d| d.child(ui::badge(format!("{decisions} реш."), p.blue)))
                    })),
                ),
            )
    }

    fn render_running(&self, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(r) = &self.running else { return div().into_any_element() };
        let secs = r.started.elapsed().as_secs();
        let shown: Vec<&String> = r.steps.iter().rev().take(6).rev().collect();
        let count = shown.len();
        let title = match r.kind {
            RunKind::Full => "Ревьюер собирает карту сессии",
            RunKind::Batch => "Ревьюер дополняет карту новой порцией",
        };
        v_flex()
            .gap(px(20.))
            .child(
                ui::frame("changes-running", p)
                    .child(
                        h_flex()
                            .px(px(10.))
                            .pt(px(7.))
                            .pb(px(8.))
                            .gap_2()
                            .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).child(title))
                            .child(ui::dot_badge(format!("#{} · {}", r.n, r.model), p.blue))
                            .child(div().flex_1())
                            .child(PressButton::new("changes-cancel").xsmall().ghost().label("Отменить").on_click(cx.listener(|this, _, _, cx| this.cancel(cx)))),
                    )
                    .child(ui::inner(p).children(shown.into_iter().enumerate().map(|(i, s)| {
                        let last = i + 1 == count;
                        h_flex()
                            .px(px(12.))
                            .py(px(7.))
                            .gap(px(9.))
                            .when(last, |d| d.bg(p.surface2).border_y_1().border_color(p.border))
                            .child(if last {
                                Spinner::new().xsmall().color(p.blue.solid).into_any_element()
                            } else {
                                Icon::new(IconName::CircleCheck).xsmall().text_color(p.faint).into_any_element()
                            })
                            .child(div().flex_1().min_w_0().truncate().text_size(px(13.)).text_color(if last { p.ink } else { p.fg }).child(s.clone()))
                    })).when(count == 0, |this| this.child(div().px(px(12.)).py(px(8.)).text_size(px(13.)).text_color(p.muted).child("Читает сессию и диффы…"))))
                    .child(h_flex().px(px(10.)).pt(px(7.)).pb(px(4.)).child(ui::mono(format!("обычно 1–3 минуты · {}:{:02}", secs / 60, secs % 60), theme).text_color(p.muted))),
            )
            .child(Skeleton::new().w(relative(0.7)).h(px(14.)).rounded(px(4.)))
            .child(Skeleton::new().secondary().w_full().h(px(320.)).rounded(px(12.)))
            .into_any_element()
    }

    fn render_failed(&self, err: Option<String>, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex().gap_3().child(ui::eyebrow("Карта изменений", p, theme)).child(
            ui::card(p)
                .p(px(16.))
                .gap(px(12.))
                .child(
                    h_flex()
                        .gap_2()
                        .items_start()
                        .child(Icon::new(IconName::TriangleAlert).small().text_color(p.red.solid))
                        .child(div().flex_1().text_size(px(13.5)).text_color(p.fg).child(err.unwrap_or_else(|| "Отчёта пока нет".into()))),
                )
                .child(
                    h_flex().child(
                        PressButton::new("changes-retry")
                            .primary()
                            .small()
                            .icon(IconName::RefreshCw)
                            .label("Собрать карту")
                            .disabled(self.running.is_some())
                            .on_click(cx.listener(|this, _, _, cx| this.start(cx))),
                    ),
                ),
        )
    }

    fn render_report(&self, g: &Graph, canvas_h: f32, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(r) = self.report() else { return div().into_any_element() };
        let patches = self.patches();
        let files = patches.iter().map(|(f, _)| f.as_str()).collect::<std::collections::BTreeSet<_>>().len();
        let (plus, minus) = line_stats(&patches);
        let dir = self.run_dir();
        // Nodes that are new or different compared with the previous good iteration.
        let fresh: HashSet<String> = self
            .selected
            .and_then(|i| self.runs[..i].iter().rev().find_map(Run::ok))
            .map(|prev| g.changed_since(&map::build_report(prev)))
            .unwrap_or_default();
        v_flex()
            .gap(px(22.))
            .when(self.running.as_ref().is_some_and(|run| run.kind == RunKind::Batch), |this| {
                let run = self.running.as_ref().unwrap();
                this.child(
                    h_flex()
                        .gap_2()
                        .px(px(12.))
                        .py(px(8.))
                        .rounded(px(8.))
                        .bg(p.blue.soft)
                        .text_size(px(12.5))
                        .text_color(p.blue.text)
                        .child(Spinner::new().xsmall().color(p.blue.solid))
                        .child(format!("Батч #{} собирается · {} · {}", run.n, run.model, run.steps.last().cloned().unwrap_or_else(|| "читает новые ходы".into()))),
                )
            })
            // The canvas first: it is what you look at; the summary and numbers follow.
            .child(self.render_map_section(g, r, &fresh, canvas_h, p, theme, cx))
            .when(!r.summary.is_empty(), |this| {
                this.child(
                    div()
                        .max_w(px(780.))
                        .text_size(px(14.))
                        .line_height(px(22.))
                        .text_color(p.fg)
                        .child(TextView::markdown(SharedString::from(format!("changes-summary-{}", dir.display())), r.summary.clone()).selectable(true)),
                )
            })
            .child(metrics(files, plus, minus, &r.code, r.risks.len(), p, theme))

            .when_some(self.render_decisions(g, p, theme, cx), |this, d| this.child(d))
            .child(
                h_flex()
                    .gap(px(14.))
                    .items_start()
                    .child(div().flex_1().min_w_0().child(self.render_files(g, &patches, p, theme, cx)))
                    .child(div().flex_1().min_w_0().child(self.render_risks(g, r, p, theme, cx))),
            )
            .when_some(r.flow.clone(), |this, d| this.child(self.render_diagram("flow".into(), &d, &dir, p, theme)))
            .when_some(r.structure.clone(), |this, d| this.child(self.render_diagram("structure".into(), &d, &dir, p, theme)))
            .children(r.diagrams.iter().enumerate().map(|(i, d)| self.render_diagram(format!("extra-{i}"), d, &dir, p, theme)))
            .when(!r.ui.is_empty(), |this| {
                this.child(
                    v_flex()
                        .gap(px(12.))
                        .child(ui::section_title("Интерфейс", None, p, theme).child(ui::mono("было → стало", theme).text_color(p.faint)))
                        .children(r.ui.iter().enumerate().map(|(i, u)| self.render_ui_pair(i, u, &dir, p))),
                )
            })
            .into_any_element()
    }

    fn render_map_section(&self, g: &Graph, r: &Report, fresh: &HashSet<String>, canvas_h: f32, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let mode_btn = |key: &'static str, label: &'static str, mode: MapMode, cx: &mut Context<Self>| {
            let active = self.mode == mode && !self.show_list;
            h_flex()
                .id(key)
                .px(px(11.))
                .h(px(26.))
                .rounded(px(6.))
                .cursor_pointer()
                .text_size(px(12.5))
                .font_weight(if active { FontWeight::SEMIBOLD } else { FontWeight::MEDIUM })
                .map(|d| if active { d.bg(p.surface).text_color(p.ink).shadow(ui::card_shadow()) } else { d.text_color(p.muted).hover(|s| s.text_color(p.ink)) })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.mode = mode;
                    this.show_list = false;
                    cx.notify();
                }))
                .child(label)
        };
        let count = |s: NodeStatus| g.nodes.iter().filter(|n| n.status == s && !n.implicit).count();
        let removed_links = g.edges.iter().filter(|e| e.kind == EdgeKind::Removed).count();
        let legend = |label: String, tone: Option<Tone>, dashed: bool| {
            h_flex()
                .gap(px(5.))
                .text_size(px(12.))
                .text_color(p.muted)
                .child(
                    div().size(px(10.)).rounded(px(3.)).border_1().when(dashed, |d| d.border_dashed()).map(|d| match tone {
                        Some(t) => d.border_color(t.solid).bg(t.soft),
                        None => d.border_color(p.border_strong).bg(p.surface),
                    }),
                )
                .child(label)
        };
        let has_nodes = g.nodes.iter().any(|n| !n.implicit);
        v_flex()
            .gap(px(10.))
            .child(
                h_flex()
                    .gap(px(10.))
                    .flex_wrap()
                    .child(
                        h_flex()
                            .p(px(2.))
                            .gap(px(2.))
                            .rounded(px(8.))
                            .border_1()
                            .border_color(p.border)
                            .bg(p.sunken)
                            .child(mode_btn("map-before", "Было", MapMode::Before, cx))
                            .child(mode_btn("map-after", "Стало", MapMode::After, cx))
                            .child(mode_btn("map-diff", "Разница", MapMode::Diff, cx))
                            .child(
                                h_flex()
                                    .id("map-list")
                                    .px(px(11.))
                                    .h(px(26.))
                                    .rounded(px(6.))
                                    .cursor_pointer()
                                    .text_size(px(12.5))
                                    .map(|d| if self.show_list { d.bg(p.surface).text_color(p.ink).font_weight(FontWeight::SEMIBOLD).shadow(ui::card_shadow()) } else { d.text_color(p.muted).hover(|s| s.text_color(p.ink)) })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.show_list = !this.show_list;
                                        cx.notify();
                                    }))
                                    .child("Списком"),
                            ),
                    )
                    .child(legend(format!("новое · {}", count(NodeStatus::Added)), Some(p.green), false))
                    .child(legend(format!("изменено · {}", count(NodeStatus::Changed)), Some(p.amber), false))
                    .child(legend(format!("удалено · {}", count(NodeStatus::Removed)), Some(p.red), false))
                    .when(removed_links > 0, |d| d.child(legend(format!("убрана связь · {removed_links}"), Some(p.red), true)))
                    .child(legend("не менялось".into(), None, true))
                    .when(!fresh.is_empty(), |d| {
                        d.child(h_flex().gap(px(5.)).text_size(px(12.)).text_color(p.blue.text).child(div().size(px(8.)).rounded_full().bg(p.blue.solid)).child(format!("новое в итерации · {}", fresh.len())))
                    }),
            )
            .map(|this| {
                if self.show_list || !has_nodes {
                    this.child(render_columns(&r.code, p, theme))
                } else {
                    let dtos: Vec<(usize, &map::MapDto)> = g.visible_dtos(self.mode).collect();
                    let dto_focus = self
                        .hovered_dto
                        .as_ref()
                        .or(match &self.panel {
                            Some(Panel::Dto(k)) => Some(k),
                            _ => None,
                        })
                        .and_then(|k| g.dto_index(k));
                    this.child(self.render_map(g, r, fresh, canvas_h, p, theme, cx))
                        .when(!dtos.is_empty(), |this| this.child(self.render_dto_lane(&dtos, g, fresh, dto_focus, p, theme, cx)))
                }
            })
    }

    /// The canvas: frames by meaning, cards inside, curved links (dashed
    /// «проверяет» from tests) with DTO labels. Drag to pan, ⌘/Ctrl + wheel or
    /// pinch to zoom around the pointer, double click to fit.
    fn render_map(&self, g: &Graph, r: &Report, fresh: &HashSet<String>, height: f32, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        use map::NODE_W;
        let mode = self.mode;
        let cv = map::canvas_layout(g, mode);
        let v = self.view;
        let z = v.zoom;
        let sx = move |x: f32| v.x + x * z;
        let sy = move |y: f32| v.y + y * z;
        let zl = move |l: f32| l * z;
        // Level of detail: small zoom drops the fine print.
        let detail = z >= 0.42;
        let text = z >= 0.3;
        let selected = match &self.panel {
            Some(Panel::Node(k)) => Some(k.clone()),
            _ => None,
        };
        let group_focus: Option<usize> = match &self.panel {
            Some(Panel::Group(k)) => g.group_index(k),
            _ => None,
        };
        let dto_focus: Option<usize> = self
            .hovered_dto
            .as_ref()
            .or(match &self.panel {
                Some(Panel::Dto(k)) => Some(k),
                _ => None,
            })
            .and_then(|k| g.dto_index(k));
        let focus = self.hovered.clone().or(selected.clone());
        let focus_ix = if dto_focus.is_some() { None } else { focus.as_ref().and_then(|k| g.index(k)) };
        let lit: Option<HashSet<usize>> = match (dto_focus, focus_ix, group_focus) {
            (Some(d), _, _) => Some(g.dtos[d].users.iter().copied().collect()),
            (None, Some(f), _) => {
                let mut s: HashSet<usize> = g.edges.iter().filter(|e| (e.from == f || e.to == f) && g.edge_visible(e, mode)).flat_map(|e| [e.from, e.to]).collect();
                s.insert(f);
                Some(s)
            }
            (None, None, Some(gi)) => {
                let members: HashSet<usize> = g.groups[gi].nodes.iter().copied().collect();
                let mut s = members.clone();
                s.extend(g.edges.iter().filter(|e| members.contains(&e.from) || members.contains(&e.to)).flat_map(|e| [e.from, e.to]));
                Some(s)
            }
            _ => None,
        };
        let risk_count = |n: &map::MapNode| r.risks.iter().filter(|risk| risk.nodes.iter().any(|k| map::normalize(k) == n.key) || (!risk.id.is_empty() && n.risk_ids.contains(&risk.id))).count();
        let mono = theme.mono_font_family.clone();
        let has_tests = g.has_tests();

        // Links in screen space: (curve, color, width, dashed); DTO labels at their middle.
        let mut lines: Vec<([(f32, f32); 4], Hsla, f32, bool)> = Vec::new();
        let mut labels: Vec<((f32, f32), usize, bool, bool)> = Vec::new();
        for e in &g.edges {
            if !g.edge_visible(e, mode) {
                continue;
            }
            let (Some(from), Some(to)) = (cv.boxes[e.from], cv.boxes[e.to]) else { continue };
            let world = map::link_curve(from, to);
            let curve = world.map(|(x, y)| (sx(x), sy(y)));
            let base = match (mode, e.kind, e.verify) {
                (_, _, true) => p.blue.solid.opacity(0.75),
                (MapMode::Diff, EdgeKind::New, _) => p.green.solid,
                (MapMode::Diff, EdgeKind::Removed, _) => p.red.solid,
                _ => p.border_strong,
            };
            let dashed = e.verify || (mode == MapMode::Diff && e.kind == EdgeKind::Removed);
            let hot = match dto_focus {
                Some(d) => e.via == Some(d),
                None => match (focus_ix, group_focus) {
                    (Some(f), _) => e.from == f || e.to == f,
                    (None, Some(gi)) => g.nodes[e.from].group == gi || g.nodes[e.to].group == gi,
                    _ => false,
                },
            };
            let any_focus = dto_focus.is_some() || focus_ix.is_some() || group_focus.is_some();
            let color = match (any_focus, hot) {
                (true, true) if base == p.border_strong => p.ink.opacity(0.7),
                (true, true) => base,
                (true, false) => base.opacity(0.18),
                _ => base,
            };
            lines.push((curve, color, if hot { 2.2 } else { 1.4 }, dashed));
            if let Some(d) = e.via.filter(|&d| g.dtos[d].visible(mode) && text) {
                labels.push((map::curve_mid(&curve), d, hot, any_focus && !hot));
            }
        }
        let viewport = self.viewport.clone();
        let dot = p.border_strong.opacity(0.35);
        let grid = (v.x, v.y, z);
        let links = canvas(
            move |bounds, _, _| {
                viewport.set(Some(bounds));
                lines
            },
            move |bounds, lines, window, _| {
                let o = bounds.origin;
                // Dot grid that moves with the canvas (every 24 world px, thinned when zoomed out).
                let (gx, gy, gz) = grid;
                let mut step = 24. * gz;
                while step < 14. {
                    step *= 2.;
                }
                let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
                let (mut x, start_y) = (gx.rem_euclid(step), gy.rem_euclid(step));
                if (w / step) * (h / step) < 6000. {
                    while x < w {
                        let mut y = start_y;
                        while y < h {
                            window.paint_quad(fill(Bounds::new(point(px(x - 0.75), px(y - 0.75)) + o, size(px(1.5), px(1.5))), dot));
                            y += step;
                        }
                        x += step;
                    }
                }
                let pt = |(x, y): (f32, f32)| point(px(x), px(y)) + o;
                for (c, color, width, dashed) in lines {
                    let (a, c1, c2, b) = (pt(c[0]), pt(c[1]), pt(c[2]), pt(c[3]));
                    let mut path = PathBuilder::stroke(px(width));
                    if dashed {
                        path = path.dash_array(&[px(4.), px(3.)]);
                    }
                    path.move_to(a);
                    path.cubic_bezier_to(b, c1, c2);
                    if let Ok(path) = path.build() {
                        window.paint_path(path, color);
                    }
                    let (dx, dy) = (f32::from(b.x - c2.x), f32::from(b.y - c2.y));
                    let len = (dx * dx + dy * dy).sqrt().max(0.001);
                    let (ux, uy) = (dx / len, dy / len);
                    let back = point(b.x - px(ux * 7.), b.y - px(uy * 7.));
                    let side = (px(-uy * 3.5), px(ux * 3.5));
                    let mut head = PathBuilder::fill();
                    head.move_to(b);
                    head.line_to(point(back.x + side.0, back.y + side.1));
                    head.line_to(point(back.x - side.0, back.y - side.1));
                    head.close();
                    if let Ok(head) = head.build() {
                        window.paint_path(head, color);
                    }
                }
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();

        // Frames: what the cards do together.
        let frames = g.groups.iter().enumerate().filter_map(|(gi, gr)| cv.groups[gi].map(|b| (gi, gr, b))).map(|(gi, gr, b)| {
            let (tone, dashed) = match gr.kind {
                map::GroupKind::Feature => (Some(p.accent), false),
                map::GroupKind::Tests => (Some(p.blue), false),
                map::GroupKind::Helper | map::GroupKind::Infra | map::GroupKind::File => (None, false),
                map::GroupKind::Context => (None, true),
            };
            let border = tone.map_or(p.border_strong, |t| ui::mix(t.solid, p.border, 0.45));
            let bg = match (gr.kind, tone) {
                (map::GroupKind::Context, _) => transparent_black(),
                (_, Some(t)) => ui::mix(t.soft, p.sunken, 0.32),
                _ => ui::mix(p.surface, p.sunken, 0.35),
            };
            let key = gr.key.clone();
            let picked = group_focus == Some(gi);
            let count = gr.nodes.iter().filter(|&&i| cv.boxes[i].is_some()).count();
            div()
                .id(SharedString::from(format!("frame-{gi}-{}", gr.key)))
                .absolute()
                .left(px(sx(b.x)))
                .top(px(sy(b.y)))
                .w(px(zl(b.w)))
                .h(px(zl(b.h)))
                .rounded(px(zl(14.).max(4.)))
                .border_1()
                .when(dashed, |d| d.border_dashed())
                .border_color(if picked { p.ink } else { border })
                .bg(bg)
                .when(text, |d| {
                    d.child(
                        v_flex()
                            .id(SharedString::from(format!("frame-head-{gi}")))
                            .absolute()
                            .left(px(zl(map::GROUP_PAD)))
                            .right(px(zl(map::GROUP_PAD)))
                            .top(px(zl(12.)))
                            .gap(px(zl(3.)))
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if this.dragged {
                                    return;
                                }
                                cx.stop_propagation();
                                this.panel = Some(Panel::Group(key.clone()));
                                cx.notify();
                            }))
                            .child(
                                h_flex()
                                    .gap(px(zl(7.)))
                                    .h(px(zl(20.)))
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_size(px(zl(15.)))
                                            .line_height(px(zl(20.)))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(tone.map_or(p.ink, |t| t.text))
                                            .child(gr.title.clone()),
                                    )
                                    .child(
                                        div()
                                            .flex_shrink_0()
                                            .px(px(zl(5.)))
                                            .rounded(px(zl(4.)))
                                            .border_1()
                                            .border_color(border)
                                            .font_family(mono.clone())
                                            .text_size(px(zl(10.)))
                                            .text_color(tone.map_or(p.muted, |t| t.text))
                                            .child(format!("{} · {count}", gr.kind.label())),
                                    ),
                            )
                            .when(!gr.summary.is_empty(), |d| {
                                d.child(div().text_size(px(zl(12.))).line_height(px(zl(16.))).line_clamp(2).text_color(p.muted).child(gr.summary.clone()))
                            }),
                    )
                })
        });

        let nodes = g.nodes.iter().enumerate().filter_map(|(i, n)| cv.boxes[i].map(|b| (i, n, b))).map(|(i, n, (x, y, h))| {
            let tone = if mode == MapMode::Diff || (mode == MapMode::After && n.status == NodeStatus::Added) || (mode == MapMode::Before && n.status == NodeStatus::Removed) {
                status_tone(n.status, p)
            } else {
                None
            };
            let is_sel = selected.as_deref() == Some(n.key.as_str());
            let dim = lit.as_ref().is_some_and(|s| !s.contains(&i));
            let ctx = n.status == NodeStatus::Context && n.test.is_none();
            let risks = risk_count(n);
            let is_fresh = fresh.contains(&n.key);
            let key = n.key.clone();
            let hover_key = n.key.clone();
            let role = n.role();
            let sig = map::signature_parts(n, mode);
            let sig_changed = mode == MapMode::Diff && n.signature_changed();
            let untested = has_tests && g.untested(i);
            let file = n.file.rsplit('/').next().unwrap_or_default().to_owned();
            let test_result = n.test.as_ref().map(|t| t.result.clone()).filter(|r| !r.is_empty());
            div()
                .id(SharedString::from(format!("node-{i}-{}", n.key)))
                .absolute()
                .left(px(sx(x)))
                .top(px(sy(y)))
                .w(px(zl(NODE_W)))
                .h(px(zl(h)))
                .rounded(px(zl(9.).max(3.)))
                .border_1()
                .overflow_hidden()
                .px(px(zl(11.)))
                .pt(px(zl(8.)))
                .flex()
                .flex_col()
                .gap(px(zl(4.)))
                .cursor_pointer()
                .when(dim, |d| d.opacity(0.25))
                .map(|d| {
                    if ctx {
                        d.border_dashed().border_color(p.border_strong).bg(ui::mix(p.surface, p.sunken, 0.55))
                    } else {
                        d.bg(p.surface).shadow(ui::card_shadow()).border_color(match tone {
                            Some(t) => ui::mix(t.solid, p.border, 0.5),
                            None => p.border,
                        })
                    }
                })
                .when(is_sel, |d| d.border_color(p.ink).shadow(vec![BoxShadow { color: p.ink.opacity(0.12), offset: point(px(0.), px(0.)), blur_radius: px(0.), spread_radius: px(3.), inset: false }]))
                .hover(|s| s.border_color(p.border_strong))
                .on_hover(cx.listener(move |this, on: &bool, _, cx| {
                    if this.drag.is_some_and(|d| d.moved) {
                        return;
                    }
                    if *on {
                        this.hovered = Some(hover_key.clone());
                    } else if this.hovered.as_deref() == Some(hover_key.as_str()) {
                        this.hovered = None;
                    }
                    cx.notify();
                }))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if this.dragged {
                        return;
                    }
                    cx.stop_propagation();
                    this.panel = Some(Panel::Node(key.clone()));
                    cx.notify();
                }))
                .when_some(tone.filter(|_| !ctx), |d, t| d.child(div().absolute().left_0().top_0().bottom_0().w(px(zl(4.).max(2.))).bg(t.solid)))
                .when(text, |d| {
                    d.child(
                        h_flex()
                            .h(px(zl(15.)))
                            .gap(px(zl(5.)))
                            // New in this iteration.
                            .when(is_fresh, |d| d.child(div().flex_shrink_0().size(px(zl(7.)).max(px(4.))).rounded_full().bg(p.blue.solid)))
                            .child(Icon::empty().path(role.icon()).with_size(px(zl(12.))).text_color(if role == map::Role::Test { p.blue.solid } else { p.muted }))
                            .child(div().flex_1().min_w_0().truncate().font_family(mono.clone()).text_size(px(zl(10.))).text_color(p.muted).child(role.label().to_uppercase()))
                            .when_some(status_tone(n.status, p).filter(|_| n.status != NodeStatus::Context), |d, t| {
                                d.child(div().flex_shrink_0().px(px(zl(5.))).rounded(px(zl(4.))).bg(t.soft).text_size(px(zl(10.))).font_weight(FontWeight::SEMIBOLD).text_color(t.text).child(status_label(n.status)))
                            })
                            .when(risks > 0, |d| {
                                d.child(
                                    div()
                                        .flex_shrink_0()
                                        .px(px(zl(5.)))
                                        .rounded(px(zl(4.)))
                                        .bg(p.red.solid)
                                        .font_family(mono.clone())
                                        .text_size(px(zl(10.)))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(gpui_kit::white())
                                        .child(format!("⚠ {risks}")),
                                )
                            })
                            .when_some(test_result.clone(), |d, res| {
                                let (label, t) = match res.as_str() {
                                    "pass" | "passed" | "ok" => ("✓ проходит", p.green),
                                    "fail" | "failed" => ("✕ падает", p.red),
                                    _ => ("не запускался", p.amber),
                                };
                                d.child(div().flex_shrink_0().px(px(zl(5.))).rounded(px(zl(4.))).bg(t.soft).text_size(px(zl(10.))).text_color(t.text).child(label))
                            }),
                    )
                    .child(
                        div()
                            .text_size(px(zl(13.)))
                            .line_height(px(zl(map::TITLE_LINE)))
                            .line_clamp(map::TITLE_MAX_LINES)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(if ctx { p.muted } else { p.ink })
                            .when(n.status == NodeStatus::Removed, |d| d.line_through())
                            .child(n.title().to_owned()),
                    )
                    .when(map::shows_code_name(n), |d| {
                        d.child(div().mt(px(zl(-2.))).truncate().font_family(mono.clone()).text_size(px(zl(11.))).line_height(px(zl(map::NAME_LINE))).text_color(p.muted).child(n.name.clone()))
                    })
                    .when(!sig.is_empty() && detail, |d| {
                        d.child(
                            div()
                                .mt(px(zl(1.)))
                                .font_family(mono.clone())
                                .text_size(px(zl(10.5)))
                                .line_height(px(zl(map::SIG_LINE)))
                                .line_clamp(map::SIG_MAX_LINES)
                                .text_color(if sig_changed { p.fg } else { p.faint })
                                .child(signature_text(&sig, p)),
                        )
                    })
                    .when(detail, |d| {
                        d.child(
                            h_flex()
                                .mt(px(zl(1.)))
                                .h(px(zl(15.)))
                                .gap(px(zl(6.)))
                                .text_size(px(zl(10.5)))
                                .child(div().flex_1().min_w_0().truncate().text_color(p.faint).child(file))
                                .when(untested, |d| {
                                    d.child(div().flex_shrink_0().px(px(zl(5.))).rounded(px(zl(4.))).border_1().border_dashed().border_color(p.amber.solid).text_color(p.amber.text).child("без тестов"))
                                }),
                        )
                    })
                })

        });

        // DTO labels on links: what travels along them.
        let pills = labels.into_iter().enumerate().map(|(li, ((mx, my), d, hot, dim))| {
            let dto = &g.dtos[d];
            let tone = status_tone(dto.status, p).filter(|_| mode != MapMode::Before || dto.status == NodeStatus::Removed);
            let w = zl(map::pill_width(&dto.name));
            let key = dto.key.clone();
            let hover_key = dto.key.clone();
            div()
                .id(SharedString::from(format!("via-{li}-{}", dto.key)))
                .absolute()
                .left(px(mx - w / 2.))
                .top(px(my - zl(9.)))
                .w(px(w))
                .h(px(zl(18.)))
                .px(px(zl(6.)))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(zl(4.)))
                .border_1()
                .cursor_pointer()
                .when(dim, |d| d.opacity(0.3))
                .map(|d| match tone {
                    Some(t) => d.bg(t.soft).border_color(ui::mix(t.solid, p.border, 0.5)).text_color(t.text),
                    None => d.bg(p.surface).border_color(p.border_strong).text_color(p.fg),
                })
                .when(hot, |d| d.border_color(p.ink))
                .font_family(mono.clone())
                .text_size(px(zl(10.)))
                .font_weight(FontWeight::MEDIUM)
                .on_hover(cx.listener(move |this, on: &bool, _, cx| {
                    if *on {
                        this.hovered_dto = Some(hover_key.clone());
                    } else if this.hovered_dto.as_deref() == Some(hover_key.as_str()) {
                        this.hovered_dto = None;
                    }
                    cx.notify();
                }))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if this.dragged {
                        return;
                    }
                    cx.stop_propagation();
                    this.panel = Some(Panel::Dto(key.clone()));
                    cx.notify();
                }))
                .child(div().truncate().child(dto.name.clone()))
        });

        let pct = (z * 100.).round() as i32;
        let world = (cv.width, cv.height);
        let controls = h_flex()
            .absolute()
            .right(px(12.))
            .top(px(8.))
            .gap(px(4.))
            .p(px(4.))
            .rounded(px(9.))
            .border_1()
            .border_color(p.border)
            .bg(p.surface)
            .shadow(ui::card_shadow())
            .child(PressButton::new("zoom-out").ghost().xsmall().icon(IconName::Minus).tooltip("Уменьшить (⌘ + колесо)").on_click(cx.listener(|this, _, _, cx| this.zoom_center(1. / 1.25, cx))))
            .child(
                div()
                    .id("zoom-100")
                    .w(px(46.))
                    .text_center()
                    .cursor_pointer()
                    .font_family(mono.clone())
                    .text_size(px(11.5))
                    .text_color(p.fg)
                    .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new("100%").build(window, cx))
                    .on_click(cx.listener(|this, _, _, cx| {
                        let f = 1. / this.view.zoom;
                        this.zoom_center(f, cx);
                    }))
                    .child(format!("{pct}%")),
            )
            .child(PressButton::new("zoom-in").ghost().xsmall().icon(IconName::Plus).tooltip("Увеличить").on_click(cx.listener(|this, _, _, cx| this.zoom_center(1.25, cx))))
            .child(div().w(px(1.)).h(px(16.)).bg(p.border))
            .child(PressButton::new("zoom-fit").ghost().xsmall().icon(Icon::empty().path("icons/scan.svg")).label("Вписать").on_click(cx.listener(move |this, _, _, cx| this.fit(world, cx))));
        let legend_role = |role: map::Role| h_flex().gap(px(4.)).child(Icon::empty().path(role.icon()).xsmall().text_color(p.muted)).child(role.label());
        let swatch = |bg: Hsla, border: Hsla, label: &'static str| h_flex().gap(px(4.)).child(div().size(px(9.)).rounded(px(2.)).bg(bg).border_1().border_color(border)).child(label);
        let legend = v_flex()
            .absolute()
            .left(px(12.))
            .bottom(px(12.))
            .gap(px(6.))
            .px(px(10.))
            .py(px(8.))
            .rounded(px(9.))
            .border_1()
            .border_color(p.border)
            .bg(p.surface.opacity(0.94))
            .text_size(px(11.))
            .text_color(p.muted)
            .child(
                h_flex()
                    .gap(px(10.))
                    .child(legend_role(map::Role::Component))
                    .child(legend_role(map::Role::Function))
                    .child(legend_role(map::Role::Hook))
                    .child(legend_role(map::Role::Type))
                    .child(legend_role(map::Role::Api))
                    .child(legend_role(map::Role::Test)),
            )
            .child(
                h_flex()
                    .gap(px(10.))
                    .child(swatch(p.accent.soft, p.accent.solid, "функционал"))
                    .child(swatch(p.surface, p.border_strong, "помогает"))
                    .child(swatch(p.blue.soft, p.blue.solid, "тесты"))
                    .child(h_flex().gap(px(4.)).child(div().w(px(14.)).h(px(0.)).border_t_1().border_dashed().border_color(p.blue.solid)).child("проверяет")),
            );
        let hint = h_flex()
            .absolute()
            .left(px(12.))
            .top(px(10.))
            .gap(px(8.))
            .child(ui::mono("тащи — двигать · ⌘ + колесо или щипок — масштаб · двойной клик — вписать", theme).text_size(px(10.5)).text_color(p.faint))
            .when(!has_tests && g.nodes.iter().any(|n| !n.implicit && n.status != NodeStatus::Context), |d| {
                d.child(ui::badge("тестов нет — изменения ничем не проверены", p.amber).text_size(px(10.5)))
            });
        let grabbing = self.drag.is_some_and(|d| d.moved);

        div()
            .id("map-canvas")
            .relative()
            .w_full()
            .h(px(height))
            .rounded(px(12.))
            .border_1()
            .border_color(p.border)
            .bg(p.sunken)
            .overflow_hidden()
            .cursor(if grabbing { CursorStyle::ClosedHand } else { CursorStyle::OpenHand })
            .on_mouse_down(MouseButton::Left, cx.listener(|this, e: &MouseDownEvent, _, cx| this.drag_start(e.position, cx)))
            .on_mouse_down(MouseButton::Middle, cx.listener(|this, e: &MouseDownEvent, _, cx| this.drag_start(e.position, cx)))
            .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, _, cx| {
                let Some(mut drag) = this.drag else { return };
                if e.pressed_button.is_none() {
                    this.drag = None;
                    cx.notify();
                    return;
                }
                this.view = drag.update((f32::from(e.position.x), f32::from(e.position.y)));
                this.dragged = drag.moved;
                this.touched |= drag.moved;
                this.drag = Some(drag);
                if drag.moved {
                    this.hovered = None;
                }
                cx.notify();
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _: &MouseUpEvent, _, cx| {
                this.drag = None;
                cx.notify();
            }))
            .on_mouse_up(MouseButton::Middle, cx.listener(|this, _: &MouseUpEvent, _, cx| {
                this.drag = None;
                cx.notify();
            }))
            .on_scroll(cx.listener(|this, e: &ScrollEvent, _, cx| {
                // The canvas owns the wheel: the page does not scroll under it.
                cx.stop_propagation();
                let delta = match e.delta {
                    ScrollDelta::Pixels(d) => (f32::from(d.x), f32::from(d.y)),
                    ScrollDelta::Lines(l) => (l.x * 24., l.y * 24.),
                };
                let at = this.local(e.position);
                this.touched = true;
                this.view = if e.modifiers.platform || e.modifiers.control {
                    this.view.zoom_at(canvas::wheel_zoom(-delta.1), at)
                } else {
                    this.view.pan(delta.0, delta.1)
                };
                cx.notify();
            }))
            .on_pinch(cx.listener(|this, e: &PinchEvent, _, cx| {
                cx.stop_propagation();
                let at = this.local(e.position);
                this.touched = true;
                this.view = this.view.zoom_at(1. + e.delta, at);
                cx.notify();
            }))
            .on_click(cx.listener(move |this, e: &ClickEvent, _, cx| {
                if this.dragged {
                    return;
                }
                if e.click_count() >= 2 {
                    this.fit(world, cx);
                } else if matches!(this.panel, Some(Panel::Group(_))) {
                    this.panel = None;
                    cx.notify();
                }
            }))
            .child(links)
            .children(frames)
            .children(nodes)
            .children(pills)
            .child(hint)
            .child(legend)
            .child(controls)
    }

    /// «Данные»: every DTO as a small structure card with its fields.
    fn render_dto_lane(&self, dtos: &[(usize, &map::MapDto)], g: &Graph, fresh: &HashSet<String>, focus: Option<usize>, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let mode = self.mode;
        let mono = theme.mono_font_family.clone();
        v_flex()
            .pt(px(4.))
            .gap(px(8.))
            .child(
                h_flex()
                    .gap(px(8.))
                    .child(ui::mono("ДАННЫЕ · ЧТО ПЕРЕДАЁТСЯ", theme).text_size(px(10.)).text_color(p.faint))
                    .child(div().flex_1().h(px(1.)).bg(p.border)),
            )
            .child(h_flex().flex_wrap().items_start().gap(px(10.)).children(dtos.iter().map(|&(di, d)| {
                let tone = if mode == MapMode::Diff || (mode == MapMode::After && d.status == NodeStatus::Added) { status_tone(d.status, p) } else { None };
                let key = d.key.clone();
                let hover_key = d.key.clone();
                let dim = focus.is_some_and(|f| f != di);
                let is_fresh = fresh.contains(&format!("dto:{}", d.key));
                let users = d.users.iter().filter(|&&u| g.node_visible(u, mode)).count();
                let shown: Vec<&map::MapField> = d.fields_in(mode).collect();
                let more = shown.len().saturating_sub(7);
                v_flex()
                    .id(SharedString::from(format!("dto-{di}-{}", d.key)))
                    .relative()
                    .w(px(232.))
                    .rounded(px(8.))
                    .border_1()
                    .bg(p.surface)
                    .shadow(ui::card_shadow())
                    .border_color(tone.map_or(p.border, |t| ui::mix(t.solid, p.border, 0.5)))
                    .when(focus == Some(di), |d| d.border_color(p.ink))
                    .when(dim, |d| d.opacity(0.35))
                    .cursor_pointer()
                    .hover(|s| s.border_color(p.border_strong))
                    .on_hover(cx.listener(move |this, on: &bool, _, cx| {
                        if *on {
                            this.hovered_dto = Some(hover_key.clone());
                        } else if this.hovered_dto.as_deref() == Some(hover_key.as_str()) {
                            this.hovered_dto = None;
                        }
                        cx.notify();
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.panel = Some(Panel::Dto(key.clone()));
                        cx.notify();
                    }))
                    .child(
                        h_flex()
                            .px(px(10.))
                            .pt(px(7.))
                            .pb(px(6.))
                            .gap(px(6.))
                            .border_b_1()
                            .border_color(p.border)
                            .child(div().flex_1().min_w_0().truncate().font_family(mono.clone()).text_size(px(11.5)).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).when(d.status == NodeStatus::Removed, |d| d.line_through()).child(d.name.clone()))
                            .child(div().flex_shrink_0().font_family(mono.clone()).text_size(px(10.)).text_color(p.faint).child(dto_kind_label(&d.kind))),
                    )
                    .child(v_flex().px(px(10.)).py(px(6.)).gap(px(2.)).children(shown.iter().take(7).map(|f| field_row(f, mode, p, theme))).when(more > 0, |v| {
                        v.child(div().font_family(mono.clone()).text_size(px(10.)).text_color(p.faint).child(format!("ещё {more} полей")))
                    }).when(shown.is_empty(), |v| v.child(div().text_size(px(11.)).text_color(p.faint).child("поля не описаны"))))
                    .child(
                        h_flex()
                            .px(px(10.))
                            .pb(px(7.))
                            .gap(px(6.))
                            .font_family(mono.clone())
                            .text_size(px(10.))
                            .text_color(p.faint)
                            .child(format!("узлов: {users}"))
                            .when(d.touched() > 0, |h| h.child(format!("· полей изменено: {}", d.touched())))
                            .when(!d.file.is_empty(), |h| h.child(div().min_w_0().truncate().child(format!("· {}", d.file.rsplit('/').next().unwrap_or_default())))),
                    )
                    .when(is_fresh, |d| d.child(div().absolute().left(px(-5.)).top(px(-5.)).size(px(10.)).rounded_full().bg(p.blue.solid).border_2().border_color(p.sunken)))
            })))
    }

    fn render_decisions(&self, g: &Graph, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let upto = self.selected?;
        let groups: Vec<(&Run, &Report)> = self.runs[..=upto].iter().filter_map(|r| r.ok().map(|rep| (r, rep))).filter(|(_, rep)| !rep.decisions.is_empty()).collect();
        if groups.is_empty() {
            return None;
        }
        let total: usize = groups.iter().map(|(_, r)| r.decisions.len()).sum();
        let mono = theme.mono_font_family.clone();
        Some(
            v_flex()
                .gap(px(12.))
                .child(ui::section_title("Решения агента", Some(total.to_string()), p, theme).child(ui::mono("как и почему менялась карта, по итерациям", theme).text_color(p.faint)))
                .child(ui::card(p).overflow_hidden().children(groups.into_iter().enumerate().map(|(gi, (run, rep))| {
                    let kind = if run.meta.kind == RunKind::Full { "полный" } else { "батч" };
                    v_flex()
                        .when(gi > 0, |d| d.border_t_1().border_color(p.border))
                        .child(
                            h_flex()
                                .px(px(14.))
                                .pt(px(10.))
                                .pb(px(4.))
                                .gap(px(8.))
                                .child(div().font_family(mono.clone()).text_size(px(11.5)).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).child(format!("#{}", run.n)))
                                .child(ui::eyebrow(format!("{kind} · {}", hhmm(run.meta.at)), p, theme)),
                        )
                        .children(rep.decisions.iter().enumerate().map(|(di, d)| {
                            h_flex()
                                .px(px(14.))
                                .py(px(8.))
                                .gap(px(10.))
                                .items_start()
                                .child(div().mt(px(5.)).size(px(7.)).flex_shrink_0().rounded_full().bg(p.blue.solid))
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .min_w_0()
                                        .gap(px(3.))
                                        .child(
                                            h_flex()
                                                .gap(px(8.))
                                                .child(div().text_size(px(13.5)).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).child(d.title.clone()))
                                                .when(!d.at.is_empty(), |h| h.child(ui::mono(d.at.clone(), theme).text_color(p.faint))),
                                        )
                                        .when(!d.why.is_empty(), |v| v.child(div().text_size(px(13.)).line_height(px(19.)).text_color(p.fg).child(d.why.clone())))
                                        .when(!d.nodes.is_empty(), |v| {
                                            v.child(h_flex().gap(px(5.)).flex_wrap().children(d.nodes.iter().enumerate().map(|(ni, k)| {
                                                self.node_chip(SharedString::from(format!("dec-{}-{di}-{ni}", run.n)), &map::normalize(k), g, p, theme, cx)
                                            })))
                                        }),
                                )
                        }))
                }))),
        )
    }

    /// A clickable node name tinted by its status.
    fn node_chip(&self, id: SharedString, key: &str, g: &Graph, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let Some(i) = g.index(key).or_else(|| g.nodes.iter().position(|n| map::normalize(&n.name) == key)) else {
            return ui::chip(key.to_owned(), p, theme).into_any_element();
        };
        let n = &g.nodes[i];
        let tone = status_tone(n.status, p);
        let open = n.key.clone();
        h_flex()
            .id(id)
            .flex_shrink_0()
            .gap(px(5.))
            .px(px(6.))
            .py(px(1.))
            .rounded(px(5.))
            .border_1()
            .border_color(p.border)
            .bg(p.surface)
            .cursor_pointer()
            .hover(|s| s.border_color(p.border_strong))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.panel = Some(Panel::Node(open.clone()));
                cx.notify();
            }))
            .child(div().size(px(7.)).rounded(px(2.)).bg(tone.map_or(p.border_strong, |t| t.solid)))
            .child(div().font_family(theme.mono_font_family.clone()).text_size(px(11.5)).text_color(p.ink).child(n.name.clone()))
            .into_any_element()
    }

    fn render_files(&self, g: &Graph, patches: &[(String, String)], p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let mut files: Vec<String> = Vec::new();
        for n in g.nodes.iter().filter(|n| !n.file.is_empty() && n.status != NodeStatus::Context) {
            if !files.contains(&n.file) {
                files.push(n.file.clone());
            }
        }
        for (f, _) in patches {
            if !files.iter().any(|x| x == f || f.ends_with(&format!("/{x}")) || x.ends_with(&format!("/{f}"))) {
                files.push(f.clone());
            }
        }
        let root = self.input.cwd.clone();
        v_flex()
            .gap(px(10.))
            .child(ui::section_title("Файлы", Some(files.len().to_string()), p, theme))
            .child(ui::card(p).overflow_hidden().when(files.is_empty(), |c| c.child(div().p(px(14.)).text_size(px(12.5)).text_color(p.faint).child("—"))).children(files.iter().enumerate().map(|(fi, f)| {
                let hunks = file_hunks(patches, f);
                let (plus, minus) = line_stats(&hunks.iter().map(|h| (f.clone(), h.clone())).collect::<Vec<_>>());
                let members: Vec<&map::MapNode> = g.nodes.iter().filter(|n| &n.file == f && !n.implicit).collect();
                let all_new = !members.is_empty() && members.iter().all(|n| n.status == NodeStatus::Added);
                let (open, root) = (root.join(f), root.clone());
                v_flex()
                    .px(px(14.))
                    .py(px(10.))
                    .gap(px(7.))
                    .when(fi > 0, |d| d.border_t_1().border_color(p.border))
                    .child(
                        h_flex()
                            .id(SharedString::from(format!("file-{fi}")))
                            .gap(px(8.))
                            .cursor_pointer()
                            .on_click(move |_, window, cx| crate::preview::open_in_app(open.clone(), Some(root.clone()), window, cx))
                            .child(ui::badge(if all_new { "NEW" } else { "MOD" }, if all_new { p.green } else { p.amber }).font_family(theme.mono_font_family.clone()).text_size(px(10.)))
                            .child(div().flex_1().min_w_0().truncate().font_family(theme.mono_font_family.clone()).text_size(px(12.)).text_color(p.ink).child(f.clone()))
                            .when(plus + minus > 0, |h| {
                                h.child(ui::mono(format!("+{plus}"), theme).text_color(p.green.text)).child(ui::mono(format!("−{minus}"), theme).text_color(p.red.text))
                            }),
                    )
                    .when(!members.is_empty(), |v| {
                        v.child(h_flex().gap(px(5.)).flex_wrap().children(members.iter().enumerate().map(|(mi, n)| self.node_chip(SharedString::from(format!("file-{fi}-node-{mi}")), &n.key, g, p, theme, cx))))
                    })
            })))
    }

    fn render_risks(&self, g: &Graph, r: &Report, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap(px(10.))
            .child(ui::section_title("Риски на карте", Some(r.risks.len().to_string()), p, theme))
            .child(ui::card(p).overflow_hidden().when(r.risks.is_empty(), |c| c.child(div().p(px(14.)).text_size(px(12.5)).text_color(p.faint).child("Ревьюер рисков не нашёл"))).children(r.risks.iter().enumerate().map(|(i, risk)| {
                let node = risk.nodes.first().map(|k| map::normalize(k)).or_else(|| g.nodes.iter().find(|n| !risk.id.is_empty() && n.risk_ids.contains(&risk.id)).map(|n| n.key.clone()));
                h_flex()
                    .px(px(14.))
                    .py(px(10.))
                    .gap(px(10.))
                    .items_start()
                    .when(i > 0, |d| d.border_t_1().border_color(p.border))
                    .child(match severity_badge(&risk.severity, p) {
                        Some((label, tone)) => ui::badge(label, tone).font_family(theme.mono_font_family.clone()).text_size(px(10.)).into_any_element(),
                        None => div().mt(px(1.)).child(Icon::new(IconName::TriangleAlert).small().text_color(p.amber.solid)).into_any_element(),
                    })
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(px(4.))
                            .child(div().text_size(px(13.5)).line_height(px(19.)).text_color(p.fg).child(risk.title.clone()))
                            .when(!risk.detail.is_empty(), |v| v.child(div().text_size(px(12.5)).text_color(p.muted).child(risk.detail.clone())))
                            .when_some(node, |v, k| v.child(h_flex().child(self.node_chip(SharedString::from(format!("risk-{i}-node")), &k, g, p, theme, cx)))),
                    )
            })))
    }

    fn render_diagram(&self, key: String, d: &Diagram, dir: &Path, p: &Palette, theme: &Theme) -> impl IntoElement {
        let dark = theme.is_dark();
        let image = d.image.as_ref().map(|path| themed_image(&resolve(dir, path), dark)).filter(|path| path.exists());
        let root = self.input.cwd.clone();
        v_flex()
            .gap(px(12.))
            .child(ui::section_title(if d.title.is_empty() { "Схема".to_owned() } else { d.title.clone() }, None, p, theme))
            .map(|this| match (&image, &d.mermaid) {
                (Some(path), _) => {
                    let open = path.clone();
                    this.child(
                        ui::card(p)
                            .id(SharedString::from(format!("diagram-{key}")))
                            .p(px(10.))
                            .items_center()
                            .when(!dark, |card| card.bg(gpui_kit::white()))
                            .cursor_pointer()
                            .hover(|s| s.border_color(p.border_strong))
                            .on_click(move |_, window, cx| crate::preview::open_in_app(open.clone(), Some(root.clone()), window, cx))
                            .child(crate::media::picture(SharedString::from(format!("diagram-img-{key}-{}", path.display())), path.clone(), 1100., 900.)),
                    )
                }
                (None, Some(src)) => this.child(ui::card(p).p(px(12.)).bg(p.surface2).child(ui::mono(src.clone(), theme).text_color(p.fg))),
                (None, None) => this,
            })
            .when(!d.caption.is_empty(), |this| this.child(div().text_size(px(12.5)).text_color(p.muted).child(d.caption.clone())))
    }

    fn render_ui_pair(&self, i: usize, u: &UiChange, dir: &Path, p: &Palette) -> impl IntoElement {
        let shot = |label: &'static str, path: Option<PathBuf>| {
            let picture = match path.filter(|path| path.exists()) {
                Some(path) => {
                    let (open, root) = (path.clone(), self.input.cwd.clone());
                    div()
                        .id(SharedString::from(format!("ui-shot-{i}-{label}")))
                        .w_full()
                        .flex()
                        .justify_center()
                        .bg(p.surface2)
                        .cursor_pointer()
                        .on_click(move |_, window, cx| crate::preview::open_in_app(open.clone(), Some(root.clone()), window, cx))
                        .child(crate::media::picture(SharedString::from(format!("ui-{i}-{label}-{}", path.display())), path, 640., 420.))
                        .into_any_element()
                }
                None => div().h(px(200.)).w_full().flex().items_center().justify_center().bg(p.surface2).text_size(px(12.)).text_color(p.faint).child("нет снимка").into_any_element(),
            };
            v_flex()
                .flex_1()
                .min_w_0()
                .rounded(px(12.))
                .border_1()
                .border_color(p.border)
                .bg(p.surface)
                .shadow(ui::card_shadow())
                .overflow_hidden()
                .child(picture)
                .child(
                    h_flex()
                        .px(px(12.))
                        .py(px(8.))
                        .gap(px(8.))
                        .border_t_1()
                        .border_color(p.border)
                        .child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).child(label))
                        .when(label == "Стало" && !u.note.is_empty(), |this| this.child(div().min_w_0().truncate().text_size(px(12.5)).text_color(p.muted).child(u.note.clone()))),
                )
        };
        v_flex()
            .gap(px(8.))
            .when(!u.title.is_empty(), |this| this.child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).child(u.title.clone())))
            .child(
                h_flex()
                    .gap(px(12.))
                    .items_center()
                    .child(shot("Было", u.before.as_ref().map(|path| resolve(dir, path))))
                    .child(Icon::new(IconName::ArrowRight).small().text_color(p.faint))
                    .child(shot("Стало", u.after.as_ref().map(|path| resolve(dir, path)))),
            )
    }

    /// Sticky decision bar under the report.
    fn render_bar(&self, _g: &Graph, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let checks = self.report().map_or(0, |r| r.check.len());
        let done = self.checked.len().min(checks);
        h_flex()
            .flex_shrink_0()
            .px(px(20.))
            .py(px(10.))
            .gap(px(10.))
            .border_t_1()
            .border_color(p.border)
            .bg(p.surface)
            .child(
                ui::mono(format!("{} в замечаниях · руками: {done} из {checks}", self.remarks.len()), theme)
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(p.muted),
            )
            .when(self.accepted, |d| d.child(ui::dot_badge("Принято", p.green)))
            .child(
                PressButton::new("bar-checks")
                    .small()
                    .icon(Icon::empty().path("icons/list-checks.svg"))
                    .label(format!("Проверить руками {done}/{checks}"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.panel = if this.panel == Some(Panel::Checks) { None } else { Some(Panel::Checks) };
                        cx.notify();
                    })),
            )
            .child(
                PressButton::new("bar-return")
                    .small()
                    .icon(IconName::Undo2)
                    .label("Вернуть с замечанием")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.panel = Some(Panel::Return);
                        cx.notify();
                    })),
            )
            .child(
                PressButton::new("bar-accept")
                    .small()
                    .primary()
                    .icon(IconName::Check)
                    .label(if self.accepted { "Принято" } else { "Принять" })
                    .disabled(self.accepted)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.accepted = true;
                        cx.emit(ChangesEvent::Accept);
                        cx.notify();
                    })),
            )
    }

    fn render_panel(&self, panel: Panel, g: &Graph, p: &Palette, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (title, body): (SharedString, AnyElement) = match &panel {
            Panel::Node(key) => match g.index(key) {
                Some(i) => (g.nodes[i].name.clone().into(), self.render_node_panel(i, g, p, theme, cx).into_any_element()),
                None => ("Узел".into(), div().into_any_element()),
            },
            Panel::Dto(key) => match g.dto_index(key) {
                Some(d) => (g.dtos[d].name.clone().into(), self.render_dto_panel(d, g, p, theme, cx).into_any_element()),
                None => ("Данные".into(), div().into_any_element()),
            },
            Panel::Group(key) => match g.group_index(key) {
                Some(gi) => (g.groups[gi].title.clone().into(), self.render_group_panel(gi, g, p, theme, cx).into_any_element()),
                None => ("Группа".into(), div().into_any_element()),
            },
            Panel::Checks => ("Проверь руками".into(), self.render_checks_panel(p, theme, cx).into_any_element()),
            Panel::Return => ("Вернуть с замечанием".into(), self.render_return_panel(g, p, theme, window, cx).into_any_element()),
        };
        v_flex()
            .w(px(420.))
            .flex_shrink_0()
            .h_full()
            .border_l_1()
            .border_color(p.border)
            .bg(p.surface)
            .child(
                h_flex()
                    .h(px(44.))
                    .px(px(14.))
                    .gap(px(8.))
                    .border_b_1()
                    .border_color(p.border)
                    .child(div().flex_1().min_w_0().truncate().font_family(theme.mono_font_family.clone()).text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).child(title))
                    .child(PressButton::new("panel-close").ghost().small().icon(IconName::Close).on_click(cx.listener(|this, _, _, cx| {
                        this.panel = None;
                        cx.notify();
                    }))),
            )
            .child(div().id("changes-panel").flex_1().min_h_0().overflow_y_scroll().child(v_flex().p(px(16.)).gap(px(14.)).child(body)))
    }

    /// A frame: what it does, its entities by role, the tests that check them.
    fn render_group_panel(&self, gi: usize, g: &Graph, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let gr = &g.groups[gi];
        let members: Vec<usize> = gr.nodes.iter().copied().filter(|&i| g.node_visible(i, self.mode)).collect();
        let mut tests: Vec<usize> = members.iter().flat_map(|&i| g.tests_of(i)).collect();
        tests.sort_unstable();
        tests.dedup();
        let untested: Vec<usize> = if g.has_tests() { members.iter().copied().filter(|&i| g.untested(i)).collect() } else { Vec::new() };
        let label = |t: &str| ui::eyebrow(t.to_owned(), p, theme);
        let row = |i: usize, cx: &mut Context<Self>| {
            let n = &g.nodes[i];
            let role = n.role();
            let key = n.key.clone();
            h_flex()
                .id(SharedString::from(format!("group-member-{i}")))
                .gap(px(8.))
                .py(px(5.))
                .cursor_pointer()
                .hover(|s| s.bg(p.surface2))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.panel = Some(Panel::Node(key.clone()));
                    cx.notify();
                }))
                .child(Icon::empty().path(role.icon()).small().text_color(p.muted))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(div().truncate().text_size(px(13.)).font_weight(FontWeight::MEDIUM).text_color(p.ink).child(n.title().to_owned()))
                        .child(div().truncate().font_family(theme.mono_font_family.clone()).text_size(px(11.)).text_color(p.faint).child(format!("{} · {}", role.label(), n.name))),
                )
                .when_some(status_tone(n.status, p).filter(|_| n.status != NodeStatus::Context), |d, t| d.child(ui::badge(status_label(n.status), t)))
        };
        v_flex()
            .gap(px(14.))
            .child(h_flex().gap(px(8.)).child(ui::chip(gr.kind.label(), p, theme)).child(ui::mono(format!("{} сущностей", members.len()), theme).text_color(p.faint)))
            .when(!gr.summary.is_empty(), |d| d.child(div().text_size(px(13.5)).line_height(px(20.)).text_color(p.fg).child(gr.summary.clone())))
            .child(v_flex().gap(px(2.)).child(label("Сущности")).children(members.iter().map(|&i| row(i, cx).into_any_element()).collect::<Vec<_>>()))
            .when(!tests.is_empty(), |d| d.child(v_flex().gap(px(2.)).child(label("Проверяют тесты")).children(tests.iter().map(|&i| row(i, cx).into_any_element()).collect::<Vec<_>>())))
            .when(!untested.is_empty(), |d| {
                d.child(
                    v_flex()
                        .gap(px(4.))
                        .p(px(10.))
                        .rounded(px(8.))
                        .bg(p.amber.soft)
                        .child(div().text_size(px(12.5)).font_weight(FontWeight::SEMIBOLD).text_color(p.amber.text).child(format!("Без тестов: {}", untested.len())))
                        .child(div().text_size(px(12.)).text_color(p.amber.text).child(untested.iter().map(|&i| g.nodes[i].title().to_owned()).collect::<Vec<_>>().join(", "))),
                )
            })
    }

    fn render_node_panel(&self, i: usize, g: &Graph, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let n = &g.nodes[i];
        let r = self.report();
        let mono = theme.mono_font_family.clone();
        let incoming: Vec<usize> = g.edges.iter().filter(|e| e.to == i && e.kind != EdgeKind::Removed).map(|e| e.from).collect();
        let outgoing: Vec<usize> = g.edges.iter().filter(|e| e.from == i && e.kind != EdgeKind::Removed).map(|e| e.to).collect();
        let gone: Vec<usize> = g.edges.iter().filter(|e| (e.from == i || e.to == i) && e.kind == EdgeKind::Removed).map(|e| if e.from == i { e.to } else { e.from }).collect();
        let risks: Vec<&Risk> = r.map(|r| r.risks.iter().filter(|risk| risk.nodes.iter().any(|k| map::normalize(k) == n.key) || (!risk.id.is_empty() && n.risk_ids.contains(&risk.id))).collect()).unwrap_or_default();
        let checks: Vec<(usize, Check)> = r
            .map(|r| r.check.iter().cloned().enumerate().filter(|(_, c)| !c.id.is_empty() && n.check_ids.contains(&c.id)).collect())
            .unwrap_or_default();
        let hunks = node_hunks(n, &self.patches());
        let in_remarks = self.remarks.contains(&n.key);
        let key = n.key.clone();
        let file_path = self.input.cwd.join(&n.file);
        let root = self.input.cwd.clone();
        let links = |label: &'static str, list: &[usize], tone: Option<Tone>, cx: &mut Context<Self>| {
            h_flex()
                .gap(px(8.))
                .items_start()
                .child(div().w(px(70.)).flex_shrink_0().pt(px(2.)).text_size(px(12.)).text_color(tone.map_or(p.muted, |t| t.text)).child(label))
                .child(v_flex().flex_1().gap(px(5.)).children(list.iter().enumerate().map(|(li, &j)| {
                    // What travels along this link.
                    let edge = g.edges.iter().find(|e| (e.from == i && e.to == j) || (e.from == j && e.to == i));
                    let via = edge.and_then(|e| e.via).map(|d| g.dtos[d].name.clone());
                    let note = g.notes.get(&(i, j)).or_else(|| g.notes.get(&(j, i))).cloned();
                    h_flex()
                        .gap(px(6.))
                        .flex_wrap()
                        .child(self.node_chip(SharedString::from(format!("panel-{label}-{li}")), &g.nodes[j].key, g, p, theme, cx))
                        .when_some(via, |h, v| h.child(div().text_size(px(11.5)).text_color(p.muted).child("передаёт")).child(ui::chip(v, p, theme)))
                        .when_some(note, |h, n| h.child(div().text_size(px(11.5)).text_color(p.muted).child(n)))
                })))
        };
        let io = |label: &'static str, list: &[usize]| {
            v_flex().gap(px(6.)).child(ui::eyebrow(label, p, theme)).children(list.iter().map(|&d| {
                let dto = &g.dtos[d];
                v_flex()
                    .gap(px(2.))
                    .p(px(8.))
                    .rounded(px(6.))
                    .border_1()
                    .border_color(p.border)
                    .bg(p.surface2)
                    .child(h_flex().gap(px(6.)).child(div().font_family(mono.clone()).text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).child(dto.name.clone())).child(div().text_size(px(11.)).text_color(p.faint).child(dto_kind_label(&dto.kind))))
                    .children(dto.fields_in(self.mode).map(|f| field_row(f, MapMode::Diff, p, theme)))
            }))
        };
        v_flex()
            .gap(px(14.))
            .child(
                h_flex()
                    .gap(px(8.))
                    .child(match status_tone(n.status, p) {
                        Some(t) => ui::dot_badge(status_label(n.status), t).into_any_element(),
                        None => ui::chip(status_label(n.status), p, theme).into_any_element(),
                    })
                    .when(!n.kind.is_empty(), |h| h.child(div().text_size(px(12.)).text_color(p.muted).child(kind_label(&n.kind))))
                    .child(div().flex_1())
                    .child(
                        PressButton::new("node-remark")
                            .xsmall()
                            .icon(if in_remarks { IconName::Check } else { IconName::Plus })
                            .label(if in_remarks { "в замечаниях" } else { "в замечания" })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(pos) = this.remarks.iter().position(|k| *k == key) {
                                    this.remarks.remove(pos);
                                } else {
                                    this.remarks.push(key.clone());
                                }
                                cx.notify();
                            })),
                    ),
            )
            .child(div().font_family(mono.clone()).text_size(px(18.)).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).when(n.status == NodeStatus::Removed, |d| d.line_through()).child(n.name.clone()))
            .when(!n.file.is_empty(), |v| {
                v.child(
                    div()
                        .id("node-file")
                        .cursor_pointer()
                        .font_family(mono.clone())
                        .text_size(px(11.5))
                        .text_color(p.blue.text)
                        .hover(|s| s.text_color(p.blue.solid))
                        .on_click(move |_, window, cx| crate::preview::open_in_app(file_path.clone(), Some(root.clone()), window, cx))
                        .child(n.file.clone()),
                )
            })
            .when(!n.note.is_empty(), |v| v.child(div().text_size(px(13.5)).line_height(px(20.)).text_color(p.fg).child(n.note.clone())))
            .when(!n.signature.is_empty() || !n.signature_before.is_empty(), |v| {
                let changed = n.signature_changed();
                v.child(
                    v_flex()
                        .gap(px(6.))
                        .child(ui::eyebrow(if changed { "сигнатура · было → стало" } else { "сигнатура" }, p, theme))
                        .when(changed, |v| {
                            v.child(
                                div()
                                    .p(px(8.))
                                    .rounded(px(6.))
                                    .bg(ui::mix(p.red.soft, p.surface, 0.5))
                                    .font_family(mono.clone())
                                    .text_size(px(12.))
                                    .line_height(px(18.))
                                    .text_color(p.fg)
                                    .child(signature_text(&map::sig_diff(&n.signature_before, &n.signature), p)),
                            )
                        })
                        .child(
                            div()
                                .p(px(8.))
                                .rounded(px(6.))
                                .border_1()
                                .border_color(p.border)
                                .bg(p.surface2)
                                .font_family(mono.clone())
                                .text_size(px(12.))
                                .line_height(px(18.))
                                .text_color(p.ink)
                                .child(if n.signature.is_empty() { n.signature_before.clone() } else { n.signature.clone() }),
                        ),
                )
            })
            .when(!n.inputs.is_empty(), |v| v.child(io("принимает", &n.inputs)))
            .when(!n.outputs.is_empty(), |v| v.child(io("возвращает / отдаёт дальше", &n.outputs)))
            .when(!incoming.is_empty() || !outgoing.is_empty() || !gone.is_empty(), |v| {
                v.child(
                    v_flex()
                        .gap(px(8.))
                        .child(ui::eyebrow("связи", p, theme))
                        .when(!incoming.is_empty(), |v| v.child(links("вызывают", &incoming, None, cx)))
                        .when(!outgoing.is_empty(), |v| v.child(links("вызывает", &outgoing, None, cx)))
                        .when(!gone.is_empty(), |v| v.child(links("убрано", &gone, Some(p.red), cx))),
                )
            })
            .children(risks.into_iter().map(|risk| {
                v_flex()
                    .gap(px(5.))
                    .p(px(12.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(ui::mix(p.red.solid, p.border, 0.4))
                    .bg(ui::mix(p.red.soft, p.surface, 0.6))
                    .child(h_flex().gap(px(6.)).items_start().child(Icon::new(IconName::TriangleAlert).small().text_color(p.red.solid)).child(div().flex_1().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(p.red.text).child(risk.title.clone())))
                    .when(!risk.detail.is_empty(), |v| v.child(div().text_size(px(12.5)).text_color(p.fg).child(risk.detail.clone())))
                    .when(!risk.fix.is_empty(), |v| v.child(div().text_size(px(12.5)).text_color(p.fg).child(format!("Как поправить: {}", risk.fix))))
            }))
            .when(!checks.is_empty(), |v| {
                v.child(
                    v_flex().gap(px(6.)).child(ui::eyebrow("проверить руками", p, theme)).children(checks.into_iter().map(|(ci, c)| {
                        let on = self.checked.contains(&ci);
                        h_flex()
                            .id(SharedString::from(format!("node-check-{ci}")))
                            .gap(px(8.))
                            .items_start()
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if !this.checked.remove(&ci) {
                                    this.checked.insert(ci);
                                }
                                cx.notify();
                            }))
                            .child(if on {
                                Icon::new(IconName::CircleCheck).small().text_color(p.green.solid).into_any_element()
                            } else {
                                div().mt(px(1.)).size(px(14.)).flex_shrink_0().rounded_full().border_1().border_color(p.border_strong).into_any_element()
                            })
                            .child(div().flex_1().text_size(px(13.)).text_color(if on { p.muted } else { p.fg }).child(c.title.clone()))
                    })),
                )
            })
            .child(
                v_flex()
                    .gap(px(8.))
                    .child(ui::eyebrow("дифф", p, theme))
                    .when(hunks.is_empty(), |v| v.child(div().text_size(px(12.5)).text_color(p.faint).child(if n.status == NodeStatus::Context { "Этот узел сессия не меняла" } else { "Ханки этого узла не нашлись в диффе" })))
                    .children(hunks.into_iter().take(4).enumerate().map(|(hi, h)| render_hunk(hi, &h, p, theme))),
            )
    }

    fn render_dto_panel(&self, d: usize, g: &Graph, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let dto = &g.dtos[d];
        let mono = theme.mono_font_family.clone();
        let carried: Vec<(usize, usize)> = g.edges.iter().filter(|e| e.via == Some(d)).map(|e| (e.from, e.to)).collect();
        let file_path = self.input.cwd.join(&dto.file);
        let root = self.input.cwd.clone();
        v_flex()
            .gap(px(14.))
            .child(
                h_flex()
                    .gap(px(8.))
                    .child(match status_tone(dto.status, p) {
                        Some(t) => ui::dot_badge(status_label(dto.status), t).into_any_element(),
                        None => ui::chip(status_label(dto.status), p, theme).into_any_element(),
                    })
                    .child(div().text_size(px(12.)).text_color(p.muted).child(dto_kind_label(&dto.kind))),
            )
            .child(div().font_family(mono.clone()).text_size(px(18.)).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).when(dto.status == NodeStatus::Removed, |d| d.line_through()).child(dto.name.clone()))
            .when(!dto.file.is_empty(), |v| {
                v.child(
                    div()
                        .id("dto-file")
                        .cursor_pointer()
                        .font_family(mono.clone())
                        .text_size(px(11.5))
                        .text_color(p.blue.text)
                        .hover(|s| s.text_color(p.blue.solid))
                        .on_click(move |_, window, cx| crate::preview::open_in_app(file_path.clone(), Some(root.clone()), window, cx))
                        .child(dto.file.clone()),
                )
            })
            .when(!dto.note.is_empty(), |v| v.child(div().text_size(px(13.5)).line_height(px(20.)).text_color(p.fg).child(dto.note.clone())))
            .child(
                v_flex()
                    .gap(px(4.))
                    .child(ui::eyebrow(format!("поля · {}", dto.fields.len()), p, theme))
                    .child(
                        v_flex()
                            .p(px(10.))
                            .gap(px(4.))
                            .rounded(px(6.))
                            .border_1()
                            .border_color(p.border)
                            .bg(p.surface2)
                            .when(dto.fields.is_empty(), |v| v.child(div().text_size(px(12.)).text_color(p.faint).child("Ревьюер не описал поля")))
                            .children(dto.fields.iter().map(|f| {
                                v_flex()
                                    .gap(px(1.))
                                    .child(field_row(f, MapMode::Diff, p, theme))
                                    .when(!f.note.is_empty(), |v| v.child(div().pl(px(12.)).text_size(px(11.5)).text_color(p.muted).child(f.note.clone())))
                            })),
                    ),
            )
            .when(!dto.users.is_empty(), |v| {
                v.child(
                    v_flex()
                        .gap(px(6.))
                        .child(ui::eyebrow("кто использует", p, theme))
                        .child(h_flex().gap(px(5.)).flex_wrap().children(dto.users.iter().enumerate().map(|(ui_, &n)| self.node_chip(SharedString::from(format!("dto-user-{ui_}")), &g.nodes[n].key, g, p, theme, cx)))),
                )
            })
            .when(!carried.is_empty(), |v| {
                v.child(
                    v_flex().gap(px(6.)).child(ui::eyebrow("едет по связям", p, theme)).children(carried.iter().enumerate().map(|(ci, &(a, b))| {
                        h_flex()
                            .gap(px(6.))
                            .flex_wrap()
                            .child(self.node_chip(SharedString::from(format!("dto-from-{ci}")), &g.nodes[a].key, g, p, theme, cx))
                            .child(Icon::new(IconName::ArrowRight).xsmall().text_color(p.faint))
                            .child(self.node_chip(SharedString::from(format!("dto-to-{ci}")), &g.nodes[b].key, g, p, theme, cx))
                    })),
                )
            })
    }

    fn render_checks_panel(&self, p: &Palette, _theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let items: Vec<Check> = self.report().map(|r| r.check.clone()).unwrap_or_default();
        v_flex()
            .gap(px(8.))
            .when(items.is_empty(), |v| v.child(div().text_size(px(12.5)).text_color(p.faint).child("Ревьюер не оставил проверок")))
            .children(items.into_iter().enumerate().map(|(i, c)| {
                let on = self.checked.contains(&i);
                h_flex()
                    .id(SharedString::from(format!("check-{i}")))
                    .p(px(10.))
                    .gap(px(10.))
                    .items_start()
                    .rounded(px(8.))
                    .border_1()
                    .border_color(p.border)
                    .cursor_pointer()
                    .hover(|s| s.bg(p.surface2))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.checked.remove(&i) {
                            this.checked.insert(i);
                        }
                        cx.notify();
                    }))
                    .child(
                        div()
                            .mt(px(1.))
                            .size(px(16.))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.))
                            .border_1()
                            .map(|d| if on { d.bg(p.green.solid).border_color(p.green.solid).child(Icon::new(IconName::Check).xsmall().text_color(gpui_kit::white())) } else { d.bg(p.surface).border_color(p.border_strong) }),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .gap(px(3.))
                            .child(div().text_size(px(13.5)).text_color(if on { p.muted } else { p.fg }).when(on, |d| d.line_through()).child(c.title.clone()))
                            .when(!c.how.is_empty(), |v| v.child(div().text_size(px(12.)).text_color(p.muted).child(c.how.clone()))),
                    )
            }))
    }

    fn render_return_panel(&self, g: &Graph, p: &Palette, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let note = window.use_keyed_state(SharedString::from(format!("changes-return-{}", self.input.key)), cx, |window, cx| {
            InputState::new(window, cx).placeholder("Что поправить — своими словами")
        });
        let preview = self.remark_text(g, &note.read(cx).value());
        let send = note.clone();
        v_flex()
            .gap(px(12.))
            .child(div().text_size(px(12.5)).text_color(p.muted).child("Узлы добавляются кнопкой «в замечания» в панели узла. Текст уйдёт агенту в эту сессию."))
            .child(h_flex().gap(px(5.)).flex_wrap().when(self.remarks.is_empty(), |h| h.child(div().text_size(px(12.5)).text_color(p.faint).child("Узлов пока нет"))).children(self.remarks.iter().enumerate().map(|(i, k)| self.node_chip(SharedString::from(format!("remark-{i}")), k, g, p, theme, cx))))
            .child(Input::new(&note))
            .child(ui::card(p).p(px(10.)).bg(p.surface2).child(ui::mono(preview, theme).text_color(p.fg).whitespace_normal()))
            .child(
                h_flex().child(
                    PressButton::new("return-send")
                        .primary()
                        .small()
                        .icon(IconName::ArrowUp)
                        .label("Отправить агенту")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            let graph = this.report().map(|r| map::build_report(r)).unwrap_or_default();
                            let text = this.remark_text(&graph, &send.read(cx).value());
                            cx.emit(ChangesEvent::Return(text));
                            send.update(cx, |s, cx| s.set_value("", window, cx));
                            this.remarks.clear();
                            this.panel = None;
                            cx.notify();
                        })),
                ),
            )
    }
}

/// Signature runs as text: added parts green, removed ones red and struck.
fn signature_text(parts: &[(String, map::Part)], p: &Palette) -> StyledText {
    let mut text = String::new();
    let mut highlights = Vec::new();
    for (t, part) in parts {
        let range = text.len()..text.len() + t.len();
        text.push_str(t);
        match part {
            map::Part::Same => {}
            map::Part::Added => highlights.push((range, HighlightStyle { color: Some(p.green.text), background_color: Some(p.green.soft), ..Default::default() })),
            map::Part::Removed => highlights.push((
                range,
                HighlightStyle { color: Some(p.red.text), strikethrough: Some(StrikethroughStyle { thickness: px(1.), color: Some(p.red.text) }), ..Default::default() },
            )),
        }
    }
    StyledText::new(text).with_highlights(highlights)
}

fn dto_kind_label(kind: &str) -> String {
    match kind.trim().to_lowercase().as_str() {
        "props" => "пропсы",
        "args" | "arguments" | "params" => "аргументы",
        "response" => "ответ API",
        "request" | "body" => "запрос API",
        "event" | "payload" => "событие",
        "state" | "store" => "состояние",
        "type" | "interface" | "struct" => "тип",
        "" => "данные",
        other => return other.to_owned(),
    }
    .to_owned()
}

/// `+ flyToHub?: boolean` — a DTO field with its change mark.
fn field_row(f: &map::MapField, mode: MapMode, p: &Palette, theme: &Theme) -> impl IntoElement {
    let show = mode == MapMode::Diff || (mode == MapMode::After && f.status == NodeStatus::Added) || (mode == MapMode::Before && f.status == NodeStatus::Removed);
    let (mark, tone) = match (show || f.status == NodeStatus::Changed, f.status) {
        (true, NodeStatus::Added) => ("+", Some(p.green)),
        (true, NodeStatus::Removed) => ("−", Some(p.red)),
        (true, NodeStatus::Changed) => ("~", Some(p.amber)),
        _ => (" ", None),
    };
    h_flex()
        .gap(px(4.))
        .min_w_0()
        .font_family(theme.mono_font_family.clone())
        .text_size(px(10.5))
        .line_height(px(15.))
        .child(div().w(px(8.)).flex_shrink_0().text_color(tone.map_or(p.faint, |t| t.text)).child(mark))
        .child(
            div()
                .flex_shrink_0()
                .text_color(tone.map_or(p.fg, |t| t.text))
                .when(f.status == NodeStatus::Removed && tone.is_some(), |d| d.line_through())
                .child(f.name.clone()),
        )
        .when(!f.ty.is_empty(), |h| h.child(div().min_w_0().truncate().text_color(p.muted).child(format!(": {}", f.ty))))
}

fn render_hunk(i: usize, hunk: &str, p: &Palette, theme: &Theme) -> impl IntoElement {
    let lines: Vec<&str> = hunk.lines().take(60).collect();
    v_flex()
        .id(SharedString::from(format!("hunk-{i}")))
        .rounded(px(8.))
        .border_1()
        .border_color(p.border)
        .overflow_hidden()
        .font_family(theme.mono_font_family.clone())
        .text_size(px(11.))
        .children(lines.into_iter().map(|l| {
            let (bg, fg) = if l.starts_with("@@") {
                (p.blue.soft, p.blue.text)
            } else if l.starts_with('+') {
                (ui::mix(p.green.soft, p.surface, 0.8), p.green.text)
            } else if l.starts_with('-') {
                (ui::mix(p.red.soft, p.surface, 0.8), p.red.text)
            } else {
                (p.surface, p.fg)
            };
            div().px(px(8.)).py(px(1.)).bg(bg).text_color(fg).truncate().child(if l.is_empty() { " ".to_owned() } else { l.to_owned() })
        }))
}

fn render_columns(code: &CodeChanges, p: &Palette, theme: &Theme) -> impl IntoElement {
    let column = |key: &'static str, title: &'static str, sign: &'static str, tone: Tone, entries: &[Entry]| {
        v_flex()
            .id(SharedString::from(format!("code-{key}")))
            .flex_1()
            .min_w_0()
            .rounded(px(12.))
            .border_1()
            .border_color(p.border)
            .bg(p.surface)
            .shadow(ui::card_shadow())
            .overflow_hidden()
            .child(div().h(px(3.)).bg(tone.solid))
            .child(
                h_flex()
                    .px(px(14.))
                    .pt(px(10.))
                    .pb(px(6.))
                    .gap_2()
                    .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).text_color(tone.text).child(format!("{sign} {title}")))
                    .child(ui::mono(entries.len().to_string(), theme).text_color(p.muted)),
            )
            .child(
                v_flex()
                    .px(px(14.))
                    .pb(px(12.))
                    .gap(px(10.))
                    .when(entries.is_empty(), |this| this.child(div().text_size(px(12.5)).text_color(p.faint).child("—")))
                    .children(entries.iter().map(|e| {
                        v_flex()
                            .gap(px(2.))
                            .child(
                                h_flex()
                                    .gap(px(6.))
                                    .child(ui::chip(kind_label(&e.kind), p, theme))
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .font_family(theme.mono_font_family.clone())
                                            .text_size(px(12.5))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(p.ink)
                                            .when(key == "removed", |d| d.line_through().text_decoration_color(p.red.solid))
                                            .child(e.name.clone()),
                                    ),
                            )
                            .when(!e.file.is_empty(), |this| this.child(ui::mono(e.file.clone(), theme).truncate().text_color(p.faint)))
                            .when(!e.note.is_empty(), |this| this.child(div().text_size(px(12.5)).text_color(p.fg).child(e.note.clone())))
                    })),
            )
    };
    h_flex()
        .gap(px(12.))
        .items_start()
        .child(column("added", "Добавлено", "+", p.green, &code.added))
        .child(column("changed", "Изменено", "~", p.amber, &code.changed))
        .child(column("removed", "Удалено", "−", p.red, &code.removed))
}

/// Four numbers under the summary: files, lines, functions, risks.
fn metrics(files: usize, plus: usize, minus: usize, code: &CodeChanges, risks: usize, p: &Palette, theme: &Theme) -> impl IntoElement {
    let big = |text: String, color: Hsla| div().text_size(px(22.)).font_weight(FontWeight::SEMIBOLD).text_color(color).child(text);
    let sep = || div().text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).text_color(p.faint).child("·");
    let cells: Vec<(&'static str, AnyElement)> = vec![
        ("Файлов", big(files.to_string(), p.ink).into_any_element()),
        ("Строк", h_flex().gap(px(8.)).child(big(format!("+{plus}"), p.green.text)).child(big(format!("−{minus}"), p.red.text)).into_any_element()),
        (
            "Функций",
            h_flex()
                .gap(px(6.))
                .child(big(format!("+{}", code.added.len()), p.green.text))
                .child(sep())
                .child(big(format!("~{}", code.changed.len()), p.amber.text))
                .child(sep())
                .child(big(format!("−{}", code.removed.len()), p.red.text))
                .into_any_element(),
        ),
        ("Риски", big(risks.to_string(), if risks > 0 { p.amber.text } else { p.ink }).into_any_element()),
    ];
    ui::card(p).child(h_flex().children(cells.into_iter().enumerate().map(|(i, (label, value))| {
        v_flex()
            .flex_1()
            .px(px(16.))
            .py(px(12.))
            .gap(px(4.))
            .when(i > 0, |d| d.border_l_1().border_color(p.border))
            .child(ui::eyebrow(label, p, theme))
            .child(div().h(px(30.)).flex().items_center().child(value))
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[core::prelude::v1::test]
    fn dark_theme_picks_dark_diagram_when_rendered() {
        let dir = std::env::temp_dir().join(format!("stepan_verify-themed-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let light = dir.join("flow.png");
        assert_eq!(themed_image(&light, true), light);
        std::fs::write(dir.join("flow-dark.png"), b"").unwrap();
        assert_eq!(themed_image(&light, true), dir.join("flow-dark.png"));
        assert_eq!(themed_image(&light, false), light);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[core::prelude::v1::test]
    fn report_tolerates_old_and_new_shapes() {
        let r: Report = serde_json::from_str(r#"{"title":"t","code":{"added":[{"name":"div","kind":"function"}]},"risks":["Высокий: падает"],"check":["открой"]}"#).unwrap();
        assert_eq!(r.code.added[0].name, "div");
        assert!(r.flow.is_none() && r.diagrams.is_empty() && r.ui.is_empty() && r.decisions.is_empty());
        assert_eq!(r.risks[0].severity, "high");
        assert_eq!(r.check[0].title, "открой");
        let r: Report = serde_json::from_str(
            r#"{"risks":[{"id":"r1","severity":"mid","title":"x","node":"a"}],
                "check":[{"id":"c1","title":"y","how":"z"}],
                "removed_edges":[["a","b"],{"from":"c","to":"d"}],
                "decisions":[{"title":"Оставил флаг","why":"чтобы откатить","nodes":["a"],"at":"14:22"}]}"#,
        )
        .unwrap();
        assert_eq!(r.risks[0].nodes, vec!["a"]);
        assert_eq!(r.check[0].how, "z");
        assert_eq!((r.removed_edges[1].from.as_str(), r.removed_edges[0].to.as_str()), ("c", "b"));
        assert_eq!(r.decisions[0].title, "Оставил флаг");
        assert_eq!(r.decisions[0].nodes, vec!["a"]);
    }

    #[core::prelude::v1::test]
    fn node_hunks_by_header_or_name() {
        let patches = vec![("src/m.js".to_owned(), "@@ -1,2 +1,3 @@ function sum\n a\n+b\n@@ -10,2 +11,4 @@ function sub\n+export function sub(a, b) {\n+}\n".to_owned())];
        let g = map::build_report(
            &serde_json::from_str::<Report>(r#"{"code":{"added":[{"name":"sub","file":"m.js"}],"changed":[{"name":"sum","file":"m.js","hunks":["@@ -1,2 +1,3 @@"]}]}}"#).unwrap(),
        );
        let sub = &g.nodes[g.index("sub").unwrap()];
        assert!(node_hunks(sub, &patches)[0].contains("export function sub"));
        let sum = &g.nodes[g.index("sum").unwrap()];
        assert!(node_hunks(sum, &patches)[0].starts_with("@@ -1,2"));
    }

    #[core::prelude::v1::test]
    fn steps_are_humanized() {
        let out = Path::new("/x/out");
        assert_eq!(humanize_step("$ node /s/scripts/diagram.mjs --src /x/out/flow.mmd", out), "Рисую диаграмму");
        assert_eq!(humanize_step("$ git diff -- src", out), "Смотрю git diff");
        assert_eq!(humanize_step("Read /x/out/base.json", out), "Читаю прошлую карту");
    }
}
