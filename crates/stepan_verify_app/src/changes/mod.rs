//! The plan of a «Сначала план» task: the map of the target state a planning
//! agent writes (`~/.stepan_verify/state/plans/<task>/runs/<n>/`), drawn natively.
//! Every «Отправить агенту» is a new iteration, so the plan keeps its history.

pub(crate) mod canvas;
mod map;
pub mod plan;
mod runs;

use std::{
    cell::Cell,
    collections::HashSet,
    rc::Rc,
    path::{Path, PathBuf},
    time::Duration,
};

use gpui_kit::component::{
    ActiveTheme, Icon, IconName, Sizable as _, Theme,
    input::{Input, InputState},
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use crate::zoom::{px, units};
use stepan_verify_agents::Provider;
use stepan_verify_core::flows::{FlowProposal, Touched};
use serde::{Deserialize, Serialize};

use crate::ui::{self, Palette, PressButton, Tone};
use map::{EdgeKind, Graph, Mode as MapMode, NodeStatus};
use runs::Run;

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

/// Looks at a plan: the canvas of the target state and «как будет устроено».
#[derive(Clone, Copy, PartialEq, Eq)]
enum Lens {
    Map,
    Views,
}

impl Lens {
    fn usage_id(self) -> &'static str {
        match self {
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
    },
}

impl From<CheckRepr> for Check {
    fn from(r: CheckRepr) -> Self {
        match r {
            CheckRepr::Text(title) => Check { title, ..Default::default() },
            CheckRepr::Obj { id, title } => Check { id, title },
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
    /// «Замечания к плану»: send this text to the planning agent.
    Return(String),
    /// «Утвердить план»: this iteration.
    ApprovePlan(usize),
    /// Show the session that writes the plan.
    OpenSession,
}

/// The task whose plan is on screen.
struct PlanMode {
    task: String,
    /// The task as on disk, re-read while the view is open.
    meta: Option<stepan_verify_core::Task>,
    /// Iteration numbers seen on disk.
    seen: Vec<usize>,
    /// A stage you clicked: its nodes light up on the canvas.
    stage: Option<usize>,
}

#[derive(Clone, PartialEq)]
enum Panel {
    Node(String),
    Dto(String),
    Group(String),
    Return,
}

/// A task's plan: the map of the target state the planning agent wrote.
pub struct ChangesView {
    title: String,
    /// The task's work folder: file links open from here.
    cwd: PathBuf,
    out: PathBuf,
    runs: Vec<Run>,
    /// Index into `runs` of the iteration on screen.
    selected: Option<usize>,
    mode: MapMode,
    hovered: Option<String>,
    /// DTO under the pointer on the map: its users light up.
    hovered_dto: Option<String>,
    panel: Option<Panel>,
    /// Show the added / changed / removed columns instead of the map.
    show_list: bool,
    /// Which look at the plan is on screen.
    lens: Lens,
    /// Node keys put into the remark for the agent.
    remarks: Vec<String>,
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
    plan: PlanMode,
    _plan_watch: Task<()>,
}

impl EventEmitter<ChangesEvent> for ChangesView {}

impl ChangesView {

    /// The plan of task `task`: the map of the target state the planning
    /// agent wrote (`state/plans/<task>`), newest iteration first. Nothing
    /// runs from here; the view follows the folder as the agent writes.
    pub fn plan(task: String, title: String, cwd: PathBuf, cx: &mut Context<Self>) -> Self {
        let home = stepan_verify_core::Home::locate();
        let out = stepan_verify_core::plans::dir(&home, &task);
        let mut this = Self {
            title,
            cwd,
            out,
            runs: Vec::new(),
            selected: None,
            mode: MapMode::Diff,
            hovered: None,
            hovered_dto: None,
            panel: None,
            show_list: false,
            lens: Lens::Map,
            remarks: Vec::new(),
            view: canvas::View::default(),
            fit_key: None,
            drag: None,
            fit_size: (0., 0.),
            touched: false,
            dragged: false,
            viewport: Rc::new(Cell::new(None)),
            plan: PlanMode { task, meta: None, seen: Vec::new(), stage: None },
            _plan_watch: Task::ready(()),
        };
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
        let home = stepan_verify_core::Home::locate();
        let seen = stepan_verify_core::plans::runs(&home, &self.plan.task);
        let meta = stepan_verify_core::Board::new(home).get(&self.plan.task).ok();
        let fresh = seen != self.plan.seen;
        let status = |t: &Option<stepan_verify_core::Task>| t.as_ref().map(|t| (t.meta.status, t.meta.phase, t.meta.plan_approved));
        let moved = status(&meta) != status(&self.plan.meta);
        self.plan.seen = seen;
        self.plan.meta = meta;
        if fresh {
            // A new iteration is what you are waiting for: show it.
            self.reload(None);
        }
        if fresh || moved {
            cx.notify();
        }
    }

    /// Reread runs from disk; select `select` (a run number) or the newest good one.
    fn reload(&mut self, select: Option<usize>) {
        self.runs = runs::load_all(&self.out);
        let by_n = select.and_then(|n| self.runs.iter().position(|r| r.n == n));
        self.selected = by_n.or_else(|| self.runs.iter().rposition(|r| r.ok().is_some())).or(self.runs.len().checked_sub(1));
    }

    fn report(&self) -> Option<&Report> {
        self.selected.and_then(|i| self.runs.get(i)).and_then(Run::ok)
    }

    fn run_dir(&self) -> PathBuf {
        self.selected.and_then(|i| self.runs.get(i)).map(|r| r.dir.clone()).unwrap_or_else(|| self.out.clone())
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

    // -------------------------------------------------------------- verify

    fn remark_text(&self, graph: &Graph, note: &str) -> String {
        let title = self.report().map(|r| r.title.clone()).filter(|t| !t.is_empty()).unwrap_or_else(|| self.title.clone());
        let draft = stepan_verify_core::plans::draft(&stepan_verify_core::Home::locate(), &self.plan.task);
        let run = self.selected.and_then(|i| self.runs.get(i)).map(|r| format!("Смотрел итерацию #{}.\n", r.n)).unwrap_or_default();
        let mut text = format!("{}{run}", plan::remarks_header(&title, &draft));
        for key in &self.remarks {
            if let Some(n) = graph.index(key).map(|i| &graph.nodes[i]) {
                let file = if n.file.is_empty() { String::new() } else { format!(" ({})", n.file) };
                text.push_str(&format!("- {}{file}\n", n.name));
            }
        }
        if !note.trim().is_empty() {
            text.push_str(&format!("\n{}\n", note.trim()));
        }
        text
    }
}

/// Frames pack into rows as wide as the canvas at 100%, minus its margins.
fn layout_width(viewport_w: f32) -> f32 {
    (viewport_w - canvas::FIT_MARGIN * 2.).max(560.)
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

const VIEW_RULES: &str = "Как устроено (`views`, приём скилла show-me): 1–4 компактных вида того, КАК теперь работает сделанное — то, что человек иначе выяснял бы чтением кода. \
Бери самый маленький вид, который объясняет суть: поток выполнения — `call_tree` (дерево вызовов, отступ 2 пробела); UI — `component_tree` (компоненты с путями файлов в скобках, хуки и границы состояния, которые важны); \
раскладка файлов или рефакторинг — `file_tree` (неглубокое дерево `├──`/`└──`, у каждой строки `# зачем`); логика или алгоритм — `pseudocode`; новая форма целиком (интерфейс, тип, сигнатуры) — `code` с `lang`. \
Если форма уже была и изменилась — пиши в diff-синтаксисе, как `git diff` без заголовков: первая колонка `+` (добавлено), `-` (убрано) или пробел (контекст, оставь его), и ставь `diff: true`. \
Только то, что нужно, чтобы понять изменение: до ~25 строк на вид, имена как в коде, без пересказа всего файла. `nodes` — id узлов карты, о которых вид. Не дублируй схему flow. \
Если суть — визуальный UI, сравнение состояний или идея слишком плотная для схемы — один HTML-файл $OUT/explainer.html (вид `html`, поле `file`, `body` пустой): самодостаточный (без сети и внешних шрифтов), \
светлая и тёмная тема через prefers-color-scheme, реальные имена и данные из кода, одна мысль; не больше одного такого файла и только когда он правда нужен.";

impl ChangesView {
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
fn status_label(s: NodeStatus) -> &'static str {
    match s {
        NodeStatus::Added => "появится",
        NodeStatus::Changed => "изменится",
        NodeStatus::Removed => "уберётся",
        NodeStatus::Context => "не меняется",
    }
}

fn hhmm(at: Option<chrono::DateTime<chrono::Utc>>) -> String {
    at.map(|t| t.with_timezone(&chrono::Local).format("%H:%M").to_string()).unwrap_or_default()
}

// ---------------------------------------------------------------- rendering


impl Render for ChangesView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let p = Palette::of(&theme);
        let graph = self.report().map(map::build_report);
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
        let body: AnyElement = match (&graph, self.report()) {
            (Some(g), Some(r)) => {
                // The canvas takes most of the window height; the page scrolls around it.
                let canvas_h = (units(window.viewport_size().height) - 340.).max(440.);
                self.render_plan_report(g, r, canvas_h, &p, &theme, cx)
            }
            _ => {
                let err = self.selected.and_then(|i| self.runs.get(i)).and_then(|r| match &r.report {
                    Some(Err(e)) => Some(e.clone()),
                    _ => r.meta.error.clone(),
                });
                self.render_plan_pending(err, &p, &theme, cx).into_any_element()
            }
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
                        .child(self.render_plan_header(&self.plan, &p, &theme, cx))
                        .when(self.runs.len() > 1, |this| this.child(self.render_iterations(&p, &theme, cx)))
                        .child(body),
                ),
            )
            .when(graph.is_some(), |this| this.child(self.render_plan_bar(&self.plan, &p, &theme, cx)));
        h_flex()
            .size_full()
            .child(main)
            .when_some(self.panel.clone().zip(graph), |this, (panel, g)| this.child(self.render_panel(panel, &g, &p, &theme, window, cx)))
    }
}

impl ChangesView {

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
        v_flex()
            .gap(px(8.))
            .child(ui::section_title("Итерации", Some(self.runs.len().to_string()), p, theme).child(
                ui::mono("каждое «Отправить агенту» — новая итерация плана", theme).text_color(p.faint),
            ))
            .child(
                div().id("iterations-scroll").overflow_x_scroll().child(
                    h_flex().gap(px(6.)).pb(px(2.)).children(self.runs.iter().enumerate().map(|(i, run)| {
                        let active = self.selected == Some(i);
                        let failed = run.ok().is_none();
                        let approved = self.plan.meta.as_ref().and_then(|t| t.meta.plan_approved) == Some(run.n);
                        let decisions = run.ok().map_or(0, |r| r.decisions.len());
                        chip(SharedString::from(format!("iter-{}", run.n)), active)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.selected = Some(i);
                                cx.notify();
                            }))
                            .child(div().font_family(mono.clone()).text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).child(format!("#{}", run.n)))
                            .child(div().text_size(px(12.)).text_color(p.fg).child("план"))
                            .child(div().font_family(mono.clone()).text_size(px(11.)).text_color(p.faint).child(hhmm(run.meta.at)))
                            .when(failed, |d| d.child(ui::badge("ошибка", p.red)))
                            .when(approved, |d| d.child(ui::badge("утверждён", p.green)))
                            .when(decisions > 0, |d| d.child(ui::badge(format!("{decisions} реш."), p.blue)))
                    })),
                ),
            )
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
                            .child(mode_btn("map-before", "Сейчас", MapMode::Before, cx))
                            .child(mode_btn("map-after", "Станет", MapMode::After, cx))
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
                    .child(ui::mono("станет:", theme).text_color(p.faint))
                    .child(legend(format!("{} · {}", status_label(NodeStatus::Added), count(NodeStatus::Added)), Some(p.green), false))
                    .child(legend(format!("{} · {}", status_label(NodeStatus::Changed), count(NodeStatus::Changed)), Some(p.amber), false))
                    .child(legend(format!("{} · {}", status_label(NodeStatus::Removed), count(NodeStatus::Removed)), Some(p.red), false))
                    .when(removed_links > 0, |d| d.child(legend(format!("{} · {removed_links}", "уберётся связь"), Some(p.red), true)))
                    .child(legend(status_label(NodeStatus::Context).into(), None, true))
                    .when(!fresh.is_empty(), |d| {
                        d.child(h_flex().gap(px(5.)).text_size(px(12.)).text_color(p.blue.text).child(div().size(px(8.)).rounded_full().bg(p.blue.solid)).child(format!("новое в итерации · {}", fresh.len())))
                    })
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
        // A stage of the plan you clicked: its nodes.
        let stage_lit: Option<HashSet<usize>> = self.plan.stage.and_then(|si| r.stages.get(si)).map(|s| plan::stage_nodes(g, &s.nodes));
        let lit: Option<HashSet<usize>> = match (dto_focus, focus_ix, group_focus) {
            (None, None, _) if stage_lit.is_some() => stage_lit.clone(),
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
        let risk_count = |n: &map::MapNode| {
            r.risks
                .iter()
                .filter(|risk| risk.nodes.iter().any(|k| map::normalize(k) == n.key) || (!risk.id.is_empty() && n.risk_ids.contains(&risk.id)))
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
,
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
                            })
