//! «Изменения»: a map of what a session changed, built on demand by a
//! reviewer subagent of the session's own family (Claude or Codex) and drawn
//! natively. Every rebuild and every batch of a long session is its own run
//! (`~/.stepan_verify/state/changes/<session>/runs/<n>/`), so the map keeps its history:
//! you can step through iterations and see which decisions the agent took.

mod arch;
pub(crate) mod canvas;
mod contracts;
mod map;
pub mod media;
pub mod plan;
mod reviewed;
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
    button::Button,
    menu::{DropdownMenu as _, PopupMenuItem},
    input::{Input, InputState},
    skeleton::Skeleton,
    spinner::Spinner,
    switch::Switch,
    text::TextView,
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use crate::zoom::{px, units};
use stepan_verify_agents::{AgentEvent, AgentHandle, ItemKind, Mode, Provider, SessionSpec};
use stepan_verify_core::flows::{self as flowmap, FlowMap, FlowProposal, Freshness, Touched};
use serde::{Deserialize, Serialize};

use crate::{
    format::ago,
    ui::{self, Palette, PressButton, Tone},
};
use map::{EdgeKind, Graph, Mode as MapMode, NodeStatus};
use reviewed::Review;
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
    /// report.json of the approved plan of the session's task: the map shows plan vs fact.
    pub plan: Option<PathBuf>,
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
    /// «К чему идём»: one sentence on where this session takes the project.
    pub goal: String,
    pub summary: String,
    /// Why each module of arch.json was added or changed, in the reviewer's words.
    pub modules: Vec<ModuleNote>,
    /// Why each dependency of arch.json was added, removed or bumped.
    pub deps: Vec<DepNote>,
    pub flow: Option<Diagram>,
    pub structure: Option<Diagram>,
    /// Extra diagrams of whatever type tells the story best: sequence, state, ER, class, …
    pub diagrams: Vec<Diagram>,
    /// «Как устроено»: show-me views — call/component/file trees, pseudocode,
    /// code shapes (often as было → стало diffs) and at most one HTML explainer.
    pub views: Vec<View>,
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
    /// The project's user flows this session went through, and changes to the flow map.
    pub flows: ReportFlows,
    /// A plan: vertical slices, each with what you can run after it and how to check.
    pub stages: Vec<stepan_verify_core::plans::Stage>,
    /// A plan: decisions for you, with the agent's recommendation.
    pub questions: Vec<stepan_verify_core::plans::Question>,
}

/// The reviewer's word on a module the app found changed (`arch.json`).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct ModuleNote {
    /// Path from the repository root, as in arch.json.
    pub path: String,
    pub note: String,
}

/// The reviewer's word on a dependency change.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct DepNote {
    pub name: String,
    pub why: String,
}

/// `flows` of a report: which steps of `.stepan_verify/flows.yaml` the session
/// touched, and what the reviewer proposes to change in the map.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct ReportFlows {
    pub touched: Vec<Touched>,
    pub proposal: FlowProposal,
}

/// How well something is proven, on the ladder «сказано → указана строка →
/// запущено → воспроизведено» (claimed → pointed → ran → reproduced).
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Evidence {
    pub level: String,
    /// What backs it: `file:line`, a command, a test, a screenshot path.
    #[serde(rename = "ref", alias = "reference")]
    pub reference: String,
}

impl Evidence {
    /// Rank on the ladder: 0 none, 1 claimed … 4 reproduced.
    pub fn rank(&self) -> u8 {
        evidence_rank(&self.level)
    }
}

pub fn evidence_rank(level: &str) -> u8 {
    match level.trim().to_lowercase().as_str() {
        "reproduced" | "воспроизведено" => 4,
        "ran" | "run" | "запущено" => 3,
        "pointed" | "line" | "указана строка" => 2,
        "claimed" | "said" | "сказано" => 1,
        _ => 0,
    }
}

/// What another family's agent found when it tried to prove a risk.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct VerifyResult {
    /// confirmed | refuted | inconclusive | error
    pub verdict: String,
    pub level: String,
    pub command: String,
    pub output: String,
    pub reason: String,
    /// Who checked: «Codex · GPT-6.2-Sol».
    pub by: String,
    pub at: Option<chrono::DateTime<chrono::Utc>>,
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
    /// What proves the group works.
    pub evidence: Evidence,
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

/// Looks at one report: modules and imports, the contract surface, before/after
/// media; and the older canvas and show-me views.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Lens {
    Arch,
    Contracts,
    Media,
    Map,
    Views,
}

impl Lens {
    fn usage_id(self) -> &'static str {
        match self {
            Lens::Arch => "changes.arch",
            Lens::Contracts => "changes.contracts",
            Lens::Media => "changes.media",
            Lens::Map => "changes.map",
            Lens::Views => "changes.views",
        }
    }
}

/// One show-me view of how the change works.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct View {
    /// `call_tree` | `component_tree` | `file_tree` | `pseudocode` | `code` | `html`.
    pub kind: String,
    pub title: String,
    /// The body is in diff syntax (`+` / `-` / space in the first column).
    pub diff: bool,
    pub body: String,
    /// Language of a `code` view.
    pub lang: String,
    /// The HTML file of an `html` view, absolute or relative to the run folder.
    pub file: Option<String>,
    pub caption: String,
    /// Map nodes the view is about.
    pub nodes: Vec<String>,
}

impl View {
    fn kind_label(&self) -> &'static str {
        match self.kind.as_str() {
            "call_tree" => "дерево вызовов",
            "component_tree" => "дерево компонентов",
            "file_tree" => "файлы",
            "pseudocode" => "псевдокод",
            "html" => "HTML",
            _ => "код",
        }
    }
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
    pub evidence: Evidence,
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
    evidence: Evidence,
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
                Risk { id: o.id, severity: o.severity, title: o.title, detail: o.detail, fix: o.fix, nodes, evidence: o.evidence }
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
    pub evidence: Evidence,
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
        #[serde(default)]
        evidence: Evidence,
    },
}