,
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
                d.child(ui::badge("в плане нет тестов", p.amber).text_size(px(10.5)))
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
        let groups: Vec<(&Run, &Report)> = self.runs[upto..=upto].iter().filter_map(|r| r.ok().map(|rep| (r, rep))).filter(|(_, rep)| !rep.decisions.is_empty()).collect();
        if groups.is_empty() {
            return None;
        }
        let total: usize = groups.iter().map(|(_, r)| r.decisions.len()).sum();
        let mono = theme.mono_font_family.clone();
        Some(
            v_flex()
                .gap(px(12.))
                .child(ui::section_title("Решения агента", Some(total.to_string()), p, theme).child(
                    ui::mono("что агент выбрал для плана и почему", theme).text_color(p.faint),
                ))
                .child(ui::card(p).overflow_hidden().children(groups.into_iter().enumerate().map(|(gi, (run, rep))| {
                    let kind = "план";
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

    /// «Как устроено»: the reviewer's show-me views — trees, pseudocode and code
    /// shapes drawn as text (diff lines tinted), and one HTML explainer that opens in the preview.
    fn render_views(&self, g: &Graph, r: &Report, dir: &Path, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let root = self.cwd.clone();
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
                .child(div().text_size(px(12.5)).text_color(p.muted).child("Агент не показал, как будет устроено, — попроси в замечаниях дерево вызовов или макет."))
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
                    .child(tab("lens-map", IconName::Network, "Карта", None, Lens::Map, cx))
                    .child(tab("lens-views", IconName::Eye, "Как устроено", Some(r.views.len()), Lens::Views, cx)),
            )
            .child(ui::mono(
                match self.lens {
                    Lens::Views => "как будет устроено: деревья, данные, макет",
                    Lens::Map => "канвас: к чему придёт код",
                },
                theme,
            )
            .text_color(p.faint))
    }

    fn render_diagram(&self, key: String, d: &Diagram, dir: &Path, p: &Palette, theme: &Theme) -> impl IntoElement {
        let dark = theme.is_dark();
        let image = d.image.as_ref().map(|path| themed_image(&resolve(dir, path), dark)).filter(|path| path.exists());
        let root = self.cwd.clone();
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
            Panel::Return => ("Замечания к плану".into(), self.render_return_panel(g, p, theme, window, cx).into_any_element()),
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
        let checks: Vec<Check> = r.map(|r| r.check.iter().filter(|c| !c.id.is_empty() && n.check_ids.contains(&c.id)).cloned().collect()).unwrap_or_default();
        let in_remarks = self.remarks.contains(&n.key);
        let key = n.key.clone();
        let file_path = self.cwd.join(&n.file);
        let root = self.cwd.clone();
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
                        .child(ui::eyebrow(
                            if changed { "сигнатура · сейчас → станет" } else { "сигнатура" },
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
                    v_flex().gap(px(6.)).child(ui::eyebrow("проверить руками", p, theme)).children(checks.into_iter().map(|c| {
                        h_flex()
                            .gap(px(8.))
                            .items_start()
                            .child(div().mt(px(1.)).size(px(14.)).flex_shrink_0().rounded_full().border_1().border_color(p.border_strong))
                            .child(div().flex_1().text_size(px(13.)).text_color(p.fg).child(c.title))
                    })),
                )
            })
    }

    fn render_dto_panel(&self, d: usize, g: &Graph, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let dto = &g.dtos[d];
        let mono = theme.mono_font_family.clone();
        let carried: Vec<(usize, usize)> = g.edges.iter().filter(|e| e.via == Some(d)).map(|e| (e.from, e.to)).collect();
        let file_path = self.cwd.join(&dto.file);
        let root = self.cwd.clone();
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

    fn render_return_panel(&self, g: &Graph, p: &Palette, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let note = window.use_keyed_state(SharedString::from(format!("changes-return-{}", self.plan.task)), cx, |window, cx| {
            InputState::new(window, cx).placeholder("Что поправить — своими словами")
        });
        let preview = self.remark_text(g, &note.read(cx).value());
        let send = note.clone();
        v_flex()
            .gap(px(12.))
            .child(div().text_size(px(12.5)).text_color(p.muted).child("Узлы добавляются кнопкой «в замечания» в панели узла, ответы на вопросы пиши как «В1: …». Текст уйдёт агенту, который пишет план, — он перепишет его новой итерацией."))
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
        assert_eq!(r.check[0].title, "y");
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
}