impl From<CheckRepr> for Check {
    fn from(r: CheckRepr) -> Self {
        match r {
            CheckRepr::Text(title) => Check { title, ..Default::default() },
            CheckRepr::Obj { id, title, how, evidence } => Check { id, title, how, evidence },
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
    /// «Утвердить план»: this iteration.
    ApprovePlan(usize),
    /// Show the session that writes the plan.
    OpenSession,
}

/// The view shows a task's plan instead of a session's changes.
struct PlanMode {
    task: String,
    /// The task as on disk, re-read while the view is open.
    meta: Option<stepan_verify_core::Task>,
    /// Iteration numbers seen on disk.
    seen: Vec<usize>,
    /// A stage you clicked: its nodes light up on the canvas.
    stage: Option<usize>,
}

/// Another family's agent trying to prove (or refute) one risk.
struct VerifyJob {
    run: usize,
    risk: String,
    started: Instant,
    by: String,
    steps: Vec<String>,
    out: PathBuf,
    handle: AgentHandle,
    _events: Task<()>,
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
    /// Which look at the report is on screen.
    lens: Lens,
    /// «Архитектура»: было / стало / разница, and the module in the side panel.
    arch_mode: arch::Mode,
    arch_sel: Option<String>,
    /// arch.json of the run on screen, laid out (by run folder).
    arch_cache: std::cell::RefCell<Option<(PathBuf, Option<Rc<arch::Graph>>)>>,
    /// Ticked rows of «Проверь руками».
    checked: HashSet<usize>,
    /// Node keys put into the remark for the agent.
    remarks: Vec<String>,
    /// «Просмотрено» marks of this session (reviewed.json).
    reviewed: reviewed::Marks,
    /// Each reviewable node of the map on screen: its review state and current fingerprint.
    review: std::collections::HashMap<String, (Review, String)>,
    /// «Только новое»: hide nodes reviewed and unchanged since.
    only_new: bool,
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
    /// The project's `.stepan_verify/flows.yaml`, if it has one.
    flow_map: Option<FlowMap>,
    /// A step of a touched flow you clicked: its entities light up on the canvas.
    flow_focus: Option<(String, String)>,
    /// Risk checks by another agent family, per run number → risk key.
    verify: std::collections::HashMap<usize, std::collections::HashMap<String, VerifyResult>>,
    verifying: Option<VerifyJob>,
    /// Risks waiting for «Проверить все»: (run, risk key).
    verify_queue: Vec<(usize, String)>,
    /// Set when the view shows a task's plan.
    plan: Option<PlanMode>,
    /// The approved plan this session implements, for «План: выполнено N из M».
    fact_plan: Option<Report>,
    _ticker: Task<()>,
    _verify_ticker: Task<()>,
    _plan_watch: Task<()>,
}

impl EventEmitter<ChangesEvent> for ChangesView {}

impl ChangesView {
    /// Shows the latest report, or starts building one.
    pub fn new(input: ChangesInput, cx: &mut Context<Self>) -> Self {
        let out = out_dir(&input.key);
        if let Err(err) = runs::migrate(&out) {
            eprintln!("stepan_verify: changes: migrate {}: {err}", out.display());
        }
        let mut this = Self::blank(input, out, None, cx);
        stepan_verify_core::usage::hit("changes.open");
        media::observe(cx);
        this.reload(None);
        // Debug hook: STEPAN_VERIFY_CHANGES_LENS=arch|contracts|media|map|views opens that tab.
        this.lens = match std::env::var("STEPAN_VERIFY_CHANGES_LENS").as_deref() {
            Ok("contracts") => Lens::Contracts,
            Ok("media") => Lens::Media,
            Ok("map") => Lens::Map,
            Ok("views") => Lens::Views,
            _ => Lens::Arch,
        };
        if this.runs.is_empty() {
            this.start(cx);
        } else {
            // Debug hook: STEPAN_VERIFY_CHANGES_RUN=full|batch starts a run right away.
            match std::env::var("STEPAN_VERIFY_CHANGES_RUN").as_deref() {
                Ok("full") => this.launch(RunKind::Full, cx),
                Ok("batch") => this.launch(RunKind::Batch, cx),
                _ => {}
            }
            // Debug hook: STEPAN_VERIFY_VERIFY_RISKS=all checks every risk of the shown run.
            if std::env::var("STEPAN_VERIFY_VERIFY_RISKS").as_deref() == Ok("all") {
                if let Some((n, keys)) = this.selected.and_then(|i| this.runs.get(i)).and_then(|r| Some((r.n, r.ok()?.risks.iter().enumerate().map(|(i, x)| risk_key(i, x)).collect::<Vec<_>>()))) {
                    for k in keys {
                        this.verify_risk(n, k, cx);
                    }
                }
            }
        }
        this
    }

    /// The plan of task `task`: the map of the target state the planning
    /// agent wrote (`state/plans/<task>`), newest iteration first. Nothing
    /// runs from here; the view follows the folder as the agent writes.
    pub fn plan(input: ChangesInput, task: String, cx: &mut Context<Self>) -> Self {
        let home = stepan_verify_core::Home::locate();
        let out = stepan_verify_core::plans::dir(&home, &task);
        let mode = PlanMode { task, meta: None, seen: Vec::new(), stage: None };
        let mut this = Self::blank(input, out, Some(mode), cx);
        this.poll_plan(cx);
        this._plan_watch = cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_secs(2)).await;
            if this.update(cx, |this, cx| this.poll_plan(cx)).is_err() {
                break;
            }
        });
        this
    }

    /// New plan iterations and task changes on disk.
    fn poll_plan(&mut self, cx: &mut Context<Self>) {
        let Some(plan) = &self.plan else { return };
        let home = stepan_verify_core::Home::locate();
        let seen = stepan_verify_core::plans::runs(&home, &plan.task);
        let meta = stepan_verify_core::Board::new(home).get(&plan.task).ok();
        let fresh = seen != plan.seen;
        let moved = meta.as_ref().map(|t| (t.meta.status, t.meta.phase, t.meta.plan_approved)) != plan.meta.as_ref().map(|t| (t.meta.status, t.meta.phase, t.meta.plan_approved));
        if let Some(plan) = &mut self.plan {
            plan.seen = seen;
            plan.meta = meta;
        }
        if fresh {
            // A new iteration is what you are waiting for: show it.
            self.reload(None);
        }
        if fresh || moved {
            cx.notify();
        }
    }

    fn plan_mode(&self) -> bool {
        self.plan.is_some()
    }

    fn blank(input: ChangesInput, out: PathBuf, plan: Option<PlanMode>, _cx: &mut Context<Self>) -> Self {
        let follow = plan.is_none() && runs::follows(&out);
        let marks = if plan.is_none() { reviewed::load(&out) } else { Default::default() };
        let fact_plan = input.plan.as_deref().and_then(load_report);
        Self {
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
            lens: if plan.is_some() { Lens::Map } else { Lens::Arch },
            arch_mode: arch::Mode::default(),
            arch_sel: None,
            arch_cache: Default::default(),
            checked: HashSet::new(),
            remarks: Vec::new(),
            reviewed: marks,
            review: Default::default(),
            only_new: false,
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
            flow_map: None,
            flow_focus: None,
            verify: Default::default(),
            verifying: None,
            verify_queue: Vec::new(),
            plan,
            fact_plan,
            _ticker: Task::ready(()),
            _verify_ticker: Task::ready(()),
            _plan_watch: Task::ready(()),
        }
    }

    /// Reread runs from disk; select `select` (a run number) or the newest good one.
    fn reload(&mut self, select: Option<usize>) {
        self.runs = runs::load_all(&self.out);
        if self.plan_mode() {
            // A plan has no session diff, no flows and no risk checks.
            let by_n = select.and_then(|n| self.runs.iter().position(|r| r.n == n));
            self.selected = by_n.or_else(|| self.runs.iter().rposition(|r| r.ok().is_some())).or(self.runs.len().checked_sub(1));
            return;
        }
        self.flow_map = FlowMap::load(&self.input.cwd).ok().flatten();
        for run in &self.runs {
            let checks = fs::read_to_string(run.dir.join("verify.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
            self.verify.insert(run.n, checks);
        }
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
        if input.plan != self.input.plan {
            self.fact_plan = input.plan.as_deref().and_then(load_report);
        }
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
        commands
            && match &last.meta.tree {
                Some(tree) => runs::capture(&self.input.cwd, &self.out, tree).is_some_and(|c| !c.files.is_empty()),
                // Runs from before the shadow capture.
                None => last.meta.git_snapshot.as_deref().is_some_and(|snap| !runs::diff(&self.input.cwd, Some(snap), last.meta.at.map(Into::into)).trim().is_empty()),
            }
    }

    fn toggle_follow(&mut self, on: bool, cx: &mut Context<Self>) {
        self.follow = on;
        if on {
            stepan_verify_core::usage::hit("changes.follow");
        }
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
        let model = crate::settings::report_model(provider, cx);
        let reviewer = reviewer_name(provider, model.as_deref());
        // The session's diff base: resolved on its first run, then kept.
        let git_base = self
            .runs
            .iter()
            .find_map(|r| r.meta.base.clone())
            .filter(|b| runs::base_ok(&self.input.cwd, b))
            .or_else(|| runs::resolve_base(&self.input.cwd, media::session_base(&self.input.key).as_deref()));
        let mut meta = Meta {
            at: Some(chrono::Utc::now()),
            kind,
            upto: self.input.log.len(),
            base_run: base.as_ref().filter(|_| kind == RunKind::Batch).map(|b| b.0),
            git_snapshot: None,
            base: git_base.clone(),
            tree: None,
            edits: 0,
            reviewer: reviewer.clone(),
            error: None,
            cwd: Some(self.input.cwd.clone()),
        };
        let written = match (kind, &base) {
            (RunKind::Batch, Some((_, base_dir, base_meta))) => self.write_batch_context(&dir, base_dir, base_meta, git_base.as_deref()),
            _ => self.write_full_context(&dir, git_base.as_deref()),
        };
        // Modules, imports and dependencies, computed from the files: the «Архитектура»
        // and «Контракты» tabs draw them, the reviewer only adds what they are for.
        // A batch keeps the base of the first run, so the graph covers the whole session;
        // a full run starts from the session's diff base, so the agent's commits stay in.
        let arch_base = base
            .as_ref()
            .filter(|_| kind == RunKind::Batch)
            .and_then(|(_, base_dir, _)| arch::load(base_dir))
            .map(|a| a.base)
            .or_else(|| git_base.clone())
            .unwrap_or_else(|| "HEAD".to_owned());
        if let Some(a) = stepan_verify_core::arch::compute(&self.input.cwd, &arch_base, &runs::is_noise) {
            if let Ok(text) = serde_json::to_string_pretty(&a) {
                let _ = fs::write(dir.join("arch.json"), text);
            }
        }
        // The flow map as it is now (compiled from the flow language): the reviewer reads it and proposes changes.
        if let Ok(Some(map)) = FlowMap::load(&self.input.cwd) {
            if let Ok(text) = serde_yaml::to_string(&map) {
                let _ = fs::write(dir.join("flows.yaml"), text);
            }
        }
        match written {
            Ok((edits, tree)) => {
                meta.edits = edits;
                meta.tree = tree;
            }
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

    /// Review state of every card of the map on screen, against its content now.
    fn refresh_review(&mut self, g: &Graph) {
        let patches = self.patches();
        self.review = g
            .nodes
            .iter()
            .filter(|n| reviewed::reviewable(n))
            .map(|n| {
                let fp = reviewed::fingerprint(n, &node_hunks(n, &patches));
                (n.key.clone(), (reviewed::review(&self.reviewed, &n.key, &fp), fp))
            })
            .collect();
    }

    /// Mark a node reviewed as it is now, or take the mark off a reviewed one.
    fn toggle_reviewed(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some((state, fp)) = self.review.get(key).cloned() else { return };
        if state == Review::Seen {
            self.reviewed.remove(key);
        } else {
            let run = self.selected.and_then(|i| self.runs.get(i)).map_or(0, |r| r.n);
            self.reviewed.insert(key.to_owned(), reviewed::Mark { run, fingerprint: fp, at: Some(chrono::Utc::now()) });
        }
        if let Err(err) = reviewed::save(&self.out, &self.reviewed) {
            eprintln!("stepan_verify: changes: reviewed.json: {err}");
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

    /// git.patch (this session's files) and other.patch (the rest of the
    /// folder) from `from` to the work tree now, listed in `md` under
    /// `heading`. Returns the captured tree; None outside git.
    fn write_git(&self, dir: &Path, from: &str, heading: &str, md: &mut String) -> anyhow::Result<Option<String>> {
        let i = &self.input;
        let Some(cap) = runs::capture(&i.cwd, &self.out, from) else { return Ok(None) };
        let edits: Vec<&str> = i.patches.iter().map(|(p, _)| p.as_str()).collect();
        let (own, other) = runs::split_own(&cap, &i.cwd, &edits, &i.commands);
        fs::write(dir.join("git.patch"), runs::tree_diff(&cap.top, from, &cap.tree, &own))?;
        let list = |md: &mut String, files: &[&runs::Changed]| {
            for f in files.iter().take(200) {
                match &f.from {
                    Some(old) => md.push_str(&format!("- {} {old} → {}\n", f.status, f.path)),
                    None => md.push_str(&format!("- {} {}\n", f.status, f.path)),
                }
            }
            if files.len() > 200 {
                md.push_str(&format!("- … и ещё {}\n", files.len() - 200));
            }
        };
        if !own.is_empty() {
            md.push_str(heading);
            list(md, &own);
        }
        if !other.is_empty() {
            fs::write(dir.join("other.patch"), runs::tree_diff(&cap.top, from, &cap.tree, &other))?;
            md.push_str(
                "\n## Изменено в папке, но не этой сессией (возможно, другим агентом или пользователем)\n\n\
                 Сессия не правила эти файлы инструментами и не называла их в командах. Не приписывай их ей; диффы — в other.patch. \
                 Бери файл в карту, только если по командам сессии видно, что это её работа (скрипт, генератор, сборка).\n\n",
            );
            list(md, &other);
        }
        Ok(Some(cap.tree))
    }

    /// context.md + session.patch + git.patch (+ other.patch) for a full run.
    /// Returns the edit count and the captured work tree.
    fn write_full_context(&self, dir: &Path, base: Option<&str>) -> anyhow::Result<(usize, Option<String>)> {
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
        let heading = |base: &str| {
            format!(
                "\n## Изменения этой сессии в git (git.patch)\n\nС базы `{}` до рабочего дерева сейчас: и коммиты агента после неё, и новые неотслеживаемые файлы.\n\n",
                &base[..base.len().min(10)]
            )
        };
        let tree = match base {
            Some(base) => self.write_git(dir, base, &heading(base), &mut md)?,
            None => None,
        };
        if tree.is_none() {
            fs::write(dir.join("git.patch"), runs::diff(&i.cwd, None, None))?;
        }
        fs::write(dir.join("context.md"), md)?;
        let patch: String = i.patches.iter().map(|(p, d)| format!("--- a/{p}\n+++ b/{p}\n{d}\n")).collect();
        fs::write(dir.join("session.patch"), patch)?;
        Ok((i.patches.len(), tree))
    }

    /// The part of the session after the base run, plus the base map.
    fn write_batch_context(&self, dir: &Path, base_dir: &Path, base_meta: &Meta, base: Option<&str>) -> anyhow::Result<(usize, Option<String>)> {
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
        // From the work tree the base run saw; its files now, minus the rest of the folder.
        let heading = "\n## Изменения этой сессии в git с прошлого прогона (git.patch)\n\n";
        let tree = match &base_meta.tree {
            Some(prev) => self.write_git(dir, prev, heading, &mut md)?,
            None => {
                // A base run from before the shadow capture: the old diff, and a tree for the next batch.
                let since = base_meta.at.map(Into::into);
                fs::write(dir.join("git.patch"), runs::diff(&i.cwd, base_meta.git_snapshot.as_deref(), since))?;
                base.and_then(|b| runs::capture(&i.cwd, &self.out, b)).map(|c| c.tree)
            }
        };
        fs::write(dir.join("context.md"), md)?;
        fs::write(dir.join("session.patch"), patch)?;
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
        for (list, key) in [("diagrams", "image"), ("views", "file")] {
            for d in v.get_mut(list).and_then(|l| l.as_array_mut()).into_iter().flatten() {
                if let Some(p) = d.get_mut(key) {
                    if let Some(rel) = p.as_str() {
                        *p = resolve(base_dir, rel).display().to_string().into();
                    }
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
        Ok((edits, tree))
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
        self.drag = Some(canvas::Drag::new((units(at.x), units(at.y)), self.view));
        self.dragged = false;
        cx.notify();
    }

    /// A window position relative to the canvas viewport.
    fn local(&self, at: Point<Pixels>) -> (f32, f32) {
        let o = self.viewport.get().map(|b| b.origin).unwrap_or_default();
        (units(at.x - o.x), units(at.y - o.y))
    }

    fn zoom_center(&mut self, factor: f32, cx: &mut Context<Self>) {
        let center = self.viewport.get().map_or((400., 300.), |b| (units(b.size.width) / 2., units(b.size.height) / 2.));
        self.view = self.view.zoom_at(factor, center);
        self.touched = true;
        cx.notify();
    }

    /// World width the frames pack into: the canvas at 100%.
    fn layout_target(&self) -> f32 {
        layout_width(self.viewport.get().map_or(1200., |b| units(b.size.width)))
    }

    /// Back to the readable start: 100% on the main feature.
    fn readable(&mut self, g: &Graph, cx: &mut Context<Self>) {
        if let Some(b) = self.viewport.get() {
            let size = (units(b.size.width), units(b.size.height));
            let c = map::canvas_layout(g, self.mode, layout_width(size.0));
            self.view = canvas::View::readable((c.width, c.height), map::main_frame(g, &c).map(|f| (f.x, f.y)), size);
            self.fit_size = size;
            self.touched = false;
            cx.notify();
        }
    }

    fn fit(&mut self, world: (f32, f32), cx: &mut Context<Self>) {
        if let Some(b) = self.viewport.get() {
            self.fit_size = (units(b.size.width), units(b.size.height));
            self.view = canvas::View::fit(world, self.fit_size);
            self.touched = false;
            cx.notify();
        }
    }

    // -------------------------------------------------------------- flows

    /// Steps of the project's flows this session went through: what the
    /// reviewer said, plus what the diff and the map's entities show.
    fn touched(&self, g: &Graph, r: &Report) -> Vec<Touched> {
        let det = match &self.flow_map {
            Some(map) => {
                let files: Vec<String> = self.patches().into_iter().map(|(f, _)| f).collect();
                let entities: Vec<String> = g
                    .nodes
                    .iter()
                    .filter(|n| !n.implicit && n.status != NodeStatus::Context)
                    .flat_map(|n| [n.name.clone(), n.entity.clone()])
                    .filter(|s| !s.trim().is_empty())
                    .collect();
                map.touched(&files, &entities)
            }
            None => Vec::new(),
        };
        let known = |t: &Touched| {
            self.flow_map.as_ref().is_some_and(|m| m.feature(&t.feature).is_some()) || r.flows.proposal.upsert.iter().any(|f| f.id == t.feature)
        };
        merge_touched(&r.flows.touched, &det).into_iter().filter(known).collect()
    }

    /// A feature by id: the map first, else a proposed one.
    fn flow_feature<'a>(&'a self, r: &'a Report, id: &str) -> Option<&'a flowmap::Feature> {
        self.flow_map.as_ref().and_then(|m| m.feature(id)).or_else(|| r.flows.proposal.upsert.iter().find(|f| f.id == id))
    }

    /// Id of the selected run's proposal for accept / reject bookkeeping.
    fn proposal_id(&self) -> Option<String> {
        self.selected.and_then(|i| self.runs.get(i)).map(|run| format!("{}#{}", self.input.key, run.n))
    }

    fn decide_proposal(&mut self, accept: bool, cx: &mut Context<Self>) {
        let (Some(id), Some(r)) = (self.proposal_id(), self.report().cloned()) else { return };
        let home = stepan_verify_core::Home::locate();
        let cwd = self.input.cwd.clone();
        if accept {
            let mut map = self.flow_map.clone().unwrap_or_else(|| FlowMap {
                version: 1,
                project: stepan_verify_core::worktree::project_name(&cwd).unwrap_or_default(),
                ..Default::default()
            });
            map.apply(&r.flows.proposal);
            if let Err(err) = map.save(&cwd) {
                eprintln!("stepan_verify: flows: save: {err:#}");
                return;
            }
            self.flow_map = Some(map);
        }
        let mut decisions = flowmap::Decisions::load(&home, &cwd);
        if accept {
            decisions.accepted.insert(id);
        } else {
            decisions.rejected.insert(id);
        }
        let _ = decisions.save(&home, &cwd);
        cx.notify();
    }

    // -------------------------------------------------------------- verify

    /// Ask an agent of the other family to prove or refute a risk by running something.
    fn verify_risk(&mut self, run_n: usize, key: String, cx: &mut Context<Self>) {
        if self.verifying.is_some() {
            if !self.verify_queue.contains(&(run_n, key.clone())) {
                self.verify_queue.push((run_n, key));
            }
            cx.notify();
            return;
        }
        let Some(run) = self.runs.iter().find(|r| r.n == run_n) else { return };
        let Some(report) = run.ok() else { return };
        let Some((i, risk)) = report.risks.iter().enumerate().find(|(i, r)| risk_key(*i, r) == key) else { return };
        let other = other_family(self.input.provider);
        let model = crate::settings::report_model(other, cx);
        let by = reviewer_name(other, model.as_deref());
        let vdir = run.dir.join("verify");
        let _ = fs::create_dir_all(&vdir);
        let out = vdir.join(format!("{}.json", sanitize(&key)));
        let _ = fs::remove_file(&out);
        let prompt = prompt_verify(&run.dir, &out, i, risk, self.input.provider);
        let spec = SessionSpec {
            provider: other,
            native_id: None,
            transcript: None,
            cwd: self.input.cwd.clone(),
            mode: Mode::FullAccess,
            image_cache: Some(stepan_verify_core::Home::locate().state().join("images")),
            env: vec![("STEPAN_VERIFY_VERIFY_OUT".into(), out.display().to_string())],
            model,
            instructions: None,
        };
        let (handle, events) = stepan_verify_agents::open(spec);
        handle.send(prompt);
        let pump = cx.spawn(async move |this, cx| {
            while let Ok(event) = events.recv().await {
                let done = matches!(event, AgentEvent::TurnCompleted { .. } | AgentEvent::Exited);
                let alive = this.update(cx, |this, cx| {
                    this.on_verify_event(event, cx);
                    cx.notify();
                });
                if alive.is_err() || done {
                    break;
                }
            }
        });
        self.verifying = Some(VerifyJob { run: run_n, risk: key, started: Instant::now(), by, steps: Vec::new(), out, handle, _events: pump });
        self._verify_ticker = cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            let going = this.update(cx, |this, cx| {
                cx.notify();
                this.verifying.is_some()
            });
            if !matches!(going, Ok(true)) {
                break;
            }
        });
        cx.notify();
    }

    fn on_verify_event(&mut self, event: AgentEvent, cx: &mut Context<Self>) {
        let Some(job) = &mut self.verifying else { return };
        match event {
            AgentEvent::Upsert(item) if item.kind == ItemKind::Tool && !item.title.is_empty() => {
                let step = humanize_step(&item.title, job.out.parent().unwrap_or(&job.out));
                if job.steps.last() != Some(&step) {
                    job.steps.push(step);
                }
            }
            AgentEvent::TurnCompleted { error } => self.finish_verify(error, cx),
            AgentEvent::Exited => self.finish_verify(Some("Проверяющий завершился".into()), cx),
            AgentEvent::Error(err) => self.finish_verify(Some(err), cx),
            _ => {}
        }
    }

    fn finish_verify(&mut self, error: Option<String>, cx: &mut Context<Self>) {
        let Some(job) = self.verifying.take() else { return };
        let mut result = fs::read_to_string(&job.out)
            .ok()
            .and_then(|t| serde_json::from_str::<VerifyResult>(&t).ok())
            .unwrap_or_else(|| VerifyResult {
                verdict: "error".into(),
                reason: error.unwrap_or_else(|| "Проверяющий не записал результат".into()),
                ..Default::default()
            });
        result.verdict = result.verdict.trim().to_lowercase();
        result.by = job.by.clone();
        result.at = Some(chrono::Utc::now());
        let checks = self.verify.entry(job.run).or_default();
        checks.insert(job.risk.clone(), result);
        if let Some(run) = self.runs.iter().find(|r| r.n == job.run) {
            let _ = fs::write(run.dir.join("verify.json"), serde_json::to_string_pretty(checks).unwrap_or_default());
        }
        if !self.verify_queue.is_empty() {
            let (run, key) = self.verify_queue.remove(0);
            self.verify_risk(run, key, cx);
        }
        cx.notify();
    }

    fn cancel_verify(&mut self, cx: &mut Context<Self>) {
        self.verify_queue.clear();
        if let Some(job) = &self.verifying {
            job.handle.interrupt();
        }
        self.finish_verify(Some("Проверка остановлена".into()), cx);
    }

    fn remark_text(&self, graph: &Graph, note: &str) -> String {
        let title = self.report().map(|r| r.title.clone()).filter(|t| !t.is_empty()).unwrap_or_else(|| self.input.title.clone());
        let mut text = match &self.plan {
            Some(plan) => {
                let draft = stepan_verify_core::plans::draft(&stepan_verify_core::Home::locate(), &plan.task);
                let run = self.selected.and_then(|i| self.runs.get(i)).map(|r| format!("Смотрел итерацию #{}.\n", r.n)).unwrap_or_default();
                format!("{}{run}", plan::remarks_header(&title, &draft))
            }
            None => format!("Ревью изменений «{title}» — верни на доработку:\n"),
        };
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

/// Union of touched steps by (feature, path), first-seen order kept.
pub fn merge_touched(a: &[Touched], b: &[Touched]) -> Vec<Touched> {
    let mut out: Vec<Touched> = Vec::new();
    for t in a.iter().chain(b) {
        if t.feature.trim().is_empty() {
            continue;
        }
        match out.iter_mut().find(|x| x.feature == t.feature && x.path == t.path) {
            Some(x) => {
                for s in &t.steps {
                    if !x.steps.contains(s) {
                        x.steps.push(s.clone());
                    }
                }
            }
            None => out.push(t.clone()),
        }
    }
    out
}

/// Stable key of a risk in a run: its id, else its position.
pub fn risk_key(i: usize, r: &Risk) -> String {
    if r.id.trim().is_empty() { format!("r{}", i + 1) } else { r.id.trim().to_owned() }
}

fn sanitize(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
}

/// Claude sessions are double-checked by Codex and the other way round.
pub fn other_family(p: Provider) -> Provider {
    match p {
        Provider::Claude => Provider::Codex,
        Provider::Codex => Provider::Claude,
    }
}

fn prompt_verify(run: &Path, out: &Path, i: usize, risk: &Risk, author: Provider) -> String {
    let nodes = if risk.nodes.is_empty() { String::new() } else { format!("\nЗатронутые узлы: {}", risk.nodes.join(", ")) };
    format!(
        "[stepan_verify verify] Ты — независимый проверяющий. Ревьюер-агент ({author}) нашёл риск в изменениях сессии. Твоя задача — подтвердить или опровергнуть его ЗАПУСКОМ, а не рассуждением.\n\n\
         Риск #{n}: {title}\nКогда и почему: {detail}\nКак поправить (по мнению ревьюера): {fix}{nodes}\n\n\
         Контекст сессии: {run}/context.md, диффы: {run}/git.patch и {run}/session.patch, отчёт ревьюера: {run}/report.json. Текущая папка — проект.\n\n\
         Как: найди самую дешёвую команду, которая показывает, есть проблема или нет (тест, скрипт, `node -e`, curl к локальному серверу, сборка, типизация), запусти её и посмотри вывод. \
         Если для эксперимента нужно менять код — сделай `git worktree add` во временной папке и удали его после; рабочее дерево проекта не трогай. Ничего не коммить, не пушить. \
         Останови всё, что запустил. Уложись примерно в 5 минут.\n\n\
         Запиши результат в {out} — JSON: {{\"verdict\": \"confirmed|refuted|inconclusive\", \"level\": \"reproduced|ran|pointed|claimed\", \"command\": \"что запускал\", \"output\": \"ключевые строки вывода, до 40\", \"reason\": \"одна-две фразы по-русски: почему такой вердикт\"}}. \
         confirmed — только если запуск показал проблему; refuted — если запуск показал, что её нет; иначе inconclusive (и честно напиши, чего не хватило). Когда записал — ответь одним словом: готово.",
        author = match author {
            Provider::Claude => "Claude",
            Provider::Codex => "Codex",
        },
        n = i + 1,
        title = risk.title,
        detail = if risk.detail.is_empty() { "—" } else { &risk.detail },
        fix = if risk.fix.is_empty() { "—" } else { &risk.fix },
        run = run.display(),
        out = out.display(),
    )
}

/// Frames pack into rows as wide as the canvas at 100%, minus its margins.
fn layout_width(viewport_w: f32) -> f32 {
    (viewport_w - canvas::FIT_MARGIN * 2.).max(560.)
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

const SCHEMA: &str = "{\"title\": \"заголовок изменений\", \"goal\": \"к чему идём: одна фраза, куда сессия ведёт проект (не что сделано, а зачем)\", \"summary\": \"2–4 предложения простым языком: что изменилось для продукта и пользователя\",\n\
 \"modules\": [{\"path\": \"путь из arch.json\", \"note\": \"зачем этот модуль появился или что в нём поменялось, одной фразой\"}],\n\
 \"deps\": [{\"name\": \"пакет из arch.json\", \"why\": \"зачем добавлен, убран или обновлён\"}],\n\
 \"flow\": {\"title\": \"…\", \"image\": \"flow.png\", \"mermaid\": \"исходник\", \"caption\": \"одна фраза\"} или null,\n\
 \"structure\": {\"title\": \"…\", \"image\": \"structure.png\", \"caption\": \"…\"} или null,\n\
 \"diagrams\": [{\"title\": \"…\", \"image\": \"lifecycle.png\", \"mermaid\": \"исходник\", \"caption\": \"…\"}],\n\
 \"views\": [{\"kind\": \"call_tree|component_tree|file_tree|pseudocode|code|html\", \"title\": \"что показывает вид\", \"diff\": true, \
\"body\": \"текст вида, строки через \\n (у html пусто)\", \"lang\": \"ts (только у code)\", \"file\": \"explainer.html (только у html)\", \"caption\": \"одна фраза\", \"nodes\": [\"id\"]}],\n\
 \"code\": {\"added\": [{\"id\": \"короткий_id\", \"name\": \"имя как в коде\", \"kind\": \"function|component|hook|endpoint|route|type|table|config|test|file|other\", \
\"role\": \"component|function|hook|type|dto|config|test|api|store|style|script|other\", \"entity\": \"человеческое имя сущности\", \
\"file\": \"путь от корня проекта\", \"line\": 42, \"note\": \"зачем, одной фразой\", \
\"signature\": \"takeFlight(card: JokerCard, from: Rect) → Promise<void>\", \"signature_before\": \"только если интерфейс менялся\", \
\"inputs\": [\"id DTO\"], \"outputs\": [\"id DTO\"], \"uses\": [\"id\", {\"to\": \"id\", \"via\": \"id DTO\", \"note\": \"что и зачем передаёт\"}], \
\"hunks\": [\"@@ -41,19 +46,33 @@\"], \"risk_ids\": [\"r1\"], \"check_ids\": [\"c1\"]}], \"removed\": [], \"changed\": [], \"context\": []},\n\
 \"dtos\": [{\"id\": \"joker_props\", \"name\": \"JokerPickDialogProps\", \"kind\": \"props|args|response|request|event|state|type\", \"file\": \"путь\", \
\"status\": \"added|changed|removed|context\", \"note\": \"что это за данные\", \"fields\": [{\"name\": \"flyToHub\", \"type\": \"boolean?\", \"status\": \"added|changed|removed|context\", \"note\": \"зачем\"}], \
\"used_by\": [\"id узла\"]}],\n\
 \"features\": [{\"id\": \"f1\", \"title\": \"функционал простыми словами\", \"summary\": \"что он даёт пользователю\", \"kind\": \"feature|helper|infra|tests\", \"nodes\": [\"id\"], \"evidence\": {\"level\": \"claimed|pointed|ran|reproduced\", \"ref\": \"что доказывает: тест, команда, file:line, скриншот\"}}],\n\
 \"tests\": [{\"id\": \"t1\", \"title\": \"что проверяет\", \"file\": \"путь\", \"verifies\": [\"id узла\"], \"kind\": \"unit|e2e|snapshot|manual\", \"status\": \"added|changed|existing\", \"result\": \"pass|fail|not_run\"}],\n\
 \"removed_edges\": [[\"id откуда\", \"id куда\"]],\n\
 \"ui\": [{\"title\": \"экран/элемент\", \"before\": null, \"after\": \"путь\", \"note\": \"что поменялось\"}],\n\
 \"risks\": [{\"id\": \"r1\", \"severity\": \"high|mid|low\", \"title\": \"что может сломаться\", \"detail\": \"когда и почему\", \"fix\": \"как поправить\", \"nodes\": [\"id\"], \"evidence\": {\"level\": \"claimed|pointed|ran|reproduced\", \"ref\": \"file:line или команда с выводом\"}}],\n\
 \"check\": [{\"id\": \"c1\", \"title\": \"что проверить руками\", \"how\": \"как именно\", \"evidence\": {\"level\": \"…\", \"ref\": \"…\"}}],\n\
 \"decisions\": [{\"title\": \"решение агента\", \"why\": \"почему так (по его словам или по коду)\", \"nodes\": [\"id\"], \"at\": \"когда в сессии\"}],\n\
 \"flows\": {\"touched\": [{\"feature\": \"id фичи из flows.yaml\", \"path\": \"id пути\", \"steps\": [\"id шага\"]}], \
\"proposal\": {\"areas\": [{\"id\": \"…\", \"title\": \"…\"}], \"upsert\": [{\"id\": \"area.feature\", \"title\": \"как назвал бы пользователь\", \"area\": \"id\", \"summary\": \"…\", \"aliases\": [], \
\"paths\": [{\"id\": \"…\", \"title\": \"…\", \"steps\": [{\"id\": \"…\", \"screen\": \"где пользователь\", \"action\": \"что делает\", \"result\": \"что видит\", \"entities\": [\"имена из code\"], \"files\": [\"путь\"]}]}], \
\"proofs\": [{\"kind\": \"test|screenshot|video|cli|manual\", \"title\": \"…\", \"reference\": \"путь или команда\", \"covers\": [\"id шага\"], \"at\": \"ISO-время, если проверено сейчас\"}]}], \
\"remove\": [\"id фичи\"], \"notes\": [\"+ новая фича «…»\", \"~ шаг «…» теперь …\"]}}}";

const FLOW_RULES: &str = "Флоу пользователя: если есть $OUT/flows.yaml — это карта флоу проекта (фичи → пути пользователя шагами «экран → действие → результат» → доказательства). \
В `flows.touched` отметь, какие шаги каких путей задела сессия (id из flows.yaml). В `flows.proposal` предложи правки карты: новые фичи и шаги, которые появились, изменившиеся шаги \
(upsert — фича целиком, с сохранёнными id), удалённые фичи (remove), новые доказательства (proofs: тест, скриншот, видео, CLI-сценарий — только реально существующие). \
`notes` — по строке на правку для человека; каждая заметка о правке шага или фичи ОБЯЗАНА сопровождаться этой фичей в `upsert` (скопируй её из flows.yaml целиком и поменяй) — заметки без upsert в карту не попадут. Если flows.yaml нет — предложи в `proposal.upsert` фичи, которые затронула сессия (это станет началом карты). \
Пиши словами пользователя, а не кода: «Выбор карты Джокера», а не JokerPickDialog. Если сессия флоу не меняла — оставь `proposal` пустым.";

const EVIDENCE_RULES: &str = "Доказанность: у каждого риска, проверки и группы `features` поле `evidence` — уровень по лестнице: \
`claimed` (только сказано, словами агента или твоими), `pointed` (указана строка/файл, где это видно), `ran` (запускали команду или тест и видели вывод — в `ref` команда и итог), \
`reproduced` (воспроизвели в работающем приложении: скриншот, видео, сценарий). Не завышай: если ничего не запускали — максимум `pointed`. \
Если группе нечем доказать — `evidence` с пустым level, это сигнал человеку.";

const MAP_RULES: &str = "Карта — главное в отчёте: приложение само рисует граф из `code`. Для каждой записи выпиши `uses` (что она вызывает, рендерит, использует — id других записей или имена), \
у удалённых — какими были. Связи, которые сессия убрала у оставшихся записей, перечисли в `removed_edges`. Неизменённый код рядом (кто вызывает новое, что использует изменённое) — в `code.context`, 3–10 узлов. \
`hunks` — заголовки ханков из диффа (строка `@@ … @@`), которые относятся к записи. Риски и проверки привязывай к узлам (`nodes`, `risk_ids`, `check_ids`). \
`signature` — у КАЖДОЙ записи: интерфейс псевдокодом в стиле TypeScript, суть входа и выхода, а не копия кода (компонент: `Name({ a, b? }: Props) → JSX`, функция: `name(x: T, y: U) → R`, эндпоинт: `POST /path (Body) → Response`, конфиг: `NAME: Type = значение`). \
Если интерфейс поменялся — прежний в `signature_before`. `dtos` — все данные, которые передаются между узлами карты: пропсы компонентов, аргументы, тела и ответы API, события, куски состояния; \
у каждой структуры поля с типами, изменённые поля помечай `status`. В `uses` указывай `via` — какой DTO едет по связи; `inputs`/`outputs` у записи — какие DTO она принимает и отдаёт. \
Если session.patch пуст, агент правил файлы командами — бери изменения из git.patch. \
other.patch (если есть) — файлы, изменённые в папке не этой сессией (другим агентом или человеком): в карту их не бери, если по командам сессии не видно, что это её работа. \
Человек смотрит на карту как на канвас смыслов, а не реализации: сгруппируй записи в `features` по смыслу — функциональность для пользователя (kind `feature`, «эти штуки делают функционал X», 1–5 групп), \
то, что ей помогает (`helper`: утилиты, константы, стили, обёртки, типы), инфраструктура и конфиг (`infra`). Каждая запись — ровно в одной группе; группы называй по-человечески, не по файлам; \
`summary` группы — одна фраза, что она даёт. У каждой записи `role` (кто это: компонент, функция, хук, тип, конфиг…) и `entity` — короткое человеческое имя («Диалог выбора Джокера», «Полёт карты в альбом»). \
Низкоуровневые мелочи не плоди отдельными узлами — сворачивай в helper-группу. `tests` — тесты, которые сессия добавила или поменяла, и существующие, что проверяют изменённое (поищи в проекте рядом с файлами); \
`verifies` — id узлов, которые тест проверяет; `result` — только если видно, что тест запускали. Если тестов нет — оставь `tests` пустым, это тоже сигнал.";

const ARCH_RULES: &str = "Структура: $OUT/arch.json посчитало приложение из файлов, а не модель — модули (файлы) со статусом added|changed|removed|same и +/- строк, \
импорты между ними было/стало (`edges`: from импортирует to, status added|removed|same), изменения зависимостей в манифестах (`deps`) и lock-файлах (`locks`). \
Не пересказывай и не исправляй его: человек увидит граф сам. Твоя часть — смысл: `goal` (к чему идём), `modules` — по фразе на каждый added/changed/removed модуль из arch.json \
(`path` ровно как там: зачем модуль появился, что в нём теперь, почему удалён), `deps` — по фразе на каждый пакет из arch.json deps (зачем). \
В `code` у записей пиши `file` тем же путём, что в arch.json, чтобы записи легли в свои модули. Если arch.json нет или он пуст — пропусти `modules` и `deps`.";

const VIEW_RULES: &str = "Как устроено (`views`, приём скилла show-me): 1–4 компактных вида того, КАК теперь работает сделанное — то, что человек иначе выяснял бы чтением кода. \
Бери самый маленький вид, который объясняет суть: поток выполнения — `call_tree` (дерево вызовов, отступ 2 пробела); UI — `component_tree` (компоненты с путями файлов в скобках, хуки и границы состояния, которые важны); \
раскладка файлов или рефакторинг — `file_tree` (неглубокое дерево `├──`/`└──`, у каждой строки `# зачем`); логика или алгоритм — `pseudocode`; новая форма целиком (интерфейс, тип, сигнатуры) — `code` с `lang`. \
Если форма уже была и изменилась — пиши в diff-синтаксисе, как `git diff` без заголовков: первая колонка `+` (добавлено), `-` (убрано) или пробел (контекст, оставь его), и ставь `diff: true`. \
Только то, что нужно, чтобы понять изменение: до ~25 строк на вид, имена как в коде, без пересказа всего файла. `nodes` — id узлов карты, о которых вид. Не дублируй схему flow. \
Если суть — визуальный UI, сравнение состояний или идея слишком плотная для схемы — один HTML-файл $OUT/explainer.html (вид `html`, поле `file`, `body` пустой): самодостаточный (без сети и внешних шрифтов), \
светлая и тёмная тема через prefers-color-scheme, реальные имена и данные из кода, одна мысль; не больше одного такого файла и только когда он правда нужен.";

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
         session.patch (правки инструментами), git.patch (файлы этой сессии в git с начала сессии, включая коммиты агента), \
         если есть, other.patch (изменено в папке не этой сессией) и arch.json (модули и импорты было/стало, зависимости — посчитано приложением). Текущая папка — проект.\n\n\
         Шаги:\n\
         1. Прочитай $OUT/context.md, $OUT/session.patch, $OUT/git.patch и $OUT/arch.json.\n\
         2. Что добавлено, удалено и изменено на уровне функций, компонентов, хуков, эндпоинтов, роутов, типов, таблиц, конфигов. Имена — как в коде. {MAP_RULES}\n\
         2а. {ARCH_RULES}\n\
         3. Флоу: схема, как теперь работает то, что делала сессия: 5–12 узлов, подписи по-русски, в $OUT/flow.mmd. Тип выбирай под историю, а не всегда flowchart: \
поток запроса — sequenceDiagram, кто что делает — swimlane-beta, жизненный цикл — stateDiagram-v2. Если поменялась структура модулей — before.mmd/after.mmd (тем же типом), иначе пропусти. \
Если сессия меняла то, что лучше видно другой схемой (схема БД — erDiagram, классы/интерфейсы — classDiagram, статусы — stateDiagram-v2, папки — treeView-beta, план — gantt), \
добавь 0–3 такие в `diagrams` (было/стало, если это изменение, иначе одна). Не рисуй схему ради схемы.\n\
         3а. {VIEW_RULES}\n\
         4. UI: скриншоты из context.md — как «Стало». «Было» — только если это дёшево и честно, иначе null. Никогда не подделывай «Было».\n\
         5. Решения: 3–7 главных решений агента за сессию (`decisions`) — что выбрал и почему, к каким узлам.\n\
         5а. {FLOW_RULES}\n\
         5б. {EVIDENCE_RULES}\n\
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
         сообщения человека и агента, команды, правки. $OUT/session.patch — новые правки инструментами, $OUT/git.patch — изменения файлов сессии в git с прошлого шага, $OUT/other.patch (если есть) — изменено в папке не этой сессией. Текущая папка — проект.\n\n\
         Сделай:\n\
         1. Прочитай base.json и дельту. Не перечитывай весь проект: только то, что нужно, чтобы понять новые правки.\n\
         2. Обнови карту: сохрани id и записи из base.json, добавь новые узлы, поменяй статус/описание/связи затронутых, перенеси откаты. {MAP_RULES}\n\
         2а. {ARCH_RULES} arch.json — за всю сессию на сейчас: перепиши `modules` и `deps` под него.\n\
         3. `decisions` — ТОЛЬКО решения из этой порции: что агент выбрал и почему, к каким узлам; `at` — момент в сессии.\n\
         4. title/goal/summary/риски/проверки обнови под итог на сейчас. Поля flow/structure/diagrams/ui скопируй из base.json как есть (схемы не перерисовывай).\n\
         4а. {FLOW_RULES} В батче `flows.touched` — шаги, задетые за всю сессию (объедини с base.json), `proposal` — все предложения сессии на сейчас (объедини с base.json).\n\
         4б. {EVIDENCE_RULES}\n\
         4в. `views` (как устроено): оставь виды из base.json; поправь или добавь текстовый вид, только если порция поменяла форму кода (дерево вызовов, компонентов, файлов, псевдокод). Вид `html` копируй как есть. {VIEW_RULES}\n\
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
        ("other.patch", "Смотрю чужие изменения в папке"),
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

/// A report from disk; None when missing or unreadable.
fn load_report(path: &Path) -> Option<Report> {
    fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok())
}

impl ChangesView {
    /// A screenshot path from the report: the run folder first, then the project
    /// (pr-before-after keeps pairs in `docs/pr/<slug>/`).
    fn resolve_shot(&self, dir: &Path, p: &str) -> PathBuf {
        let in_run = resolve(dir, p);
        if in_run.exists() { in_run } else { self.input.cwd.join(p) }
    }
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

/// The model the next report of this family runs on; the same setting as
/// Настройки → «Отчёт „Изменения“», so picking here changes it there too.
fn model_picker(provider: Provider, current: String, running: bool) -> impl IntoElement {
    Button::new("changes-model")
        .small()
        .outline()
        .dropdown_caret(true)
        .label(current)
        .tooltip(if running { "Модель для следующего прогона (текущий уже идёт)" } else { "Модель отчёта · то же, что в Настройках" })
        .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, cx| {
            let chosen = crate::settings::report_model(provider, cx);
            let options: Vec<(Option<String>, String)> = match provider {
                Provider::Claude => [("haiku", "~40 с"), ("sonnet", "1–3 мин"), ("opus", "3–6 мин")]
                    .into_iter()
                    .map(|(m, time)| (Some(m.to_owned()), format!("{} · {time}", reviewer_name(provider, Some(m)))))
                    .collect(),
                Provider::Codex => {
                    let default = stepan_verify_core::config::codex_default_model().map(|m| format!("по умолчанию · {m}")).unwrap_or_else(|| "по умолчанию".into());
                    std::iter::once((None, default))
                        .chain(stepan_verify_core::config::codex_models().into_iter().map(|(slug, name)| (Some(slug), name)))
                        .collect()
                }
            };
            options.into_iter().fold(menu.label(format!("Отчёт для {}-сессий", provider_label(provider))), |menu, (model, label)| {
                let checked = model == chosen;
                menu.item(PopupMenuItem::new(label).checked(checked).on_click(move |_, _, cx| {
                    let model = model.clone();
                    crate::settings::update(cx, move |c| match provider {
                        Provider::Claude => c.changes.model = model.unwrap_or_else(|| "sonnet".into()),
                        Provider::Codex => c.changes.codex_model = model.unwrap_or_default(),
                    });
                }))
            })
        })
}

fn provider_label(provider: Provider) -> &'static str {
    match provider {
        Provider::Claude => "Claude",
        Provider::Codex => "Codex",
    }
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

/// A node's status in words; a plan speaks of the future.
fn status_label(s: NodeStatus, plan: bool) -> &'static str {
    match (s, plan) {
        (NodeStatus::Added, false) => "новое",
        (NodeStatus::Changed, false) => "изменено",
        (NodeStatus::Removed, false) => "удалено",
        (NodeStatus::Context, false) => "не менялось",
        (NodeStatus::Added, true) => "появится",
        (NodeStatus::Changed, true) => "изменится",
        (NodeStatus::Removed, true) => "уберётся",
        (NodeStatus::Context, true) => "не меняется",
    }
}

/// How well a canvas group is proven: the reviewer's word for it, raised by
/// tests that verify its entries (a passing one counts as «запущено»).
fn group_rank(r: &Report, g: &Graph, gi: usize) -> u8 {
    let gr = &g.groups[gi];
    let stated = r.features.iter().find(|f| !f.id.is_empty() && map::normalize(&f.id) == gr.key).map_or(0, |f| f.evidence.rank());
    let tests = gr
        .nodes
        .iter()
        .flat_map(|&i| g.tests_of(i))
        .map(|t| match g.nodes[t].test.as_ref().map(|x| x.result.as_str()) {
            Some("pass" | "passed" | "ok") => 3,
            _ => 2,
        })
        .max()
        .unwrap_or(0);
    stated.max(tests)
}

/// Badge for a level on the proof ladder.
fn evidence_badge(rank: u8, p: &Palette) -> (&'static str, Tone) {
    match rank {
        4 => ("воспроизведено", p.green),
        3 => ("запущено", p.blue),
        2 => ("указана строка", Tone { solid: p.faint, soft: p.surface2, text: p.muted }),
        1 => ("только сказано", p.amber),
        _ => ("нет доказательства", p.red),
    }
}

fn freshness_badge(f: Freshness, p: &Palette) -> (&'static str, Tone) {
    match f {
        Freshness::Fresh => ("доказано", p.green),
        Freshness::Stale => ("доказательство устарело", p.amber),
        Freshness::Missing => ("не доказано", p.red),
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
        let mut graph = self.report().map(|r| map::build_report(r));
        match &mut graph {
            Some(g) if !self.plan_mode() => {
                self.refresh_review(g);
                if self.only_new {
                    g.hidden = self.review.iter().filter(|(_, (s, _))| *s == Review::Seen).map(|(k, _)| k.clone()).collect();
                }
            }
            _ => self.review.clear(),
        }
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
            let key = format!("{run}-{:?}-{}", self.mode, self.only_new);
            let size = self.viewport.get().map(|b| (units(b.size.width), units(b.size.height)));
            let resized = size.is_some_and(|(w, h)| (w - self.fit_size.0).abs() > 1. || (h - self.fit_size.1).abs() > 1.);
            if self.fit_key.as_deref() != Some(key.as_str()) || (resized && !self.touched) {
                match size {
                    Some(size) => {
                        let c = map::canvas_layout(g, self.mode, layout_width(size.0));
                        // Open readable (100%) on the main feature, not shrunk to fit everything.
                        let focus = map::main_frame(g, &c).map(|b| (b.x, b.y));
                        self.view = canvas::View::readable((c.width, c.height), focus, size);
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
                let canvas_h = (units(window.viewport_size().height) - 340.).max(440.);
                self.render_report(g, canvas_h, &p, &theme, window, cx).into_any_element()
            }
            (None, _, Some(r)) if self.selected.is_none() || self.selected.and_then(|i| self.runs.get(i)).is_some_and(|run| run.n == r.n) => {
                self.render_running(&p, &theme, cx).into_any_element()
            }
            (None, run, _) if self.plan_mode() => {
                let err = run.and_then(|r| match &r.report {
                    Some(Err(e)) => Some(e.clone()),
                    _ => r.meta.error.clone(),
                });
                self.render_plan_pending(err, &p, &theme, cx).into_any_element()
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
                        // With a report the scenes live in «До / После»; without one, here. A plan has no scenes.
                        .when(!self.plan_mode() && graph.is_none(), |this| this.child(media::render(&self.input, window, cx))),
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
    fn render_header(&self, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        if let Some(plan) = &self.plan {
            return self.render_plan_header(plan, p, theme, cx).into_any_element();
        }
        let run = self.selected.and_then(|i| self.runs.get(i));
        let r = self.report();
        let provider = self.input.provider;
        let next = reviewer_name(provider, crate::settings::report_model(provider, cx).as_deref());
        // Who wrote the iteration on screen; the picker says who writes the next one.
        let eyebrow = match run {
            Some(run) if r.is_some() => {
                let by = Some(run.meta.reviewer.clone()).filter(|s| !s.is_empty() && *s != next).map(|s| format!(" · {s}")).unwrap_or_default();
                format!("Карта изменений · итерация {} · {}{by}", run.n, run.meta.at.map(ago).unwrap_or_default())
            }
            _ => "Карта изменений".to_owned(),
        };
        let out = self.out.clone();
        let can_batch = self.running.is_none() && self.runs.iter().any(|r| r.ok().is_some()) && self.news;
        v_flex()
            .gap(px(8.))
            .child(
                h_flex()
                    .gap(px(8.))
                    .child(div().flex_1().min_w_0().child(ui::eyebrow(eyebrow, p, theme).truncate()))
                    .child(model_picker(provider, next, self.running.is_some()))
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
            .into_any_element()
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
            .child(ui::section_title("Итерации", Some(self.runs.len().to_string()), p, theme).child(
                ui::mono(if self.plan_mode() { "каждое «Отправить агенту» — новая итерация плана" } else { "каждый «Пересобрать» и каждый батч — отдельный снимок карты" }, theme).text_color(p.faint),
            ))
            .child(
                div().id("iterations-scroll").overflow_x_scroll().child(
                    h_flex().gap(px(6.)).pb(px(2.)).children(self.runs.iter().enumerate().map(|(i, run)| {
                        let active = self.selected == Some(i);
                        let is_running = running_n == Some(run.n);
                        let failed = run.ok().is_none() && !is_running;
                        let approved = self.plan.as_ref().and_then(|pl| pl.meta.as_ref()).and_then(|t| t.meta.plan_approved) == Some(run.n);
                        let kind = match run.meta.kind {
                            _ if self.plan_mode() => "план".to_owned(),
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
                            .when(approved, |d| d.child(ui::badge("утверждён", p.green)))
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

    fn render_report(&self, g: &Graph, canvas_h: f32, p: &Palette, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(r) = self.report() else { return div().into_any_element() };
        if self.plan_mode() {
            return self.render_plan_report(g, r, canvas_h, p, theme, cx);
        }
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
            // Where the session takes the project, what it did, then the looks at the change.
            .child(self.render_goal(r, &dir, p, theme))
            // How the session covers its task's approved plan.
            .when_some(self.fact_plan.as_ref(), |this, plan| this.child(self.render_coverage(plan, g, p, theme, cx)))
            .child(self.render_lens_tabs(r, p, theme, cx))
            .child(match self.lens {
                Lens::Arch => v_flex()
                    .gap(px(22.))
                    .child(self.render_arch(r, p, theme, cx))
                    // The reviewer's diagrams explain the same structure in pictures.
                    .when_some(r.flow.clone(), |this, d| this.child(self.render_diagram("flow".into(), &d, &dir, p, theme)))
                    .when_some(r.structure.clone(), |this, d| this.child(self.render_diagram("structure".into(), &d, &dir, p, theme)))
                    .children(r.diagrams.iter().enumerate().map(|(i, d)| self.render_diagram(format!("extra-{i}"), d, &dir, p, theme)))
                    .into_any_element(),
                Lens::Contracts => self.render_contracts(r, p, theme),
                Lens::Media => v_flex()
                    .gap(px(22.))
                    .children(r.ui.iter().enumerate().map(|(i, u)| self.render_ui_pair(i, u, &dir, p)))
                    .child(media::render(&self.input, window, cx))
                    .into_any_element(),
                Lens::Map => self.render_map_section(g, r, &fresh, canvas_h, p, theme, cx).into_any_element(),
                Lens::Views => self.render_views(g, r, &dir, p, theme, cx),
            })
            .child(self.render_risks(g, r, p, theme, cx))
            .when_some(self.render_decisions(g, p, theme, cx), |this, d| this.child(d))
            // Which user flows the session went through: below the structure, not in its way.
            .child(self.render_flows(g, r, p, theme, cx))
            .into_any_element()
    }

    /// «К чему идём» and what the session did, above the tabs.
    fn render_goal(&self, r: &Report, dir: &Path, p: &Palette, theme: &Theme) -> impl IntoElement {
        v_flex()
            .gap(px(8.))
            .max_w(px(860.))
            .when(!r.goal.is_empty(), |this| {
                this.child(
                    h_flex()
                        .gap(px(10.))
                        .items_baseline()
                        .child(ui::eyebrow("к чему идём", p, theme).flex_shrink_0())
                        .child(div().text_size(px(15.)).line_height(px(22.)).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).child(r.goal.clone())),
                )
            })
            .when(!r.summary.is_empty(), |this| {
                this.child(
                    div()
                        .text_size(px(14.))
                        .line_height(px(22.))
                        .text_color(p.fg)
                        .child(TextView::markdown(SharedString::from(format!("changes-summary-{}", dir.display())), r.summary.clone()).selectable(true)),
                )
            })
    }

    /// «Затронутые флоу»: user flows of the project the session went through,
    /// touched steps lit, how well each is proven; and the reviewer's proposal
    /// for the flow map with «Принять» / «Отклонить».
    fn render_flows(&self, g: &Graph, r: &Report, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let touched = self.touched(g, r);
        let proposal = &r.flows.proposal;
        let mono = theme.mono_font_family.clone();
        if self.flow_map.is_none() && proposal.is_empty() {
            return h_flex()
                .gap(px(8.))
                .px(px(12.))
                .py(px(7.))
                .rounded(px(8.))
                .border_1()
                .border_dashed()
                .border_color(p.border_strong)
                .text_size(px(12.5))
                .text_color(p.muted)
                .child(Icon::empty().path("icons/route.svg").small().text_color(p.faint))
                .child("Карты флоу нет — построить на экране «Флоу»: тогда здесь будет видно, какие пути пользователя задела сессия")
                .into_any_element();
        }
        // Session proof: tests of this report and before/after scenes of this session.
        let scenes: usize = media::load_all(&self.input.key).iter().filter_map(|m| m.manifest.as_ref()?.as_ref().ok()).map(|m| m.items.len()).sum();
        let lit_step = self.flow_focus.clone();
        let cards = touched.iter().enumerate().filter_map(|(ti, t)| {
            let f = self.flow_feature(r, &t.feature)?;
            let path = f.paths.iter().find(|x| x.id == t.path).or(f.paths.first())?;
            let freshness = f.freshness(&self.input.cwd);
            // Entities of the touched steps, for the session's own tests.
            let step_entities: Vec<String> = path.steps.iter().filter(|s| t.steps.contains(&s.id)).flat_map(|s| s.entities.iter().map(|e| map::normalize(e))).collect();
            let session_tests = r.tests.iter().filter(|x| x.verifies.iter().any(|v| step_entities.contains(&map::normalize(v)))).count();
            let (fl, ft) = freshness_badge(freshness, p);
            let fid = f.id.clone();
            Some(
                v_flex()
                    .id(SharedString::from(format!("flow-{ti}")))
                    .flex_shrink_0()
                    .min_w(px(320.))
                    .max_w(px(640.))
                    .p(px(12.))
                    .gap(px(9.))
                    .rounded(px(10.))
                    .border_1()
                    .border_color(ui::mix(p.accent.solid, p.border, 0.35))
                    .bg(p.surface)
                    .shadow(ui::card_shadow())
                    .child(
                        h_flex()
                            .gap(px(8.))
                            .child(div().min_w_0().truncate().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).child(f.title.clone()))
                            .child(ui::badge(fl, ft))
                            .child(div().flex_1())
                            .child(ui::mono(path.title.clone(), theme).truncate().text_color(p.faint)),
                    )
                    .child(
                        h_flex().flex_wrap().gap(px(6.)).items_center().children(path.steps.iter().enumerate().flat_map(|(si, step)| {
                            let hit = t.steps.contains(&step.id);
                            let focused = lit_step.as_ref().is_some_and(|(ff, ss)| *ff == fid && *ss == step.id);
                            let (ff, ss) = (fid.clone(), step.id.clone());
                            let chip = v_flex()
                                .id(SharedString::from(format!("flow-{ti}-step-{si}")))
                                .max_w(px(200.))
                                .px(px(9.))
                                .py(px(5.))
                                .rounded(px(7.))
                                .border_1()
                                .cursor_pointer()
                                .map(|d| match (hit, focused) {
                                    (_, true) => d.border_color(p.ink).bg(if hit { p.amber.soft } else { p.surface2 }),
                                    (true, false) => d.border_color(ui::mix(p.amber.solid, p.border, 0.55)).bg(p.amber.soft),
                                    (false, false) => d.border_color(p.border).bg(p.surface2).opacity(0.75),
                                })
                                .hover(|d| d.border_color(p.border_strong))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let key = (ff.clone(), ss.clone());
                                    this.flow_focus = if this.flow_focus.as_ref() == Some(&key) { None } else { Some(key) };
                                    cx.notify();
                                }))
                                .child(div().truncate().font_family(mono.clone()).text_size(px(10.)).text_color(if hit { p.amber.text } else { p.faint }).child(step.screen.clone()))
                                .child(div().line_clamp(2).text_size(px(12.)).line_height(px(16.)).text_color(if hit { p.ink } else { p.muted }).child(step.action.clone()));
                            let arrow = (si > 0).then(|| div().text_size(px(12.)).text_color(p.faint).child("→").into_any_element());
                            arrow.into_iter().chain(std::iter::once(chip.into_any_element()))
                        })),
                    )
                    .child(
                        h_flex()
                            .gap(px(10.))
                            .text_size(px(11.5))
                            .text_color(p.muted)
                            .child(format!("задето шагов: {} из {}", t.steps.len(), path.steps.len()))
                            .when(session_tests > 0 || scenes > 0, |d| {
                                let mut parts = Vec::new();
                                if session_tests > 0 {
                                    parts.push(format!("тестов: {session_tests}"));
                                }
                                if scenes > 0 {
                                    parts.push(format!("кадров было → стало: {scenes}"));
                                }
                                d.child(div().text_color(p.green.text).child(format!("доказательства сессии · {}", parts.join(" · "))))
                            })
                            .when(session_tests == 0 && scenes == 0 && freshness != Freshness::Fresh, |d| {
                                d.child(div().text_color(p.amber.text).child("сессия это ничем не проверила"))
                            }),
                    ),
            )
        });
        let cards: Vec<_> = cards.collect();
        let decided = self.proposal_id().map(|id| {
            let d = flowmap::Decisions::load(&stepan_verify_core::Home::locate(), &self.input.cwd);
            (d.accepted.contains(&id), d.rejected.contains(&id))
        });
        let notes: Vec<String> = if proposal.notes.is_empty() {
            proposal
                .upsert
                .iter()
                .map(|f| {
                    let known = self.flow_map.as_ref().is_some_and(|m| m.feature(&f.id).is_some());
                    format!("{} {}", if known { "~" } else { "+" }, f.title)
                })
                .chain(proposal.remove.iter().map(|id| format!("− {id}")))
                .collect()
        } else {
            proposal.notes.clone()
        };
        v_flex()
            .gap(px(10.))
            .child(
                ui::section_title("Затронутые флоу", Some(cards.len().to_string()), p, theme)
                    .child(ui::mono("клик по шагу — подсветит его сущности на канвасе", theme).text_color(p.faint)),
            )
            .map(|this| {
                if cards.is_empty() {
                    this.child(div().text_size(px(12.5)).text_color(p.muted).child(if self.flow_map.is_some() {
                        "Сессия не задела флоу из карты проекта"
                    } else {
                        "Карты флоу пока нет — ниже то, что ревьюер предлагает в неё записать"
                    }))
                } else {
                    this.child(div().id("flows-scroll").overflow_x_scroll().child(h_flex().gap(px(12.)).items_start().pb(px(4.)).children(cards)))
                }
            })
            .when(proposal.is_empty() && !proposal.notes.is_empty(), |this| {
                this.child(
                    v_flex()
                        .gap(px(4.))
                        .px(px(12.))
                        .py(px(8.))
                        .rounded(px(8.))
                        .border_1()
                        .border_dashed()
                        .border_color(p.border_strong)
                        .child(ui::eyebrow("Ревьюер о карте флоу · правки целиком не прислал, применять нечего", p, theme))
                        .children(proposal.notes.iter().take(8).map(|n| div().text_size(px(12.5)).text_color(p.fg).child(n.clone()))),
                )
            })
            .when(!proposal.is_empty(), |this| {
                let (accepted, rejected) = decided.unwrap_or((false, false));
                this.child(
                    v_flex()
                        .gap(px(8.))
                        .p(px(12.))
                        .rounded(px(10.))
                        .border_1()
                        .border_color(if accepted { ui::mix(p.green.solid, p.border, 0.4) } else { ui::mix(p.blue.solid, p.border, 0.4) })
                        .bg(if accepted { p.green.soft } else { ui::mix(p.blue.soft, p.surface, 0.5) })
                        .child(
                            h_flex()
                                .gap(px(8.))
                                .child(Icon::empty().path("icons/route.svg").small().text_color(p.blue.solid))
                                .child(div().flex_1().text_size(px(13.5)).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).child("Предложения к карте флоу"))
                                .map(|d| {
                                    if accepted {
                                        d.child(ui::badge("принято в карту", p.green))
                                    } else if rejected {
                                        d.child(ui::badge("отклонено", Tone { solid: p.faint, soft: p.surface2, text: p.muted }))
                                    } else {
                                        d.child(
                                            PressButton::new("flows-reject")
                                                .small()
                                                .label("Отклонить")
                                                .on_click(cx.listener(|this, _, _, cx| this.decide_proposal(false, cx))),
                                        )
                                        .child(
                                            PressButton::new("flows-accept")
                                                .small()
                                                .primary()
                                                .icon(IconName::Check)
                                                .label("Принять в карту")
                                                .tooltip(format!("Записать в {}", flowmap::DIR))
                                                .on_click(cx.listener(|this, _, _, cx| this.decide_proposal(true, cx))),
                                        )
                                    }
                                }),
                        )
                        .children(notes.into_iter().take(12).map(|n| div().text_size(px(12.5)).line_height(px(18.)).text_color(p.fg).child(n))),
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
        let plan = self.plan_mode();
        let reviewable = self.review.len();
        let seen = self.review.values().filter(|(s, _)| *s == Review::Seen).count();
        let changed_since = self.review.values().filter(|(s, _)| *s == Review::ChangedSince).count();
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
                            .child(mode_btn("map-before", if plan { "Сейчас" } else { "Было" }, MapMode::Before, cx))
                            .child(mode_btn("map-after", if plan { "Станет" } else { "Стало" }, MapMode::After, cx))
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
                    .when(plan, |d| d.child(ui::mono("станет:", theme).text_color(p.faint)))
                    .child(legend(format!("{} · {}", status_label(NodeStatus::Added, plan), count(NodeStatus::Added)), Some(p.green), false))
                    .child(legend(format!("{} · {}", status_label(NodeStatus::Changed, plan), count(NodeStatus::Changed)), Some(p.amber), false))
                    .child(legend(format!("{} · {}", status_label(NodeStatus::Removed, plan), count(NodeStatus::Removed)), Some(p.red), false))
                    .when(removed_links > 0, |d| d.child(legend(format!("{} · {removed_links}", if plan { "уберётся связь" } else { "убрана связь" }), Some(p.red), true)))
                    .child(legend(status_label(NodeStatus::Context, plan).into(), None, true))
                    .when(!fresh.is_empty(), |d| {
                        d.child(h_flex().gap(px(5.)).text_size(px(12.)).text_color(p.blue.text).child(div().size(px(8.)).rounded_full().bg(p.blue.solid)).child(format!("новое в итерации · {}", fresh.len())))
                    })
                    .when(reviewable > 0 && !self.show_list, |d| {
                        d.child(div().flex_1()).child(
                            h_flex()
                                .flex_shrink_0()
                                .gap(px(12.))
                                .when(changed_since > 0, |d| {
                                d.child(
                                    h_flex()
                                        .gap(px(5.))
                                        .text_size(px(12.))
                                        .text_color(p.accent.text)
                                        .child(Icon::new(IconName::RefreshCw).xsmall().text_color(p.accent.solid))
                                        .child(format!("изменилось после просмотра · {changed_since}")),
                                )
                            })
                            .child(
                                h_flex()
                                    .gap(px(5.))
                                    .text_size(px(12.))
                                    .text_color(p.muted)
                                    .child(Icon::new(IconName::CircleCheck).xsmall().text_color(if seen > 0 { p.green.solid } else { p.faint }))
                                    .child(format!("просмотрено {seen} из {reviewable}")),
                            )
                            .child(
                                h_flex()
                                    .gap(px(6.))
                                    .child(Switch::new("map-only-new").checked(self.only_new).on_click(cx.listener(|this, on: &bool, _, cx| {
                                        this.only_new = *on;
                                        if *on {
                                            stepan_verify_core::usage::hit("changes.only_new");
                                        }
                                        cx.notify();
                                    })))
                                    .child(div().text_size(px(12.5)).font_weight(FontWeight::MEDIUM).text_color(if self.only_new { p.ink } else { p.muted }).child("Только новое")),
                            ),
                        )
                    }),
            )
            .map(|this| {
                if self.show_list || !has_nodes {
                    this.child(render_columns(&r.code, p, theme))
                } else if self.only_new && seen == reviewable && reviewable > 0 {
                    this.child(
                        v_flex()
                            .h(px(160.))
                            .items_center()
                            .justify_center()
                            .gap(px(10.))
                            .rounded(px(10.))
                            .border_1()
                            .border_dashed()
                            .border_color(p.border_strong)
                            .bg(p.sunken)
                            .child(h_flex().gap(px(6.)).text_size(px(13.5)).text_color(p.fg).child(Icon::new(IconName::CircleCheck).small().text_color(p.green.solid)).child("Всё на карте просмотрено"))
                            .child(div().text_size(px(12.5)).text_color(p.muted).child("Новое и изменённое после просмотра появится здесь со следующим прогоном"))
                            .child(PressButton::new("map-show-all").xsmall().outline().label("Показать всю карту").on_click(cx.listener(|this, _, _, cx| {
                                this.only_new = false;
                                cx.notify();
                            }))),
                    )
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
        let cv = map::canvas_layout(g, mode, self.layout_target());
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
        // A step of a touched flow you clicked: its entities and files.
        let flow_lit: Option<HashSet<usize>> = self.flow_focus.as_ref().and_then(|(fid, sid)| {
            let f = self.flow_feature(r, fid)?;
            let step = f.paths.iter().flat_map(|x| &x.steps).find(|s| s.id == *sid)?;
            let names: Vec<String> = step.entities.iter().map(|e| map::normalize(e)).collect();
            let hits: HashSet<usize> = g
                .nodes
                .iter()
                .enumerate()
                .filter(|(_, n)| {
                    names.contains(&map::normalize(&n.name))
                        || names.contains(&map::normalize(&n.entity))
                        || names.contains(&n.key)
                        || (!n.file.is_empty() && step.files.iter().any(|f| f.ends_with(&n.file) || n.file.ends_with(f.as_str())))
                })
                .map(|(i, _)| i)
                .collect();
            Some(hits)
        });
        // A stage of the plan you clicked: its nodes.
        let stage_lit: Option<HashSet<usize>> = self.plan.as_ref().and_then(|pl| pl.stage).and_then(|si| r.stages.get(si)).map(|s| plan::stage_nodes(g, &s.nodes));
        let lit: Option<HashSet<usize>> = match (dto_focus, focus_ix, group_focus) {
            (None, None, _) if stage_lit.is_some() || flow_lit.is_some() => stage_lit.clone().or(flow_lit.clone()),
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
        // Risks another agent refuted by running something do not count on the card.
        let checked = self.selected.and_then(|i| self.runs.get(i)).and_then(|run| self.verify.get(&run.n));
        let refuted = |i: usize, risk: &Risk| checked.and_then(|c| c.get(&risk_key(i, risk))).is_some_and(|v| v.verdict == "refuted");
        let risk_count = |n: &map::MapNode| {
            r.risks
                .iter()
                .enumerate()
                .filter(|(i, risk)| !refuted(*i, risk))
                .filter(|(_, risk)| risk.nodes.iter().any(|k| map::normalize(k) == n.key) || (!risk.id.is_empty() && n.risk_ids.contains(&risk.id)))
                .count()
        };
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
            // DTO labels: at a readable zoom, or on the link you point at.
            if let Some(d) = e.via.filter(|&d| g.dtos[d].visible(mode) && text && (z >= 0.95 || hot)) {
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
                let (w, h) = (units(bounds.size.width), units(bounds.size.height));
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
                                    )
                                    // A plan proves nothing yet.
                                    .when(gr.kind == map::GroupKind::Feature && !self.plan_mode(), |d| {
                                        let (label, tone) = evidence_badge(group_rank(r, g, gi), p);
                                        d.child(
                                            div()
                                                .flex_shrink_0()
                                                .px(px(zl(5.)))
                                                .rounded(px(zl(4.)))
                                                .bg(tone.soft)
                                                .text_size(px(zl(10.)))
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_color(tone.text)
                                                .child(label),
                                        )
                                    }),
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
            let rev = self.review.get(&n.key).map(|(s, _)| *s);
            let review_key = n.key.clone();
            // At rest: role and the human name. Pointed at: grows over its
            // neighbours with the code name, signature and file.
            let expanded = text && self.hovered.as_deref() == Some(n.key.as_str()) && !self.drag.is_some_and(|d| d.moved);
            let h = if expanded { map::node_height(n, mode).max(h) } else { h };
            let el = div()
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
                // Reviewed and unchanged since: steps back until you point at it.
                .when(!dim && rev == Some(Review::Seen) && !is_sel && !expanded, |d| d.opacity(0.5))
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
                                d.child(div().flex_shrink_0().px(px(zl(5.))).rounded(px(zl(4.))).bg(t.soft).text_size(px(zl(10.))).font_weight(FontWeight::SEMIBOLD).text_color(t.text).child(status_label(n.status, self.plan_mode())))
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
                            })
                            // «Просмотрено»: a mark on reviewed cards, a toggle on the one you point at.
                            .when_some(rev.filter(|r| *r != Review::New || expanded), |d, r| {
                                let (icon, color, tip) = match r {
                                    Review::Seen => (IconName::CircleCheck, p.green.solid, "Просмотрено · снять отметку"),
                                    Review::ChangedSince => (IconName::RefreshCw, p.accent.solid, "Изменилось после просмотра · отметить просмотренным"),
                                    Review::New => (IconName::Check, p.faint, "Отметить просмотренным"),
                                };
                                d.child(
                                    div()
                                        .id(SharedString::from(format!("node-review-{review_key}")))
                                        .flex_shrink_0()
                                        .size(px(zl(15.)))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded(px(zl(4.)))
                                        .hover(|s| s.bg(p.surface2))
                                        .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tip).build(window, cx))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            if this.dragged {
                                                return;
                                            }
                                            cx.stop_propagation();
                                            this.toggle_reviewed(&review_key, cx);
                                        }))
                                        .child(Icon::new(icon).with_size(px(zl(12.))).text_color(color)),
                                )
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
                    .when(expanded && map::shows_code_name(n), |d| {
                        d.child(div().mt(px(zl(-2.))).truncate().font_family(mono.clone()).text_size(px(zl(11.))).line_height(px(zl(map::NAME_LINE))).text_color(p.muted).child(n.name.clone()))
                    })
                    .when(expanded && !sig.is_empty() && detail, |d| {
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
                    .when(expanded, |d| {
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
                .when(expanded, |d| d.shadow(ui::popover_shadow()).border_color(p.border_strong));
            (expanded, el.into_any_element())
        });
        // The card you point at draws last: it grows over its neighbours.
        let mut nodes: Vec<(bool, AnyElement)> = nodes.collect();
        nodes.sort_by_key(|(expanded, _)| *expanded);
        let nodes = nodes.into_iter().map(|(_, el)| el);

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
                    .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new("100% — к главному функционалу").build(window, cx))
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(g) = this.report().map(map::build_report) {
                            this.readable(&g, cx);
                        }
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
                d.child(ui::badge(if self.plan_mode() { "в плане нет тестов" } else { "тестов нет — изменения ничем не проверены" }, p.amber).text_size(px(10.5)))
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
                this.view = drag.update((units(e.position.x), units(e.position.y)));
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
                    ScrollDelta::Pixels(d) => (units(d.x), units(d.y)),
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
        // A plan iteration is whole: only its own decisions.
        let from = if self.plan_mode() { upto } else { 0 };
        let groups: Vec<(&Run, &Report)> = self.runs[from..=upto].iter().filter_map(|r| r.ok().map(|rep| (r, rep))).filter(|(_, rep)| !rep.decisions.is_empty()).collect();
        if groups.is_empty() {
            return None;
        }
        let total: usize = groups.iter().map(|(_, r)| r.decisions.len()).sum();
        let mono = theme.mono_font_family.clone();
        Some(
            v_flex()
                .gap(px(12.))
                .child(ui::section_title("Решения агента", Some(total.to_string()), p, theme).child(
                    ui::mono(if self.plan_mode() { "что агент выбрал для плана и почему" } else { "как и почему менялась карта, по итерациям" }, theme).text_color(p.faint),
                ))
                .child(ui::card(p).overflow_hidden().children(groups.into_iter().enumerate().map(|(gi, (run, rep))| {
                    let kind = if self.plan_mode() { "план" } else if run.meta.kind == RunKind::Full { "полный" } else { "батч" };
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

    /// Risks with their proof level; each can be handed to an agent of the
    /// other family to prove or refute by running something. Refuted ones
    /// move to «Отброшено».
    fn render_risks(&self, g: &Graph, r: &Report, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let run_n = self.selected.and_then(|i| self.runs.get(i)).map(|run| run.n);
        let results = run_n.and_then(|n| self.verify.get(&n)).cloned().unwrap_or_default();
        let other = match other_family(self.input.provider) {
            Provider::Claude => "Claude",
            Provider::Codex => "Codex",
        };
        let mono = theme.mono_font_family.clone();
        let keyed: Vec<(usize, String, &Risk)> = r.risks.iter().enumerate().map(|(i, risk)| (i, risk_key(i, risk), risk)).collect();
        let refuted = |k: &str| results.get(k).is_some_and(|v| v.verdict == "refuted");
        let open: Vec<&(usize, String, &Risk)> = keyed.iter().filter(|(_, k, _)| !refuted(k)).collect();
        let dropped: Vec<&(usize, String, &Risk)> = keyed.iter().filter(|(_, k, _)| refuted(k)).collect();
        let unchecked: Vec<String> = open.iter().filter(|(_, k, _)| !results.contains_key(k)).map(|(_, k, _)| k.clone()).collect();
        let busy = self.verifying.as_ref().map(|j| (j.run, j.risk.clone(), j.started.elapsed().as_secs(), j.steps.last().cloned(), j.by.clone()));
        let queued: HashSet<String> = self.verify_queue.iter().filter(|(n, _)| Some(*n) == run_n).map(|(_, k)| k.clone()).collect();
        let row = |(i, key, risk): &(usize, String, &Risk), cx: &mut Context<Self>| {
            let node = risk.nodes.first().map(|k| map::normalize(k)).or_else(|| g.nodes.iter().find(|n| !risk.id.is_empty() && n.risk_ids.contains(&risk.id)).map(|n| n.key.clone()));
            let result = results.get(key).cloned();
            let running = busy.as_ref().filter(|b| Some(b.0) == run_n && b.1 == *key).cloned();
            let waiting = queued.contains(key);
            let rank = result.as_ref().map_or(risk.evidence.rank(), |v| evidence_rank(&v.level).max(risk.evidence.rank()));
            let (elabel, etone) = evidence_badge(rank, p);
            let k = key.clone();
            v_flex()
                .px(px(14.))
                .py(px(10.))
                .gap(px(6.))
                .when(*i > 0, |d| d.border_t_1().border_color(p.border))
                .child(
                    h_flex()
                        .gap(px(10.))
                        .items_start()
                        .child(match severity_badge(&risk.severity, p) {
                            Some((label, tone)) => ui::badge(label, tone).font_family(mono.clone()).text_size(px(10.)).into_any_element(),
                            None => div().mt(px(1.)).child(Icon::new(IconName::TriangleAlert).small().text_color(p.amber.solid)).into_any_element(),
                        })
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap(px(4.))
                                .child(div().text_size(px(13.5)).line_height(px(19.)).text_color(p.fg).child(risk.title.clone()))
                                .when(!risk.detail.is_empty(), |v| v.child(div().text_size(px(12.5)).text_color(p.muted).child(risk.detail.clone())))
                                .child(
                                    h_flex()
                                        .gap(px(6.))
                                        .flex_wrap()
                                        .child(ui::badge(elabel, etone).text_size(px(10.5)))
                                        .when(!risk.evidence.reference.is_empty() && result.is_none(), |d| {
                                            d.child(div().max_w(px(260.)).truncate().font_family(mono.clone()).text_size(px(10.5)).text_color(p.faint).child(risk.evidence.reference.clone()))
                                        })
                                        .when_some(result.clone(), |d, v| {
                                            let (label, tone) = match v.verdict.as_str() {
                                                "confirmed" => ("подтверждён запуском", p.red),
                                                "inconclusive" => ("не удалось доказать", p.amber),
                                                _ => ("проверка не прошла", Tone { solid: p.faint, soft: p.surface2, text: p.muted }),
                                            };
                                            d.child(ui::badge(label, tone).text_size(px(10.5))).child(ui::mono(v.by.clone(), theme).text_size(px(10.5)).text_color(p.faint))
                                        })
                                        .when_some(node, |v, nk| v.child(self.node_chip(SharedString::from(format!("risk-{i}-node")), &nk, g, p, theme, cx))),
                                ),
                        )
                        .map(|d| match (&running, waiting, &result) {
                            (Some((_, _, secs, step, by)), _, _) => d.child(
                                v_flex()
                                    .flex_shrink_0()
                                    .items_end()
                                    .gap(px(2.))
                                    .child(h_flex().gap(px(6.)).child(Spinner::new().xsmall().color(p.blue.solid)).child(div().text_size(px(11.5)).text_color(p.blue.text).child(format!("{} проверяет · {}:{:02}", by.split(" · ").next().unwrap_or(by), secs / 60, secs % 60))))
                                    .when_some(step.clone(), |v, st| v.child(div().max_w(px(220.)).truncate().font_family(mono.clone()).text_size(px(10.5)).text_color(p.faint).child(st))),
                            ),
                            (None, true, _) => d.child(div().flex_shrink_0().font_family(mono.clone()).text_size(px(11.)).text_color(p.faint).child("в очереди")),
                            (None, false, _) => d.child(
                                PressButton::new(SharedString::from(format!("verify-{key}")))
                                    .xsmall()
                                    .icon(Icon::empty().path("icons/flask-conical.svg"))
                                    .label(if result.is_some() { format!("Ещё раз · {other}") } else { format!("Проверить · {other}") })
                                    .tooltip(format!("Агент {other} попробует подтвердить или опровергнуть риск запуском"))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if let Some(n) = run_n {
                                            this.verify_risk(n, k.clone(), cx);
                                        }
                                    })),
                            ),
                        }),
                )
                .when_some(result.filter(|v| !v.reason.is_empty() || !v.command.is_empty()), |d, v| {
                    d.child(
                        v_flex()
                            .ml(px(36.))
                            .gap(px(4.))
                            .p(px(8.))
                            .rounded(px(7.))
                            .bg(p.surface2)
                            .when(!v.reason.is_empty(), |x| x.child(div().text_size(px(12.5)).line_height(px(18.)).text_color(p.fg).child(v.reason.clone())))
                            .when(!v.command.is_empty(), |x| x.child(div().font_family(mono.clone()).text_size(px(11.)).text_color(p.ink).child(format!("$ {}", v.command))))
                            .when(!v.output.is_empty(), |x| {
                                let out: String = v.output.lines().take(8).collect::<Vec<_>>().join("\n");
                                x.child(div().font_family(mono.clone()).text_size(px(10.5)).line_height(px(14.)).text_color(p.muted).child(out))
                            }),
                    )
                })
        };
        let rows: Vec<AnyElement> = open.iter().map(|x| row(x, cx).into_any_element()).collect();
        let dropped_rows: Vec<AnyElement> = dropped
            .iter()
            .map(|(i, key, risk)| {
                let v = results.get(key).cloned().unwrap_or_default();
                v_flex()
                    .px(px(14.))
                    .py(px(8.))
                    .gap(px(3.))
                    .when(*i > 0, |d| d.border_t_1().border_color(p.border))
                    .child(div().text_size(px(13.)).text_color(p.muted).line_through().child(risk.title.clone()))
                    .child(div().text_size(px(12.)).text_color(p.fg).child(if v.reason.is_empty() { "Запуск показал, что проблемы нет".to_owned() } else { v.reason.clone() }))
                    .child(ui::mono(format!("{} · {}", v.by, if v.command.is_empty() { String::new() } else { format!("$ {}", v.command) }), theme).truncate().text_size(px(10.5)).text_color(p.faint))
                    .into_any_element()
            })
            .collect();
        let can_all = !unchecked.is_empty() && self.verifying.as_ref().is_none_or(|j| Some(j.run) != run_n || !unchecked.contains(&j.risk)) && run_n.is_some();
        v_flex()
            .gap(px(10.))
            .child(
                ui::section_title("Риски на карте", Some(open.len().to_string()), p, theme)
                    .when(can_all, |d| {
                        let list = unchecked.clone();
                        d.child(
                            PressButton::new("verify-all")
                                .xsmall()
                                .icon(Icon::empty().path("icons/flask-conical.svg"))
                                .label(format!("Проверить все · {other}"))
                                .tooltip("По очереди, каждый риск — запуском, агентом другой семьи")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let Some(n) = run_n else { return };
                                    for k in &list {
                                        this.verify_risk(n, k.clone(), cx);
                                    }
                                })),
                        )
                    })
                    .when(self.verifying.is_some(), |d| {
                        d.child(PressButton::new("verify-stop").xsmall().ghost().label("Остановить").on_click(cx.listener(|this, _, _, cx| this.cancel_verify(cx))))
                    }),
            )
            .child(
                ui::card(p)
                    .overflow_hidden()
                    .when(open.is_empty(), |c| c.child(div().p(px(14.)).text_size(px(12.5)).text_color(p.faint).child(if dropped.is_empty() { "Ревьюер рисков не нашёл" } else { "Все риски опровергнуты проверкой" })))
                    .children(rows),
            )
            .when(!dropped_rows.is_empty(), |this| {
                this.child(
                    v_flex()
                        .gap(px(6.))
                        .child(ui::eyebrow(format!("Отброшено · {}", dropped_rows.len()), p, theme))
                        .child(ui::card(p).overflow_hidden().opacity(0.85).children(dropped_rows)),
                )
            })
    }

    /// «Как устроено»: the reviewer's show-me views — trees, pseudocode and code
    /// shapes drawn as text (diff lines tinted), and one HTML explainer that opens in the preview.
    fn render_views(&self, g: &Graph, r: &Report, dir: &Path, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let root = self.input.cwd.clone();
        let cards: Vec<AnyElement> = r
            .views
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let label = if v.diff { format!("{} · было → стало", v.kind_label()) } else { v.kind_label().to_owned() };
                let body = match (v.kind.as_str(), &v.file) {
                    ("html", Some(file)) => {
                        let path = resolve(dir, file);
                        let exists = path.exists();
                        let (open, root) = (path.clone(), root.clone());
                        h_flex()
                            .px(px(14.))
                            .py(px(12.))
                            .gap(px(10.))
                            .child(Icon::new(IconName::Globe).small().text_color(p.muted))
                            .child(div().flex_1().min_w_0().truncate().font_family(theme.mono_font_family.clone()).text_size(px(12.)).text_color(p.muted).child(
                                path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                            ))
                            .child(
                                PressButton::new(SharedString::from(format!("view-open-{i}")))
                                    .small()
                                    .label(if exists { "Открыть" } else { "Файла нет" })
                                    .disabled(!exists)
                                    .on_click(move |_, window, cx| crate::preview::open_in_app(open.clone(), Some(root.clone()), window, cx)),
                            )
                            .into_any_element()
                    }
                    _ => sketch(i, &v.body, v.diff, p, theme).into_any_element(),
                };
                // Wide sketches (long tree comments, code) take the whole row instead of clipping.
                let wide = v.kind == "code" || v.body.lines().map(|l| l.chars().count()).max().unwrap_or(0) > 52;
                ui::card(p)
                    .map(|c| if wide { c.w_full() } else { c.flex_1().min_w(px(380.)) })
                    .overflow_hidden()
                    .child(
                        h_flex()
                            .px(px(14.))
                            .pt(px(10.))
                            .pb(px(8.))
                            .gap(px(8.))
                            .border_b_1()
                            .border_color(p.border)
                            .child(div().flex_1().min_w_0().truncate().text_size(px(13.5)).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).child(if v.title.is_empty() {
                                label.clone()
                            } else {
                                v.title.clone()
                            }))
                            .child(ui::mono(label, theme).text_color(p.faint)),
                    )
                    .child(body)
                    .when(!v.caption.is_empty() || !v.nodes.is_empty(), |card| {
                        card.child(
                            v_flex()
                                .px(px(14.))
                                .py(px(8.))
                                .gap(px(6.))
                                .border_t_1()
                                .border_color(p.border)
                                .when(!v.caption.is_empty(), |c| c.child(div().text_size(px(12.5)).text_color(p.muted).child(v.caption.clone())))
                                .when(!v.nodes.is_empty(), |c| {
                                    c.child(h_flex().gap(px(5.)).flex_wrap().children(v.nodes.iter().enumerate().map(|(ni, k)| {
                                        self.node_chip(SharedString::from(format!("view-{i}-{ni}")), &map::normalize(k), g, p, theme, cx)
                                    })))
                                }),
                        )
                    })
                    .into_any_element()
            })
            .collect();
        if cards.is_empty() {
            return ui::card(p)
                .px(px(16.))
                .py(px(14.))
                .gap(px(4.))
                .child(div().text_size(px(13.5)).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).child("В этой итерации видов нет"))
                .child(div().text_size(px(12.5)).text_color(p.muted).child(if self.plan_mode() {
                    "Агент не показал, как будет устроено, — попроси в замечаниях дерево вызовов или макет."
                } else {
                    "Их пишет ревьюер с Stepan 0.2.6 — нажми «Пересобрать» или «Дополнить»."
                }))
                .into_any_element();
        }
        div().flex().flex_wrap().gap(px(12.)).items_start().children(cards).into_any_element()
    }

    /// «Карта | Как устроено»: the canvas and the show-me views are two looks at one report.
    fn render_lens_tabs(&self, r: &Report, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let tab = |key: &'static str, icon: IconName, label: &'static str, count: Option<usize>, lens: Lens, cx: &mut Context<Self>| {
            let active = self.lens == lens;
            h_flex()
                .id(key)
                .gap(px(7.))
                .px(px(14.))
                .h(px(32.))
                .rounded(px(7.))
                .cursor_pointer()
                .text_size(px(13.5))
                .font_weight(if active { FontWeight::SEMIBOLD } else { FontWeight::MEDIUM })
                .map(|d| if active { d.bg(p.surface).text_color(p.ink).shadow(ui::card_shadow()) } else { d.text_color(p.muted).hover(|s| s.text_color(p.ink)) })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.lens = lens;
                    stepan_verify_core::usage::hit(lens.usage_id());
                    cx.notify();
                }))
                .child(Icon::new(icon).small())
                .child(label)
                .when_some(count.filter(|n| *n > 0), |d, n| d.child(div().font_family(theme.mono_font_family.clone()).text_size(px(11.5)).text_color(p.faint).child(n.to_string())))
        };
        // The older looks stay reachable, quietly.
        let old = |key: &'static str, label: &'static str, lens: Lens, cx: &mut Context<Self>| {
            let active = self.lens == lens;
            div()
                .id(key)
                .cursor_pointer()
                .text_size(px(12.5))
                .text_color(if active { p.ink } else { p.faint })
                .when(active, |d| d.font_weight(FontWeight::SEMIBOLD))
                .hover(|s| s.text_color(p.ink))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.lens = lens;
                    stepan_verify_core::usage::hit(lens.usage_id());
                    cx.notify();
                }))
                .child(label)
        };
        let modules = self.arch_graph().map(|g| g.arch.modules.iter().filter(|m| m.status != stepan_verify_core::arch::Status::Same).count());
        let scenes: usize = r.ui.len() + media::load_all(&self.input.key).iter().filter_map(|m| m.manifest.as_ref()?.as_ref().ok()).map(|m| m.items.len()).sum::<usize>();
        let types = contracts::type_count(r);
        h_flex()
            .gap(px(12.))
            .child(
                h_flex()
                    .p(px(3.))
                    .gap(px(2.))
                    .rounded(px(9.))
                    .border_1()
                    .border_color(p.border)
                    .bg(p.sunken)
                    .map(|d| {
                        if self.plan_mode() {
                            // A plan has no diff yet: its looks are the canvas and «как будет устроено».
                            d.child(tab("lens-map", IconName::Network, "Карта", None, Lens::Map, cx))
                                .child(tab("lens-views", IconName::Eye, "Как устроено", Some(r.views.len()), Lens::Views, cx))
                        } else {
                            d.child(tab("lens-arch", IconName::Network, "Архитектура", modules, Lens::Arch, cx))
                                .child(tab("lens-contracts", IconName::Frame, "Контракты", Some(types), Lens::Contracts, cx))
                                .child(tab("lens-media", IconName::Eye, "До / После", Some(scenes), Lens::Media, cx))
                        }
                    }),
            )
            .child(ui::mono(
                match (self.plan_mode(), self.lens) {
                    (true, Lens::Views) => "как будет устроено: деревья, данные, макет",
                    (true, _) => "канвас: к чему придёт код",
                    (false, Lens::Arch) => "модули и импорты: было → стало",
                    (false, Lens::Contracts) => "типы, сигнатуры, зависимости",
                    (false, Lens::Media) => "скриншоты, GIF и видео до и после",
                    (false, Lens::Map) => "канвас: что изменилось и как связано",
                    (false, Lens::Views) => "show-me: деревья, псевдокод, было → стало",
                },
                theme,
            )
            .text_color(p.faint))
            .child(div().flex_1())
            .when(!self.plan_mode(), |this| this.child(h_flex().gap(px(12.)).child(old("lens-map", "Карта", Lens::Map, cx)).child(old("lens-views", "Как устроено", Lens::Views, cx))))
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
                    .child(shot("Было", u.before.as_ref().map(|path| self.resolve_shot(dir, path))))
                    .child(Icon::new(IconName::ArrowRight).small().text_color(p.faint))
                    .child(shot("Стало", u.after.as_ref().map(|path| self.resolve_shot(dir, path)))),
            )
    }

    /// Sticky decision bar under the report.
    fn render_bar(&self, _g: &Graph, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        if let Some(plan) = &self.plan {
            return self.render_plan_bar(plan, p, theme, cx);
        }
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
            .into_any_element()
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
            Panel::Return => (
                if self.plan_mode() { "Замечания к плану".into() } else { "Вернуть с замечанием".into() },
                self.render_return_panel(g, p, theme, window, cx).into_any_element(),
            ),
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
                .when_some(status_tone(n.status, p).filter(|_| n.status != NodeStatus::Context), |d, t| d.child(ui::badge(status_label(n.status, self.plan_mode()), t)))
        };
        // «Что доказывает»: the reviewer's evidence, tests, linked checks, this session's before/after.
        let report = self.report();
        let feature = report.and_then(|r| r.features.iter().find(|f| !f.id.is_empty() && map::normalize(&f.id) == gr.key));
        let rank = report.map_or(0, |r| group_rank(r, g, gi));
        let linked_checks: Vec<Check> = report
            .map(|r| {
                r.check
                    .iter()
                    .filter(|c| !c.id.is_empty() && members.iter().any(|&i| g.nodes[i].check_ids.contains(&c.id)))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let scenes: usize = media::load_all(&self.input.key).iter().filter_map(|m| m.manifest.as_ref()?.as_ref().ok()).map(|m| m.items.len()).sum();
        let (elabel, etone) = evidence_badge(rank, p);
        let proof = v_flex()
            .gap(px(6.))
            .child(h_flex().gap(px(8.)).child(label("Что доказывает")).child(ui::badge(elabel, etone)))
            .when_some(feature.filter(|f| !f.evidence.reference.is_empty()), |d, f| {
                d.child(div().font_family(theme.mono_font_family.clone()).text_size(px(11.5)).text_color(p.fg).child(f.evidence.reference.clone()))
            })
            .children(linked_checks.iter().map(|c| {
                let (l, t) = evidence_badge(c.evidence.rank(), p);
                h_flex().gap(px(6.)).child(ui::badge(l, t).text_size(px(10.))).child(div().flex_1().min_w_0().truncate().text_size(px(12.5)).text_color(p.fg).child(c.title.clone()))
            }))
            .when(scenes > 0, |d| d.child(div().text_size(px(12.5)).text_color(p.green.text).child(format!("Было → стало этой сессии: {scenes} кадр(ов) — ниже, в «Было → Стало»"))))
            .when(rank == 0 && scenes == 0 && tests.is_empty(), |d| {
                d.child(
                    div()
                        .p(px(10.))
                        .rounded(px(8.))
                        .bg(p.red.soft)
                        .text_size(px(12.5))
                        .text_color(p.red.text)
                        .child("Нечем доказать: нет тестов, запусков и скриншотов. Попроси агента проверить это запуском или снять before/after."),
                )
            });
        v_flex()
            .gap(px(14.))
            .child(h_flex().gap(px(8.)).child(ui::chip(gr.kind.label(), p, theme)).child(ui::mono(format!("{} сущностей", members.len()), theme).text_color(p.faint)))
            .when(!gr.summary.is_empty(), |d| d.child(div().text_size(px(13.5)).line_height(px(20.)).text_color(p.fg).child(gr.summary.clone())))
            .when(gr.kind == map::GroupKind::Feature && !self.plan_mode(), |d| d.child(proof))
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
        let review = self.review.get(&n.key).map(|(s, _)| *s);
        let reviewed_on = self.reviewed.get(&n.key).map(|m| m.run);
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
                        Some(t) => ui::dot_badge(status_label(n.status, self.plan_mode()), t).into_any_element(),
                        None => ui::chip(status_label(n.status, self.plan_mode()), p, theme).into_any_element(),
                    })
                    .when(!n.kind.is_empty(), |h| h.child(div().text_size(px(12.)).text_color(p.muted).child(kind_label(&n.kind))))
                    .child(div().flex_1())
                    .when_some(review, |h, state| {
                        let key = n.key.clone();
                        h.child(
                            PressButton::new("node-reviewed")
                                .xsmall()
                                .icon(if state == Review::Seen { IconName::CircleCheck } else { IconName::Check })
                                .label(match state {
                                    Review::Seen => "просмотрено",
                                    Review::New => "отметить просмотренным",
                                    Review::ChangedSince => "просмотрено снова",
                                })
                                .tooltip(if state == Review::Seen { "Снять отметку «просмотрено»" } else { "Узел пригаснет на карте, пока не изменится" })
                                .on_click(cx.listener(move |this, _, _, cx| this.toggle_reviewed(&key, cx))),
                        )
                    })
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
            .when(review == Some(Review::ChangedSince), |v| {
                v.child(
                    h_flex()
                        .gap(px(8.))
                        .px(px(10.))
                        .py(px(7.))
                        .rounded(px(8.))
                        .bg(p.accent.soft)
                        .text_size(px(12.5))
                        .text_color(p.accent.text)
                        .child(Icon::new(IconName::RefreshCw).small().text_color(p.accent.solid))
                        .child(match reviewed_on {
                            Some(run) if run > 0 => format!("Изменилось после просмотра (просмотрено в итерации #{run})"),
                            _ => "Изменилось после просмотра".to_owned(),
                        }),
                )
            })
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
                        .child(ui::eyebrow(
                            match (changed, self.plan_mode()) {
                                (true, false) => "сигнатура · было → стало",
                                (true, true) => "сигнатура · сейчас → станет",
                                _ => "сигнатура",
                            },
                            p,
                            theme,
                        ))
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
            .when(!self.plan_mode(), |v| v.child(
                v_flex()
                    .gap(px(8.))
                    .child(ui::eyebrow("дифф", p, theme))
                    .when(hunks.is_empty(), |v| v.child(div().text_size(px(12.5)).text_color(p.faint).child(if n.status == NodeStatus::Context { "Этот узел сессия не меняла" } else { "Ханки этого узла не нашлись в диффе" })))
                    .children(hunks.into_iter().take(4).enumerate().map(|(hi, h)| render_hunk(hi, &h, p, theme))),
            ))
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
                        Some(t) => ui::dot_badge(status_label(dto.status, self.plan_mode()), t).into_any_element(),
                        None => ui::chip(status_label(dto.status, self.plan_mode()), p, theme).into_any_element(),
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
                            .when(!c.how.is_empty(), |v| v.child(div().text_size(px(12.)).text_color(p.muted).child(c.how.clone())))
                            .when(!c.evidence.level.is_empty(), |v| {
                                let (l, t) = evidence_badge(c.evidence.rank(), p);
                                v.child(h_flex().child(ui::badge(format!("агент: {l}"), t).text_size(px(10.))))
                            }),
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
            .child(div().text_size(px(12.5)).text_color(p.muted).child(if self.plan_mode() {
                "Узлы добавляются кнопкой «в замечания» в панели узла, ответы на вопросы пиши как «В1: …». Текст уйдёт агенту, который пишет план, — он перепишет его новой итерацией."
            } else {
                "Узлы добавляются кнопкой «в замечания» в панели узла. Текст уйдёт агенту в эту сессию."
            }))
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

/// A show-me text view: monospace lines, tinted as a diff when `diff` is set.
fn sketch(i: usize, body: &str, diff: bool, p: &Palette, theme: &Theme) -> impl IntoElement {
    let lines: Vec<&str> = body.trim_end().lines().take(80).collect();
    v_flex()
        .id(SharedString::from(format!("view-body-{i}")))
        .py(px(8.))
        .bg(p.surface2)
        .font_family(theme.mono_font_family.clone())
        .text_size(px(12.))
        .line_height(px(18.))
        .children(lines.into_iter().map(|l| {
            let (bg, fg) = match l.chars().next() {
                Some('+') if diff => (Some(ui::mix(p.green.soft, p.surface, 0.8)), p.green.text),
                Some('-') if diff => (Some(ui::mix(p.red.soft, p.surface, 0.8)), p.red.text),
                _ => (None, p.fg),
            };
            div()
                .px(px(14.))
                .when_some(bg, |d, bg| d.bg(bg))
                .text_color(fg)
                .whitespace_nowrap()
                .overflow_hidden()
                .child(if l.is_empty() { " ".to_owned() } else { l.to_owned() })
        }))
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
    fn touched_merges_by_feature_and_path() {
        let t = |f: &str, p: &str, steps: &[&str]| Touched { feature: f.into(), path: p.into(), steps: steps.iter().map(|s| s.to_string()).collect() };
        let reviewer = vec![t("joker", "daily", &["pick"]), t("", "x", &["y"])];
        let diff = vec![t("joker", "daily", &["open", "pick"]), t("auth", "login", &["submit"])];
        let merged = merge_touched(&reviewer, &diff);
        assert_eq!(merged, vec![t("joker", "daily", &["pick", "open"]), t("auth", "login", &["submit"])]);
    }

    #[core::prelude::v1::test]
    fn evidence_flows_and_verify_parse() {
        let r: Report = serde_json::from_str(
            r#"{"risks":[{"id":"r1","title":"двойной вызов","evidence":{"level":"ran","ref":"npm test -- fly"}},"старый риск строкой"],
                "check":[{"title":"проверь вход","evidence":{"level":"pointed"}},"строкой"],
                "features":[{"id":"f1","title":"Полёт","evidence":{"level":"reproduced","reference":"media/1/fly.gif"}}],
                "flows":{"touched":[{"feature":"rewards.joker","path":"daily","steps":["pick"]}],
                         "proposal":{"upsert":[{"id":"rewards.joker","title":"Выбор Джокера","paths":[{"id":"daily","steps":[{"id":"pick","screen":"Диалог","action":"Выбрать"}]}]}],
                                     "notes":["~ Выбор Джокера: новый шаг"]}}}"#,
        )
        .unwrap();
        assert_eq!((r.risks[0].evidence.rank(), r.risks[1].evidence.rank()), (3, 0));
        assert_eq!(r.risks[0].evidence.reference, "npm test -- fly");
        assert_eq!((r.check[0].evidence.rank(), r.check[1].evidence.rank()), (2, 0));
        assert_eq!(r.features[0].evidence.rank(), 4);
        assert_eq!(r.flows.touched[0].steps, vec!["pick".to_owned()]);
        assert!(!r.flows.proposal.is_empty());
        // Old reports: no flows, no evidence.
        let old: Report = serde_json::from_str(r#"{"risks":["x"]}"#).unwrap();
        assert!(old.flows.touched.is_empty() && old.flows.proposal.is_empty());
        // Risk keys: id, else position.
        assert_eq!((risk_key(0, &r.risks[0]), risk_key(1, &r.risks[1])), ("r1".to_owned(), "r2".to_owned()));
        let v: VerifyResult = serde_json::from_str(r#"{"verdict":"Refuted","level":"ran","command":"node -e 1","reason":"нет"}"#).unwrap();
        assert_eq!((evidence_rank(&v.level), v.command.as_str()), (3, "node -e 1"));
        assert_eq!(other_family(Provider::Claude), Provider::Codex);
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
        assert!(r.views.is_empty());
    }

    #[core::prelude::v1::test]
    fn show_me_views_parse() {
        let r: Report = serde_json::from_str(
            r#"{"views":[{"kind":"call_tree","title":"Отправка","diff":true,"body":" submit\n+  validate\n   send","nodes":["submit"]},
                {"kind":"html","title":"Состояния","file":"explainer.html"},{"kind":"something"}]}"#,
        )
        .unwrap();
        assert_eq!(r.views.len(), 3);
        assert!(r.views[0].diff && r.views[0].body.lines().nth(1) == Some("+  validate"));
        assert_eq!(r.views[0].kind_label(), "дерево вызовов");
        assert_eq!(r.views[1].file.as_deref(), Some("explainer.html"));
        assert_eq!(r.views[2].kind_label(), "код", "unknown kinds still render as text");
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
