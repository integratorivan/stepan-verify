//! «Флоу»: the project's user-flow map on a canvas — what features exist,
//! how a user reaches them, what proves they work (the feature map from the
//! «All Code Is Slop» talk). The map is `.stepan_verify/flows.yaml` in the
//! project repo: an agent builds it, sessions propose changes, you accept them.

use std::{
    cell::Cell,
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, Instant, SystemTime},
};

use gpui_kit::component::{
    ActiveTheme, Icon, IconName, Sizable as _, Theme, WindowExt as _,
    input::{Input, InputEvent, InputState},
    spinner::Spinner,
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use stepan_verify_agents::{AgentEvent, AgentHandle, ItemKind, Mode, Provider, SessionSpec};
use stepan_verify_core::{
    Home,
    flows::{self, Decisions, Feature, FlowMap, FlowProposal, Freshness, ProofKind, SessionFlows},
    flowscan::{self, EntryPoint, Inventory},
    flowsync::{self, Lag},
};
use serde::{Deserialize, Serialize};

use crate::{
    changes::canvas,
    format::ago,
    ui::{self, Palette, PressButton, Tone},
};

// ------------------------------------------------------------------ layout

pub const FEATURE_W: f32 = 240.;
pub const PATH_W: f32 = 340.;
pub const PROOF_W: f32 = 220.;
const COL_GAP: f32 = 48.;
const AREA_PAD: f32 = 20.;
const AREA_HEAD: f32 = 44.;
const ROW_GAP: f32 = 18.;
const STACK_GAP: f32 = 10.;
const AREA_GAP: f32 = 40.;
const FEATURE_H: f32 = 104.;
const PATH_HEAD: f32 = 38.;
const STEP_H: f32 = 50.;
const PROOF_H: f32 = 74.;

/// Width of one area block: feature → paths → proofs.
pub const BLOCK_W: f32 = AREA_PAD * 2. + FEATURE_W + COL_GAP + PATH_W + COL_GAP + PROOF_W;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    fn right_mid(&self, dy: f32) -> (f32, f32) {
        (self.x + self.w, self.y + dy.min(self.h / 2.))
    }
    fn left_mid(&self, dy: f32) -> (f32, f32) {
        (self.x, self.y + dy.min(self.h / 2.))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ProofSlot {
    Proof(usize),
    /// The feature has no proof at all: a red placeholder.
    Missing,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LinkKind {
    Path,
    Proof,
    Missing,
}

#[derive(Debug, Default)]
pub struct Layout {
    pub width: f32,
    pub height: f32,
    /// (area title, box).
    pub areas: Vec<(String, Rect)>,
    /// (feature index, box).
    pub features: Vec<(usize, Rect)>,
    /// (feature, path, box).
    pub paths: Vec<(usize, usize, Rect)>,
    /// (feature, slot, box).
    pub proofs: Vec<(usize, ProofSlot, Rect)>,
    pub links: Vec<((f32, f32), (f32, f32), LinkKind)>,
}

/// Features grouped by area in map order; unknown areas land in «Другое».
pub fn grouped<'a>(map: &'a FlowMap, keep: &dyn Fn(&Feature) -> bool) -> Vec<(String, Vec<(usize, &'a Feature)>)> {
    let mut out: Vec<(String, Vec<(usize, &Feature)>)> = Vec::new();
    let title = |id: &str| map.areas.iter().find(|a| a.id == id).map(|a| a.title.clone()).filter(|t| !t.is_empty());
    let mut order: Vec<String> = map.areas.iter().map(|a| a.id.clone()).collect();
    for f in &map.features {
        if !order.contains(&f.area) {
            order.push(f.area.clone());
        }
    }
    for area in order {
        let list: Vec<(usize, &Feature)> = map.features.iter().enumerate().filter(|(_, f)| f.area == area && keep(f)).collect();
        if list.is_empty() {
            continue;
        }
        let name = title(&area).unwrap_or_else(|| if area.is_empty() { "Другое".to_owned() } else { area.clone() });
        out.push((name, list));
    }
    out
}

/// Proofs of a feature that cover a path (no `covers` = the whole feature).
fn proof_covers_path(f: &Feature, proof: usize, path: usize) -> bool {
    let covers = &f.proofs[proof].covers;
    covers.is_empty() || f.paths[path].steps.iter().any(|s| covers.contains(&s.id))
}

/// Three columns per area block (что есть → как дойти → чем доказано); blocks
/// flow into `cols` columns, each block into the shortest one, so the map
/// fills the window instead of running off in one long row.
pub fn layout(map: &FlowMap, keep: &dyn Fn(&Feature) -> bool, cols: usize) -> Layout {
    let cols = cols.max(1);
    let mut lay = Layout::default();
    let mut heights = vec![0f32; cols];
    for (title, features) in grouped(map, keep) {
        let col = (0..cols).min_by(|&a, &b| heights[a].total_cmp(&heights[b])).unwrap_or(0);
        let x0 = col as f32 * (BLOCK_W + AREA_GAP);
        let top = heights[col] + if heights[col] > 0. { AREA_GAP } else { 0. };
        let mut y = top + AREA_HEAD;
        let (fx, px_, qx) = (x0 + AREA_PAD, x0 + AREA_PAD + FEATURE_W + COL_GAP, x0 + AREA_PAD + FEATURE_W + COL_GAP + PATH_W + COL_GAP);
        for (fi, f) in features {
            let path_h: Vec<f32> = f.paths.iter().map(|p| PATH_HEAD + STEP_H * p.steps.len().max(1) as f32).collect();
            let paths_total = path_h.iter().sum::<f32>() + STACK_GAP * path_h.len().saturating_sub(1) as f32;
            let proofs_n = f.proofs.len().max(1);
            let proofs_total = PROOF_H * proofs_n as f32 + STACK_GAP * (proofs_n - 1) as f32;
            let row_h = FEATURE_H.max(paths_total).max(proofs_total);
            let fr = Rect { x: fx, y, w: FEATURE_W, h: FEATURE_H };
            lay.features.push((fi, fr));
            let mut py = y;
            let mut path_rects = Vec::new();
            for (pi, h) in path_h.iter().enumerate() {
                let r = Rect { x: px_, y: py, w: PATH_W, h: *h };
                lay.paths.push((fi, pi, r));
                lay.links.push((fr.right_mid(36.), r.left_mid(19.), LinkKind::Path));
                path_rects.push(r);
                py += h + STACK_GAP;
            }
            let mut qy = y;
            if f.proofs.is_empty() {
                let r = Rect { x: qx, y: qy, w: PROOF_W, h: PROOF_H };
                lay.proofs.push((fi, ProofSlot::Missing, r));
                match path_rects.is_empty() {
                    true => lay.links.push((fr.right_mid(36.), r.left_mid(28.), LinkKind::Missing)),
                    false => path_rects.iter().for_each(|p| lay.links.push((p.right_mid(19.), r.left_mid(28.), LinkKind::Missing))),
                }
            }
            for qi in 0..f.proofs.len() {
                let r = Rect { x: qx, y: qy, w: PROOF_W, h: PROOF_H };
                lay.proofs.push((fi, ProofSlot::Proof(qi), r));
                let covering: Vec<&Rect> = path_rects.iter().enumerate().filter(|(pi, _)| proof_covers_path(f, qi, *pi)).map(|(_, r)| r).collect();
                if covering.is_empty() {
                    lay.links.push((fr.right_mid(36.), r.left_mid(28.), LinkKind::Proof));
                }
                for p in covering {
                    lay.links.push((p.right_mid(19.), r.left_mid(28.), LinkKind::Proof));
                }
                qy += PROOF_H + STACK_GAP;
            }
            y += row_h + ROW_GAP;
        }
        let h = y - ROW_GAP + AREA_PAD - top;
        lay.areas.push((title, Rect { x: x0, y: top, w: BLOCK_W, h }));
        heights[col] = top + h;
    }
    lay.width = (0..cols).filter(|&c| heights[c] > 0.).count().max(1) as f32 * (BLOCK_W + AREA_GAP) - AREA_GAP;
    lay.height = heights.iter().copied().fold(0., f32::max);
    lay
}

/// How many area columns fit a viewport this wide at 100%.
pub fn columns_for(width: f32) -> usize {
    (((width - canvas::FIT_MARGIN * 2. + AREA_GAP) / (BLOCK_W + AREA_GAP)).floor() as usize).max(1)
}

/// Search: title, aliases, summary, area and step text.
pub fn matches(f: &Feature, query: &str) -> bool {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return true;
    }
    let hay = |s: &str| s.to_lowercase().contains(&q);
    hay(&f.title)
        || hay(&f.summary)
        || hay(&f.area)
        || hay(&f.id)
        || f.aliases.iter().any(|a| hay(a))
        || f.steps().any(|s| hay(&s.screen) || hay(&s.action) || hay(&s.result) || s.entities.iter().any(|e| hay(e)))
}

// ------------------------------------------------------------------ view

pub enum FlowsEvent {
    /// Open a session by its native id.
    OpenSession(String),
    /// The map's lag behind the code changed (sidebar badge).
    LagChanged,
}

impl EventEmitter<FlowsEvent> for FlowsView {}

#[derive(Clone, PartialEq)]
enum Selection {
    Feature(usize),
    Proposals,
}

/// The map is built in passes so nothing gets lost on big projects:
/// inventory (all features, no detail) → details per area batch →
/// proofs → coverage check against the deterministic entry-point checklist.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Inventory,
    Details,
    Proofs,
    Coverage,
    Done,
}

impl Stage {
    const PIPELINE: [Stage; 4] = [Stage::Inventory, Stage::Details, Stage::Proofs, Stage::Coverage];

    fn title(self) -> &'static str {
        match self {
            Stage::Inventory => "Опись",
            Stage::Details => "Детали",
            Stage::Proofs => "Доказательства",
            Stage::Coverage => "Сверка",
            Stage::Done => "Готово",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", content = "detail", rename_all = "snake_case")]
pub enum BatchState {
    Waiting,
    Running,
    Done(u64),
    Failed(String),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BatchRun {
    pub batch: flows::Batch,
    pub state: BatchState,
    pub attempts: u32,
}

/// One agent working on one pass or batch.
struct Job {
    key: String,
    steps: Vec<String>,
    started: Instant,
    _handle: AgentHandle,
    _events: Task<()>,
}

/// Features per detail agent: small enough to be described thoroughly.
pub const BATCH_SIZE: usize = 6;
/// Automatic «add what is missing» rounds before asking you.
const AUTO_COVER_ROUNDS: u32 = 2;

struct Pipeline {
    n: usize,
    started: Instant,
    started_at: chrono::DateTime<chrono::Utc>,
    who: String,
    provider: Provider,
    model: Option<String>,
    update: bool,
    stage: Stage,
    inventory: Option<Inventory>,
    batches: Vec<BatchRun>,
    jobs: Vec<Job>,
    limit: usize,
    uncovered: Vec<EntryPoint>,
    rounds: u32,
    log: Vec<String>,
    stopped: bool,
    error: Option<String>,
    finished_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Passes already asked to fix an unreadable result file.
    repaired: std::collections::HashSet<String>,
    /// Sync run: only features the changed files touch, plus new entry points.
    sync: bool,
    /// Files changed since the last sync (sync runs).
    changed: Vec<String>,
}

impl Pipeline {
    fn running(&self) -> bool {
        self.stage != Stage::Done && !self.stopped && self.error.is_none()
    }

    fn note(&mut self, line: impl Into<String>) {
        self.log.push(format!("{} {}", chrono::Local::now().format("%H:%M:%S"), line.into()));
    }
}

/// What the build log on disk keeps: `state/flows/<repo>/builds/<n>/build.json`.
#[derive(Serialize)]
struct BuildLog<'a> {
    n: usize,
    started_at: chrono::DateTime<chrono::Utc>,
    finished_at: Option<chrono::DateTime<chrono::Utc>>,
    who: &'a str,
    update: bool,
    stage: Stage,
    stopped: bool,
    error: &'a Option<String>,
    batches: &'a [BatchRun],
    uncovered: &'a [EntryPoint],
    log: &'a [String],
}

/// A coverage pass result: new features, entries attached to existing ones, ignored entries.
#[derive(Default, Deserialize)]
#[serde(default)]
struct CoverPart {
    upsert: Vec<Feature>,
    attach: Vec<Attach>,
    ignored: Vec<flows::Ignored>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Attach {
    feature: String,
    entries: Vec<String>,
}

pub struct FlowsView {
    home: Home,
    projects: Vec<PathBuf>,
    project: Option<PathBuf>,
    map: Option<FlowMap>,
    error: Option<String>,
    mtime: Option<SystemTime>,
    sessions: Vec<SessionFlows>,
    decisions: Decisions,
    /// Freshness per feature, computed on load (it stats files).
    fresh: Vec<Freshness>,
    search: Entity<InputState>,
    query: String,
    view: canvas::View,
    drag: Option<canvas::Drag>,
    dragged: bool,
    viewport: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// The view was placed for this map (first area at 100%).
    placed: bool,
    selected: Option<Selection>,
    pipeline: Option<Pipeline>,
    build_error: Option<String>,
    /// How far the map lags behind the code (git since `synced_commit`).
    lag: Option<Lag>,
    /// When the changed files were last touched: auto-sync waits for quiet.
    lag_seen: Option<(usize, Instant)>,
    _poll: Task<()>,
    _ticker: Task<()>,
}

impl FlowsView {
    pub fn new(home: Home, project: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Найти фичу, экран, сущность…"));
        cx.subscribe(&search, |this: &mut Self, s, e: &InputEvent, cx| {
            if matches!(e, InputEvent::Change) {
                this.query = s.read(cx).value().to_string();
                cx.notify();
            }
        })
        .detach();
        let project = project.or_else(|| last_project(&home));
        // Every Claude / Codex session learns to keep the map current (idempotent).
        sync_skill(&home, crate::settings::config(cx).flows.skill);
        let mut this = Self {
            home,
            projects: Vec::new(),
            project: None,
            map: None,
            error: None,
            mtime: None,
            sessions: Vec::new(),
            decisions: Decisions::default(),
            fresh: Vec::new(),
            search,
            query: String::new(),
            view: canvas::View::default(),
            drag: None,
            dragged: false,
            viewport: Rc::new(Cell::new(None)),
            placed: false,
            selected: None,
            pipeline: None,
            build_error: None,
            lag: None,
            lag_seen: None,
            _poll: Task::ready(()),
            _ticker: Task::ready(()),
        };
        if let Some(p) = project {
            this.select_project(p, cx);
        }
        // The file is edited by agents (and you): follow it; proposals arrive with sessions.
        this._poll = cx.spawn(async move |this, cx| {
            let mut n = 0u32;
            loop {
                cx.background_executor().timer(Duration::from_millis(1500)).await;
                n += 1;
                let alive = this.update(cx, |this, cx| {
                    let Some(repo) = this.project.clone() else { return };
                    let mtime = fs::metadata(flows::path(&repo)).and_then(|m| m.modified()).ok();
                    if mtime != this.mtime && !this.is_building() {
                        this.load(cx);
                    } else if n % 10 == 0 {
                        this.load_sessions();
                        cx.notify();
                    }
                    if n % 20 == 1 {
                        this.check_lag(cx);
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        this
    }

    /// Projects worth offering (from the session scan), most recent first.
    pub fn set_projects(&mut self, projects: Vec<PathBuf>, cx: &mut Context<Self>) {
        self.projects = projects;
        if self.project.is_none() {
            if let Some(first) = self.projects.first().cloned() {
                self.select_project(first, cx);
            }
        }
        cx.notify();
    }

    fn select_project(&mut self, repo: PathBuf, cx: &mut Context<Self>) {
        if self.project.as_ref() == Some(&repo) {
            return;
        }
        let _ = fs::create_dir_all(self.home.state().join("flows"));
        let _ = fs::write(self.home.state().join("flows/last-project"), repo.display().to_string());
        self.project = Some(repo);
        self.selected = None;
        self.build_error = None;
        self.placed = false;
        self.load(cx);
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.project.clone() else { return };
        self.mtime = fs::metadata(flows::path(&repo)).and_then(|m| m.modified()).ok();
        match FlowMap::load(&repo) {
            Ok(map) => {
                self.fresh = map.as_ref().map(|m| m.features.iter().map(|f| f.freshness(&repo)).collect()).unwrap_or_default();
                if map.is_none() {
                    self.placed = false;
                }
                self.map = map;
                self.error = None;
            }
            Err(err) => self.error = Some(format!("{err:#}")),
        }
        self.load_sessions();
        cx.notify();
    }

    fn load_sessions(&mut self) {
        let Some(repo) = &self.project else { return };
        self.sessions = flows::session_flows(&self.home, repo);
        self.decisions = Decisions::load(&self.home, repo);
    }

    fn pending(&self) -> Vec<&SessionFlows> {
        flows::pending(&self.sessions, &self.decisions)
    }

    fn decide(&mut self, id: String, accept: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(repo) = self.project.clone() else { return };
        if accept {
            let Some(s) = self.sessions.iter().find(|s| s.id() == id) else { return };
            let mut map = self.map.clone().unwrap_or_else(|| FlowMap {
                version: 1,
                project: repo.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                ..Default::default()
            });
            map.apply(&s.proposal);
            if let Err(err) = map.save(&repo) {
                window.push_notification(gpui_kit::component::notification::Notification::error(format!("Карта не сохранилась: {err:#}")), cx);
                return;
            }
            self.decisions.accepted.insert(id);
        } else {
            self.decisions.rejected.insert(id);
        }
        let _ = self.decisions.save(&self.home, &repo);
        self.load(cx);
    }

    // -------------------------------------------------------------- building

    /// Recompute the lag in the background (git calls); maybe auto-sync.
    fn check_lag(&mut self, cx: &mut Context<Self>) {
        let (Some(repo), Some(map)) = (self.project.clone(), self.map.clone()) else { return };
        if map.synced_commit.is_none() || self.is_building() {
            return;
        }
        cx.spawn(async move |this, cx| {
            let lag = cx.background_spawn(async move { flowsync::lag(&repo, &map) }).await;
            let _ = this.update(cx, |this, cx| {
                let n = lag.as_ref().map_or(0, |l| l.changed.len());
                if this.lag_seen.map(|(k, _)| k) != Some(n) {
                    this.lag_seen = Some((n, Instant::now()));
                }
                this.lag = lag.filter(Lag::matters);
                // Auto-sync only after the edits went quiet for a while: a session is likely done.
                let quiet = this.lag_seen.is_some_and(|(_, t)| t.elapsed() > Duration::from_secs(120));
                if this.lag.is_some() && quiet && crate::settings::config(cx).flows.auto_sync && !this.is_building() {
                    this.sync(cx);
                }
                cx.emit(FlowsEvent::LagChanged);
                cx.notify();
            });
        })
        .detach();
    }

    /// Sidebar badge: commits (or files) the map is behind.
    pub fn lag_badge(&self) -> Option<String> {
        let lag = self.lag.as_ref()?;
        Some(if lag.commits > 0 { format!("{}", lag.commits) } else { "!".into() })
    }

    fn is_building(&self) -> bool {
        self.pipeline.as_ref().is_some_and(Pipeline::running)
    }

    /// Start (or restart) the multi-pass build of the map.
    pub(crate) fn build(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.project.clone() else { return };
        if self.is_building() {
            return;
        }
        let config = crate::settings::config(cx);
        let provider = match config.agents.default_provider {
            stepan_verify_core::config::ProviderSetting::Codex => Provider::Codex,
            _ => Provider::Claude,
        };
        let model = match provider {
            Provider::Claude => Some(config.changes.model.clone()),
            Provider::Codex => Some(config.changes.codex_model.clone()).filter(|m| !m.is_empty()),
        };
        let n = builds_dir(&self.home, &repo)
            .read_dir()
            .map(|rd| rd.flatten().filter_map(|e| e.file_name().to_str()?.parse::<usize>().ok()).max().unwrap_or(0))
            .unwrap_or(0)
            + 1;
        let mut pipe = Pipeline {
            n,
            started: Instant::now(),
            started_at: chrono::Utc::now(),
            who: crate::changes::reviewer_name(provider, model.as_deref()),
            provider,
            model,
            update: flows::path(&repo).exists(),
            stage: Stage::Inventory,
            inventory: None,
            batches: Vec::new(),
            jobs: Vec::new(),
            limit: config.agents.autopilot_limit.clamp(1, 4),
            uncovered: Vec::new(),
            rounds: 0,
            log: Vec::new(),
            stopped: false,
            error: None,
            finished_at: None,
            repaired: Default::default(),
            sync: false,
            changed: Vec::new(),
        };
        pipe.note("Опись точек входа по коду (без агента)");
        self.pipeline = Some(pipe);
        self.build_error = None;
        let scan_repo = repo.clone();
        cx.spawn(async move |this, cx| {
            let inv = cx.background_spawn(async move { flowscan::scan(&scan_repo) }).await;
            let _ = this.update(cx, |this, cx| this.inventory_ready(inv, cx));
        })
        .detach();
        self.restart_ticker(cx);
        self.save_log();
        cx.notify();
    }

    /// Bring the map up to date with the code changed since `synced_commit`:
    /// re-describe only touched features, pick up new entry points, drop gone ones.
    pub(crate) fn sync(&mut self, cx: &mut Context<Self>) {
        if self.project.is_none() || self.is_building() {
            return;
        }
        let synced = self.map.as_ref().and_then(|m| m.synced_commit.clone());
        let Some(since) = synced else {
            // Never synced (or no map): a full update build sets the baseline.
            return self.build(cx);
        };
        self.build(cx);
        let Some(pipe) = self.pipeline.as_mut() else { return };
        pipe.sync = true;
        pipe.update = true;
        pipe.note(format!("Синхронизация с {}", &since[..since.len().min(8)]));
        self.save_log();
    }

    /// Sync runs skip the inventory agent: git says what changed, the scan finds new entry points.
    fn sync_ready(&mut self, inv: Inventory, cx: &mut Context<Self>) {
        let Some(repo) = self.project.clone() else { return };
        let Some(mut map) = self.map.clone() else { return };
        let lag = flowsync::lag(&repo, &map).unwrap_or_default();
        let Some(pipe) = self.pipeline.as_mut() else { return };
        let mut touched: std::collections::BTreeSet<String> = map.touched(&lag.changed, &[]).into_iter().map(|t| t.feature).collect();
        // A deleted page also breaks paths of other features that lead to it.
        let routes = deleted_routes(&repo, &lag.changed);
        touched.extend(map.features.iter().filter(|f| routes.iter().any(|r| mentions(f, r))).map(|f| f.id.clone()));
        // Features whose every file is gone are probably gone too: the detail pass may remove them.
        let gone: Vec<String> = map
            .features
            .iter()
            .filter(|f| {
                let files: Vec<&String> = f.steps().flat_map(|s| &s.files).collect();
                !files.is_empty() && files.iter().all(|x| !repo.join(x).exists())
            })
            .map(|f| f.id.clone())
            .collect();
        for f in map.features.iter_mut().filter(|f| touched.contains(&f.id) || gone.contains(&f.id)) {
            f.status = "draft".into();
        }
        let ids: Vec<String> = map.drafts().map(|f| f.id.clone()).collect();
        pipe.batches = map.batches(&ids, BATCH_SIZE).into_iter().map(|batch| BatchRun { batch, state: BatchState::Waiting, attempts: 0 }).collect();
        pipe.changed = lag.changed.clone();
        pipe.note(format!(
            "Синхронизация: {} коммитов, {} файлов изменено, затронуто фич {}, возможно удалено {}",
            lag.commits,
            lag.changed.len(),
            touched.len(),
            gone.len()
        ));
        pipe.inventory = Some(inv);
        pipe.stage = Stage::Details;
        if let Err(err) = map.save(&repo) {
            pipe.error = Some(format!("Карта не сохранилась: {err:#}"));
        }
        self.load(cx);
        self.advance(cx);
    }

    fn inventory_ready(&mut self, inv: Inventory, cx: &mut Context<Self>) {
        let Some(repo) = self.project.clone() else { return };
        let Some(pipe) = self.pipeline.as_mut() else { return };
        if pipe.stopped {
            return;
        }
        pipe.note(format!(
            "Найдено точек входа: {} ({}), тестов: {}, stories: {}",
            inv.entries.len(),
            if inv.stacks.is_empty() { "стек не распознан".to_owned() } else { inv.stacks.join(", ") },
            inv.tests.len(),
            inv.stories.len()
        ));
        let parts = parts_dir(&repo);
        let _ = fs::create_dir_all(&parts);
        let checklist = parts.join("checklist.json");
        if let Err(err) = serde_json::to_string_pretty(&inv).map_err(anyhow::Error::from).and_then(|t| Ok(fs::write(&checklist, t)?)) {
            pipe.error = Some(format!("Не записал чек-лист: {err:#}"));
            self.save_log();
            cx.notify();
            return;
        }
        if pipe.sync {
            return self.sync_ready(inv, cx);
        }
        let update = pipe.update;
        pipe.inventory = Some(inv);
        let prompt = inventory_prompt(&repo, &checklist, update);
        self.start_job("inventory".into(), prompt, cx);
    }

    fn start_job(&mut self, key: String, prompt: String, cx: &mut Context<Self>) {
        let Some(repo) = self.project.clone() else { return };
        let Some(pipe) = self.pipeline.as_mut() else { return };
        let spec = SessionSpec {
            provider: pipe.provider,
            native_id: None,
            transcript: None,
            cwd: repo.clone(),
            // It reads the whole project and writes one file under .stepan_verify; it must not stop to ask.
            mode: Mode::FullAccess,
            image_cache: Some(self.home.state().join("images")),
            env: vec![("STEPAN_VERIFY_FLOWS_OUT".into(), parts_dir(&repo).display().to_string())],
            model: pipe.model.clone(),
            instructions: None,
        };
        let (handle, events) = stepan_verify_agents::open(spec);
        handle.send(prompt);
        let k = key.clone();
        let events = cx.spawn(async move |this, cx| {
            while let Ok(event) = events.recv().await {
                let done = matches!(event, AgentEvent::TurnCompleted { .. } | AgentEvent::Exited | AgentEvent::Error(_));
                let alive = this.update(cx, |this, cx| {
                    this.on_job_event(&k, event, cx);
                    cx.notify();
                });
                if alive.is_err() || done {
                    break;
                }
            }
        });
        pipe.note(format!("Запущен агент: {key}"));
        pipe.jobs.push(Job { key, steps: Vec::new(), started: Instant::now(), _handle: handle, _events: events });
    }

    fn on_job_event(&mut self, key: &str, event: AgentEvent, cx: &mut Context<Self>) {
        let Some(pipe) = self.pipeline.as_mut() else { return };
        let Some(job) = pipe.jobs.iter_mut().find(|j| j.key == key) else { return };
        match event {
            AgentEvent::Upsert(item) if item.kind == ItemKind::Tool && !item.title.is_empty() => {
                let step = humanize(&item.title);
                if job.steps.last() != Some(&step) {
                    job.steps.push(step);
                }
            }
            AgentEvent::TurnCompleted { error } => self.job_done(key, error, cx),
            AgentEvent::Exited => self.job_done(key, Some("агент завершился раньше времени".into()), cx),
            AgentEvent::Error(err) => self.job_done(key, Some(err), cx),
            _ => {}
        }
    }

    /// A pass finished: merge what it wrote, then move the pipeline on.
    fn job_done(&mut self, key: &str, error: Option<String>, cx: &mut Context<Self>) {
        let Some(repo) = self.project.clone() else { return };
        let Some(pipe) = self.pipeline.as_mut() else { return };
        let Some(pos) = pipe.jobs.iter().position(|j| j.key == key) else { return };
        let job = pipe.jobs.remove(pos);
        let secs = job.started.elapsed().as_secs();
        if pipe.stopped {
            return;
        }
        let part = parts_dir(&repo).join(format!("{key}.json"));
        let archive = builds_dir(&self.home, &repo).join(pipe.n.to_string()).join("parts");
        let text = fs::read_to_string(&part).ok();
        // An unreadable result gets one repair round by an agent before it counts as a failure.
        if let Some(t) = &text {
            if let Err(e) = check_part(key, t) {
                if pipe.repaired.insert(key.to_owned()) {
                    pipe.note(format!("{key}: файл не читается ({e}) — прошу агента починить"));
                    let prompt = repair_prompt(&part, &e);
                    self.start_job(key.to_owned(), prompt, cx);
                    self.save_log();
                    cx.notify();
                    return;
                }
            }
            let _ = fs::create_dir_all(&archive);
            let _ = fs::write(archive.join(format!("{key}.json")), t);
            let _ = fs::remove_file(&part);
        }
        let mut map = self.map.clone().unwrap_or_else(|| FlowMap {
            version: 1,
            project: repo.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            ..Default::default()
        });
        let missing = |error: Option<String>| -> String {
            error.unwrap_or_else(|| format!("агент не записал {}", part.strip_prefix(&repo).unwrap_or(&part).display()))
        };
        let parsed: Result<(), String> = match key {
            "inventory" => match text.as_deref().map(read_part::<FlowProposal>) {
                Some(Ok(prop)) => {
                    merge_inventory(&mut map, &prop);
                    if pipe.update {
                        mark_changed_drafts(&mut map, &repo);
                    }
                    let ids: Vec<String> = map.drafts().map(|f| f.id.clone()).collect();
                    pipe.batches = map
                        .batches(&ids, BATCH_SIZE)
                        .into_iter()
                        .map(|batch| BatchRun { batch, state: BatchState::Waiting, attempts: 0 })
                        .collect();
                    pipe.note(format!("Опись: {} обл., {} фич, на детали {} пачек", map.areas.len(), map.features.len(), pipe.batches.len()));
                    pipe.stage = Stage::Details;
                    Ok(())
                }
                Some(Err(e)) => Err(format!("опись не читается: {e}")),
                None => Err(missing(error)),
            },
            "proofs" => {
                match text.as_deref().map(read_part::<flows::ProofsPart>) {
                    Some(Ok(part)) => {
                        map.apply_proofs(&part);
                        pipe.note(format!("Доказательства: {} фич", part.features.len()));
                    }
                    // Proofs are a bonus pass: a miss does not stop the build.
                    Some(Err(e)) => pipe.note(format!("Доказательства не прочитались: {e}")),
                    None => pipe.note(format!("Доказательства: {}", missing(error))),
                }
                stamp_proofs(&mut map);
                pipe.stage = Stage::Coverage;
                Ok(())
            }
            k if k.starts_with("cover-") => {
                match text.as_deref().map(read_part::<CoverPart>) {
                    Some(Ok(part)) => {
                        let added = merge_cover(&mut map, part);
                        pipe.note(format!("Сверка: добавлено/привязано {added}"));
                    }
                    Some(Err(e)) => pipe.note(format!("Сверка не прочиталась: {e}")),
                    None => pipe.note(format!("Сверка: {}", missing(error))),
                }
                stamp_proofs(&mut map);
                pipe.stage = Stage::Coverage;
                Ok(())
            }
            batch => {
                let Some(run) = pipe.batches.iter_mut().find(|b| b.batch.id == batch) else { return };
                match text.as_deref().map(read_part::<FlowProposal>) {
                    Some(Ok(prop)) => {
                        merge_details(&mut map, &prop, pipe.sync);
                        run.state = BatchState::Done(secs);
                    }
                    Some(Err(e)) => run.state = BatchState::Failed(format!("ответ не читается: {e}")),
                    None => run.state = BatchState::Failed(missing(error)),
                }
                let line = format!("Пачка «{}»: {:?}", run.batch.title, run.state);
                pipe.note(line);
                Ok(())
            }
        };
        if let Err(e) = parsed {
            pipe.error = Some(format!("{}: {e}", key));
            pipe.note(format!("Ошибка: {e}"));
        }
        // Every merged pass lands on disk right away: progress survives failures.
        if pipe.error.is_none() {
            if let Err(err) = map.save(&repo) {
                pipe.error = Some(format!("Карта не сохранилась: {err:#}"));
            }
        }
        let had_map = self.map.is_some();
        self.load(cx);
        if !had_map {
            self.placed = false;
        }
        self.advance(cx);
    }

    /// Start whatever the current stage needs next.
    fn advance(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.project.clone() else { return };
        let Some(map) = self.map.clone() else {
            self.save_log();
            return;
        };
        let Some(pipe) = self.pipeline.as_mut() else { return };
        if !pipe.running() {
            self.save_log();
            cx.notify();
            return;
        }
        match pipe.stage {
            Stage::Inventory | Stage::Done => {}
            Stage::Details => {
                let mut start = Vec::new();
                let busy = pipe.jobs.len();
                for run in pipe.batches.iter_mut().filter(|b| b.state == BatchState::Waiting).take(pipe.limit.saturating_sub(busy)) {
                    run.state = BatchState::Running;
                    run.attempts += 1;
                    start.push(run.batch.clone());
                }
                let all_settled = pipe.jobs.is_empty() && start.is_empty() && !pipe.batches.iter().any(|b| matches!(b.state, BatchState::Waiting | BatchState::Running));
                let changed = pipe.changed.clone();
                for batch in start {
                    let mut prompt = details_prompt(&repo, &map, &batch);
                    if !changed.is_empty() {
                        prompt.push_str(&sync_note(&changed));
                    }
                    self.start_job(batch.id.clone(), prompt, cx);
                }
                if all_settled {
                    let Some(pipe) = self.pipeline.as_mut() else { return };
                    pipe.stage = Stage::Proofs;
                    let inv = pipe.inventory.clone().unwrap_or_default();
                    let media = media_files(&self.home, &repo);
                    let described: Vec<String> = pipe.batches.iter().flat_map(|b| b.batch.features.clone()).collect();
                    if pipe.sync && described.is_empty() {
                        // Nothing re-described: no proofs to re-check.
                        pipe.stage = Stage::Coverage;
                        return self.advance(cx);
                    }
                    let scope = if pipe.sync { FlowMap { features: map.features.iter().filter(|f| described.contains(&f.id)).cloned().collect(), ..map.clone() } } else { map.clone() };
                    let prompt = proofs_prompt(&repo, &scope, &inv, &media);
                    self.start_job("proofs".into(), prompt, cx);
                }
            }
            Stage::Proofs => {}
            Stage::Coverage => {
                if !pipe.jobs.is_empty() {
                    return;
                }
                let inv = pipe.inventory.clone().unwrap_or_default();
                pipe.uncovered = flowscan::uncovered(&map, &inv).into_iter().cloned().collect();
                if pipe.uncovered.is_empty() || pipe.rounds >= AUTO_COVER_ROUNDS {
                    pipe.note(format!("Сверка: не покрыто {}", pipe.uncovered.len()));
                    pipe.stage = Stage::Done;
                    pipe.finished_at = Some(chrono::Utc::now());
                    let _ = fs::remove_dir_all(parts_dir(&repo));
                    // The map now describes HEAD: lag is measured from here.
                    let mut synced = map.clone();
                    synced.synced_commit = flowsync::head(&repo);
                    if synced.synced_commit.is_some() && synced.save(&repo).is_ok() {
                        self.lag = None;
                        self.load(cx);
                    }
                } else {
                    pipe.rounds += 1;
                    let key = format!("cover-{}", pipe.rounds);
                    let prompt = cover_prompt(&repo, &pipe.uncovered.iter().collect::<Vec<_>>(), &key);
                    pipe.note(format!("Сверка: не покрыто {} — дополняю (раунд {})", pipe.uncovered.len(), pipe.rounds));
                    self.start_job(key, prompt, cx);
                }
            }
        }
        self.save_log();
        cx.notify();
    }

    /// Stop every agent of the build; what was merged stays in the map.
    fn stop_build(&mut self, cx: &mut Context<Self>) {
        let Some(pipe) = self.pipeline.as_mut() else { return };
        pipe.stopped = true;
        pipe.jobs.clear();
        for run in pipe.batches.iter_mut().filter(|b| b.state == BatchState::Running) {
            run.state = BatchState::Failed("остановлено".into());
        }
        pipe.finished_at = Some(chrono::Utc::now());
        pipe.note("Остановлено");
        self.save_log();
        cx.notify();
    }

    /// Re-run one failed batch (and the passes after it).
    fn retry_batch(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(pipe) = self.pipeline.as_mut() else { return };
        let Some(run) = pipe.batches.iter_mut().find(|b| b.batch.id == id) else { return };
        run.state = BatchState::Waiting;
        pipe.stopped = false;
        pipe.error = None;
        pipe.finished_at = None;
        pipe.stage = Stage::Details;
        pipe.note(format!("Повтор пачки {id}"));
        self.restart_ticker(cx);
        self.advance(cx);
    }

    /// One more coverage round on demand.
    fn cover_more(&mut self, cx: &mut Context<Self>) {
        let Some(pipe) = self.pipeline.as_mut() else { return };
        pipe.stopped = false;
        pipe.error = None;
        pipe.finished_at = None;
        pipe.stage = Stage::Coverage;
        pipe.rounds = pipe.rounds.min(AUTO_COVER_ROUNDS.saturating_sub(1));
        self.restart_ticker(cx);
        self.advance(cx);
    }

    fn restart_ticker(&mut self, cx: &mut Context<Self>) {
        self._ticker = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let going = this.update(cx, |this, cx| {
                    cx.notify();
                    this.is_building()
                });
                if !matches!(going, Ok(true)) {
                    break;
                }
            }
        });
    }

    fn save_log(&self) {
        let (Some(repo), Some(pipe)) = (&self.project, &self.pipeline) else { return };
        let dir = builds_dir(&self.home, repo).join(pipe.n.to_string());
        let _ = fs::create_dir_all(&dir);
        let log = BuildLog {
            n: pipe.n,
            started_at: pipe.started_at,
            finished_at: pipe.finished_at,
            who: &pipe.who,
            update: pipe.update,
            stage: pipe.stage,
            stopped: pipe.stopped,
            error: &pipe.error,
            batches: &pipe.batches,
            uncovered: &pipe.uncovered,
            log: &pipe.log,
        };
        if let Ok(text) = serde_json::to_string_pretty(&log) {
            let _ = fs::write(dir.join("build.json"), text);
        }
    }

    // -------------------------------------------------------------- canvas input

    fn local(&self, at: Point<Pixels>) -> (f32, f32) {
        let o = self.viewport.get().map(|b| b.origin).unwrap_or_default();
        (f32::from(at.x - o.x), f32::from(at.y - o.y))
    }

    fn viewport_size(&self) -> (f32, f32) {
        self.viewport.get().map_or((1200., 700.), |b| (f32::from(b.size.width), f32::from(b.size.height)))
    }

    /// Readable by default: the first area in the top-left corner at 100%,
    /// or as close to it as lets a whole block (all three columns) fit across.
    fn place(&mut self) {
        let (w, _) = self.viewport_size();
        let zoom = ((w - canvas::FIT_MARGIN * 2.) / BLOCK_W).clamp(0.6, 1.);
        self.view = canvas::View { x: canvas::FIT_MARGIN, y: canvas::FIT_TOP, zoom };
        self.placed = true;
    }

    fn fit(&mut self, world: (f32, f32), cx: &mut Context<Self>) {
        self.view = canvas::View::fit(world, self.viewport_size());
        self.placed = true;
        cx.notify();
    }

    fn zoom_center(&mut self, factor: f32, cx: &mut Context<Self>) {
        let (w, h) = self.viewport_size();
        self.view = self.view.zoom_at(factor, (w / 2., h / 2.));
        cx.notify();
    }
}

fn last_project(home: &Home) -> Option<PathBuf> {
    let p = PathBuf::from(fs::read_to_string(home.state().join("flows/last-project")).ok()?.trim());
    p.is_dir().then_some(p)
}

fn humanize(title: &str) -> String {
    let known = [
        ("flows.yaml", "Пишу карту флоу"),
        ("route", "Смотрю маршруты"),
        ("router", "Смотрю маршруты"),
        ("test", "Смотрю тесты"),
        ("spec", "Смотрю тесты"),
        ("git log", "Смотрю историю"),
        ("ls ", "Осматриваю папки"),
        ("find ", "Ищу экраны и тесты"),
        ("rg ", "Ищу по коду"),
        ("grep", "Ищу по коду"),
    ];
    let t = title.to_lowercase();
    known.iter().find(|(needle, _)| t.contains(needle)).map(|(_, l)| (*l).to_owned()).unwrap_or_else(|| {
        // Shell lines lose their `cd … &&` / env noise: «Команда npm test».
        let (label, detail) = crate::live_card::split_title(title);
        let line = if detail.is_empty() { label } else { format!("{label} {detail}") };
        line.chars().take(90).collect()
    })
}

// ------------------------------------------------------------------ passes

/// Where pass results land in the repo before they are merged (removed after the build).
pub fn parts_dir(repo: &Path) -> PathBuf {
    repo.join(".stepan_verify/flows.parts")
}

fn commits_word(n: usize) -> &'static str {
    match (n % 10, n % 100) {
        (1, r) if r != 11 => "коммит",
        (2..=4, r) if !(12..=14).contains(&r) => "коммита",
        _ => "коммитов",
    }
}

/// Install or remove the `stepan-verify-flows` skill links for Claude and Codex.
pub fn sync_skill(home: &Home, on: bool) {
    let targets = flowsync::skill_targets();
    let report = if on { flowsync::install_skill(home, &targets) } else { flowsync::uninstall_skill(home, &targets) };
    for w in report.warnings {
        eprintln!("stepan_verify: skill {}: {w}", flowsync::SKILL_NAME);
    }
}

/// A pass result: JSON (asked for), YAML accepted too; markdown fences are stripped.
fn read_part<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, String> {
    let t = text.trim();
    let t = t.strip_prefix("```json").or_else(|| t.strip_prefix("```yaml")).or_else(|| t.strip_prefix("```")).unwrap_or(t);
    let t = t.strip_suffix("```").unwrap_or(t).trim();
    serde_json::from_str(t).or_else(|je| serde_yaml::from_str(t).map_err(|ye| format!("JSON: {je}; YAML: {ye}")))
}

/// Does this pass result parse as what the pass is supposed to return?
fn check_part(key: &str, text: &str) -> Result<(), String> {
    match key {
        "proofs" => read_part::<flows::ProofsPart>(text).map(|_| ()),
        k if k.starts_with("cover-") => read_part::<CoverPart>(text).map(|_| ()),
        _ => read_part::<FlowProposal>(text).map(|_| ()),
    }
}

/// Routes of entry-point files that were deleted: `app/(g)/auth/reset/page.tsx` → `/auth/reset`.
pub fn deleted_routes(repo: &Path, changed: &[String]) -> Vec<String> {
    changed
        .iter()
        .filter(|f| !repo.join(f).exists() && flowsync::looks_like_entry(f))
        .filter_map(|f| {
            let parts: Vec<&str> = f.split('/').collect();
            let start = parts.iter().position(|p| matches!(*p, "app" | "pages" | "routes" | "screens"))? + 1;
            let mut segs: Vec<&str> = parts[start..].iter().copied().filter(|s| !(s.starts_with('(') && s.ends_with(')'))).collect();
            let last = segs.pop()?;
            let stem = last.split('.').next().unwrap_or(last);
            if !matches!(stem, "page" | "+page" | "index" | "route" | "layout") {
                segs.push(stem);
            }
            let route = format!("/{}", segs.join("/"));
            (route.len() > 1).then_some(route)
        })
        .collect()
}

/// Does a feature talk about this route anywhere (entries or step text)?
fn mentions(f: &Feature, route: &str) -> bool {
    let hit = |t: &str| t.split(|c: char| c.is_whitespace() || "()«»,.;:\"'".contains(c)).any(|w| w == route || w.starts_with(&format!("{route}?")));
    f.entries.iter().any(|e| e == route) || f.steps().any(|s| hit(&s.screen) || hit(&s.action) || hit(&s.result))
}

/// Sync runs: tell the detail agent what changed and that it may drop vanished features.
fn sync_note(changed: &[String]) -> String {
    let list: Vec<&str> = changed.iter().take(80).map(String::as_str).collect();
    format!(
        "\n\nЭто СИНХРОНИЗАЦИЯ после изменений кода. Изменились файлы:\n{}\nОпиши фичи заново по текущему коду (id не меняй). Если фичи больше нет в коде — не пиши её в upsert, а добавь её id в \"remove\".",
        list.iter().map(|f| format!("- {f}")).collect::<Vec<_>>().join("\n")
    )
}

fn repair_prompt(part: &Path, error: &str) -> String {
    format!(
        "[iv3 flows] Файл результата {} не читается: {error}\n\nОткрой его и перепиши валидным JSON (тот же смысл и та же схема, ключи как были). Строки — в двойных кавычках, особенно пути и маршруты со скобками вроде /catalog/[id]. Не меняй ничего, кроме этого файла. В конце одной строкой: исправлено.",
        part.display()
    )
}

fn builds_dir(home: &Home, repo: &Path) -> PathBuf {
    home.state().join("flows").join(flows::repo_key(repo)).join("builds")
}

/// Inventory result: new features come in as drafts; for features already on the
/// map only names and entry points are refreshed unless the agent marked them draft.
pub fn merge_inventory(map: &mut FlowMap, part: &FlowProposal) {
    for a in &part.areas {
        match map.areas.iter_mut().find(|x| x.id == a.id) {
            Some(x) => *x = a.clone(),
            None => map.areas.push(a.clone()),
        }
    }
    for f in &part.upsert {
        match map.features.iter_mut().find(|x| x.id == f.id) {
            Some(x) => {
                if !f.title.is_empty() {
                    x.title = f.title.clone();
                }
                if !f.area.is_empty() {
                    x.area = f.area.clone();
                }
                if !f.summary.is_empty() {
                    x.summary = f.summary.clone();
                }
                union(&mut x.aliases, &f.aliases);
                union(&mut x.entries, &f.entries);
                if f.status == "draft" {
                    x.status = "draft".into();
                }
            }
            None => {
                let mut f = f.clone();
                f.status = "draft".into();
                map.features.push(f);
            }
        }
    }
    map.features.retain(|f| !part.remove.contains(&f.id));
    for i in &part.ignored {
        if !map.ignored.iter().any(|x| x.entry == i.entry) {
            map.ignored.push(i.clone());
        }
    }
    if map.version == 0 {
        map.version = 1;
    }
}

/// Detail result: whole features replace their drafts; entry points are never lost.
pub fn merge_details(map: &mut FlowMap, part: &FlowProposal, allow_remove: bool) {
    let mut part = part.clone();
    for f in &mut part.upsert {
        f.status.clear();
        if let Some(old) = map.feature(&f.id) {
            let mut entries = old.entries.clone();
            union(&mut entries, &f.entries);
            f.entries = entries;
            if f.area.is_empty() {
                f.area = old.area.clone();
            }
        }
    }
    if !allow_remove {
        part.remove.clear();
    }
    map.apply(&part);
}

/// Coverage result: new features, entry points attached to existing ones, ignored entries.
fn merge_cover(map: &mut FlowMap, part: CoverPart) -> usize {
    let mut n = part.ignored.len();
    for a in part.attach {
        if let Some(f) = map.features.iter_mut().find(|f| f.id == a.feature) {
            union(&mut f.entries, &a.entries);
            n += a.entries.len();
        }
    }
    let prop = FlowProposal { upsert: part.upsert, ignored: part.ignored, ..Default::default() };
    n += prop.upsert.len();
    merge_details(map, &prop, false);
    n
}

fn union(into: &mut Vec<String>, from: &[String]) {
    for x in from {
        if !into.contains(x) {
            into.push(x.clone());
        }
    }
}

/// Update mode: features whose step files changed after the last map update get re-described.
fn mark_changed_drafts(map: &mut FlowMap, repo: &Path) {
    let Some(since) = map.updated_at else { return };
    for f in &mut map.features {
        let changed = f.steps().flat_map(|s| &s.files).any(|file| {
            fs::metadata(repo.join(file)).and_then(|m| m.modified()).is_ok_and(|m| chrono::DateTime::<chrono::Utc>::from(m) > since)
        });
        if changed {
            f.status = "draft".into();
        }
    }
}

/// Proofs found now describe the code as it is now: stamp them so later edits show «устарело».
fn stamp_proofs(map: &mut FlowMap) {
    let now = chrono::Utc::now() + chrono::Duration::seconds(2);
    for proof in map.features.iter_mut().flat_map(|f| f.proofs.iter_mut()).filter(|p| p.at.is_none()) {
        proof.at = Some(now);
    }
}

/// Before/after media of sessions in this project (`tmp/sessions/*/media/*`).
fn media_files(home: &Home, repo: &Path) -> Vec<PathBuf> {
    let root = home.root.join("tmp/sessions");
    let Ok(sessions) = fs::read_dir(&root) else { return Vec::new() };
    let repo_s = repo.display().to_string();
    let mut out = Vec::new();
    for s in sessions.flatten() {
        let Ok(runs) = fs::read_dir(s.path().join("media")) else { continue };
        for run in runs.flatten() {
            let manifest = fs::read_to_string(run.path().join("manifest.json")).unwrap_or_default();
            let ctx = fs::read_to_string(run.path().join("context.md")).unwrap_or_default();
            if !manifest.contains(&repo_s) && !ctx.contains(&repo_s) {
                continue;
            }
            let Ok(files) = fs::read_dir(run.path()) else { continue };
            out.extend(files.flatten().map(|f| f.path()).filter(|p| {
                matches!(p.extension().and_then(|e| e.to_str()), Some("png" | "jpg" | "gif" | "mp4" | "webm" | "mov"))
            }));
        }
    }
    out.truncate(200);
    out
}

const RULES: &str = "Правила: не меняй код проекта, ничего не коммить и не пушь, не запускай dev-серверы надолго. Пиши ТОЛЬКО один файл результата (путь ниже) — валидный JSON: все строки в двойных кавычках. Фичи называй как пользователь («Выбор карты Джокера»), а не именами компонентов.";

/// Pass 1: all features and areas, no paths. The checklist keeps it honest.
pub fn inventory_prompt(repo: &Path, checklist: &Path, update: bool) -> String {
    let out = parts_dir(repo).join("inventory.json");
    let map = flows::path(repo);
    let mode = if update {
        format!(
            "Карта уже есть: {}. Перечисли ВСЕ фичи — и существующие (с теми же id), и новые. Существующие, которые не менялись, — без status. Новые и заметно изменившиеся — \"status\": \"draft\". Исчезнувшие — их id в remove.",
            map.display()
        )
    } else {
        "Карты ещё нет: у всех фич \"status\": \"draft\".".to_owned()
    };
    format!(
        r#"[iv3 flows] Опись фич проекта {repo} — проход 1 из нескольких.

Карту флоу строим по шагам, чтобы на большом проекте ничего не потерялось. Сейчас нужна только ОПИСЬ: какие фичи есть у пользователя и в какие области продукта они группируются. Пути и шаги НЕ описывай — их подробно опишут отдельные агенты по областям.

Чек-лист точек входа, найденный автоматически по коду (маршруты, экраны, меню, API), — JSON: {checklist}
Каждая точка входа из чек-листа должна попасть в entries какой-то фичи или в ignored с причиной (служебная, редирект, отладка). Дополнительно найди фичи без собственного маршрута: диалоги, меню, горячие клавиши, уведомления, фоновые действия, команды CLI.

{mode}

Охват: все заметные пользователю фичи (на большом проекте это 30–150). Фича — то, что человек делает или видит ради результата; одна страница может давать несколько фич, одна фича — идти через несколько страниц.

{rules}

Запиши результат в {out}:

{{
  "areas": [{{"id": "rewards", "title": "Награды"}}],
  "upsert": [
    {{
      "id": "rewards.joker-pick",
      "title": "Выбор карты Джокера",
      "area": "rewards",
      "summary": "Пользователь выбирает карту, и она летит в коллекцию",
      "aliases": ["джокер"],
      "entries": ["/rewards/[id]", "apps/web/src/lib/components/RewardsHub/JokerPickDialog.tsx"],
      "status": "draft"
    }}
  ],
  "remove": [],
  "ignored": [{{"entry": "/debug/flags", "reason": "отладочная страница"}}]
}}

В конце коротко ответь: сколько областей и фич, сколько точек входа ушло в ignored."#,
        repo = repo.display(),
        checklist = checklist.display(),
        rules = RULES,
        out = out.display(),
    )
}

/// Pass 2: one area batch, described thoroughly.
pub fn details_prompt(repo: &Path, map: &FlowMap, batch: &flows::Batch) -> String {
    let out = parts_dir(repo).join(format!("{}.json", batch.id));
    let list: Vec<String> = batch
        .features
        .iter()
        .filter_map(|id| map.feature(id))
        .map(|f| format!("- {} · «{}» · {}\n  точки входа: {}", f.id, f.title, f.summary, if f.entries.is_empty() { "—".into() } else { f.entries.join(", ") }))
        .collect();
    format!(
        r#"[iv3 flows] Детали фич проекта {repo}: «{title}» ({n} фич) — проход 2, одна пачка.

Опиши подробно ТОЛЬКО эти фичи (остальные описывают другие агенты):
{list}

Для каждой фичи: 1–3 пути (paths) — цепочка шагов, каждый шаг: screen (где пользователь), action (что делает), result (что видит после), entities (имена компонентов/функций/сторов, через которые идёт шаг), files (пути от корня репозитория). Пройди по коду от точек входа, не угадывай. proofs — только реально существующие тесты/скриншоты/видео в репозитории (reference = путь или команда, covers = id шагов; at не заполняй); нет доказательств — пустой список. Если фича на деле две — можно добавить ещё одну фичу в той же области с новым id. id из списка не меняй.

{rules}

Запиши результат в {out}:

{{
  "upsert": [
    {{
      "id": "<id из списка>",
      "title": "…",
      "area": "…",
      "summary": "…",
      "aliases": [],
      "entries": [],
      "paths": [
        {{
          "id": "from-daily",
          "title": "Из ежедневной карты",
          "steps": [
            {{"id": "open", "screen": "Rewards Hub", "action": "Нажать «Выбрать Джокера»", "result": "Открывается диалог", "entities": ["GlobalDailyCard"], "files": ["apps/web/src/…/GlobalDailyCard.tsx"]}}
          ]
        }}
      ],
      "proofs": [{{"kind": "test", "title": "Диалог выбора", "reference": "apps/web/src/…/JokerPickDialog.test.tsx", "covers": ["open"]}}]
    }}
  ]
}}

kind в proofs: test | screenshot | video | cli | manual. В конце одной строкой: сколько путей и доказательств описал."#,
        repo = repo.display(),
        title = batch.title,
        n = batch.features.len(),
        list = list.join("\n"),
        rules = RULES,
        out = out.display(),
    )
}

/// Pass 3: match tests, stories and media to features and steps.
pub fn proofs_prompt(repo: &Path, map: &FlowMap, inv: &Inventory, media: &[PathBuf]) -> String {
    let out = parts_dir(repo).join("proofs.json");
    let features: Vec<String> = map
        .features
        .iter()
        .map(|f| {
            let steps: Vec<String> = f.paths.iter().flat_map(|p| p.steps.iter().map(|s| format!("{} ({})", s.id, s.action))).collect();
            format!("- {} «{}»: шаги {}; сейчас доказательств: {}", f.id, f.title, steps.join("; "), f.proofs.len())
        })
        .collect();
    let take = |v: &[String], n: usize| v.iter().take(n).cloned().collect::<Vec<_>>().join("\n");
    format!(
        r#"[iv3 flows] Доказательства для карты флоу проекта {repo} — проход 3.

Карта: {map}. Сопоставь доказательства с фичами и шагами: что реально проверяет, что фича работает.

Фичи:
{features}

Тесты ({tests_n}):
{tests}

Storybook ({stories_n}):
{stories}

Скриншоты и видео прошлых сессий ({media_n}):
{media}

Для каждой фичи, у которой есть хоть одно доказательство, дай ПОЛНЫЙ список её доказательств (включая уже указанные, если они верны): kind (test | screenshot | video | cli | manual), title, reference (путь или команда), covers (id шагов; пусто = вся фича). Открой тест и убедись, что он правда проверяет эту фичу. at не заполняй. Фичи без доказательств не перечисляй.

{rules}

Запиши результат в {out}:

{{
  "features": [
    {{"feature": "rewards.joker-pick", "proofs": [{{"kind": "test", "title": "Диалог выбора", "reference": "apps/web/…/JokerPickDialog.test.tsx", "covers": ["pick"]}}]}}
  ]
}}

В конце одной строкой: у скольких фич нашлись доказательства."#,
        repo = repo.display(),
        map = flows::path(repo).display(),
        features = features.join("\n"),
        tests_n = inv.tests.len(),
        tests = if inv.tests.is_empty() { "—".into() } else { take(&inv.tests, 300) },
        stories_n = inv.stories.len(),
        stories = if inv.stories.is_empty() { "—".into() } else { take(&inv.stories, 150) },
        media_n = media.len(),
        media = if media.is_empty() { "—".into() } else { media.iter().map(|m| m.display().to_string()).collect::<Vec<_>>().join("\n") },
        rules = RULES,
        out = out.display(),
    )
}

/// Pass 4: entry points the map still misses.
pub fn cover_prompt(repo: &Path, uncovered: &[&EntryPoint], key: &str) -> String {
    let out = parts_dir(repo).join(format!("{key}.json"));
    let groups = flowscan::by_area(uncovered);
    let list: Vec<String> = groups.iter().map(|(area, items)| format!("{area}:\n  - {}", items.join("\n  - "))).collect();
    format!(
        r#"[iv3 flows] Сверка покрытия карты флоу проекта {repo} — проход 4.

Карта: {map}. Эти точки входа ({n}) не попали ни в одну фичу:
{list}

Для каждой реши одно:
- она часть существующей фичи → в "attach": {{"feature": "<id>", "entries": ["<точка входа>"]}};
- это новая фича → в "upsert": полная фича (id, title, area из существующих областей, summary, entries, paths со steps, proofs) — как в карте;
- она не нужна на карте (служебная, редирект, отладка) → в "ignored": {{"entry": "<точка входа>", "reason": "…"}}.
Не переписывай существующие фичи целиком — только attach.

{rules}

Запиши результат в {out}: {{"upsert": [], "attach": [], "ignored": []}}. В конце одной строкой: сколько привязал, добавил, исключил."#,
        repo = repo.display(),
        map = flows::path(repo).display(),
        n = uncovered.len(),
        list = list.join("\n"),
        rules = RULES,
        out = out.display(),
    )
}

// ------------------------------------------------------------------ rendering

fn freshness_tone(f: Freshness, p: &Palette) -> (Tone, &'static str) {
    match f {
        Freshness::Fresh => (p.green, "доказано"),
        Freshness::Stale => (p.amber, "устарело"),
        Freshness::Missing => (p.red, "нет доказательств"),
    }
}

fn proof_label(k: ProofKind) -> (&'static str, &'static str) {
    match k {
        ProofKind::Test => ("тест", "icons/flask-conical.svg"),
        ProofKind::Screenshot => ("скриншот", "icons/image.svg"),
        ProofKind::Video => ("видео", "icons/video.svg"),
        ProofKind::Cli => ("сценарий CLI", "icons/terminal.svg"),
        ProofKind::Manual => ("руками", "icons/hand.svg"),
    }
}

impl Render for FlowsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let p = Palette::of(&theme);
        v_flex()
            .id("flows")
            .size_full()
            .child(self.render_header(&p, &theme, cx))
            .child(div().flex_1().min_h_0().relative().child(self.render_body(&p, &theme, cx)))
    }
}

impl FlowsView {
    fn render_header(&self, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let stats = self.map.as_ref().map(|m| {
            let missing = self.fresh.iter().filter(|f| **f == Freshness::Missing).count();
            let stale = self.fresh.iter().filter(|f| **f == Freshness::Stale).count();
            let paths: usize = m.features.iter().map(|f| f.paths.len()).sum();
            (m.areas.len().max(1), m.features.len(), paths, missing, stale)
        });
        let pending = self.pending().len();
        let has_map = self.map.is_some();
        let building = self.is_building();
        v_flex()
            .px(px(28.))
            .pt(px(22.))
            .pb(px(12.))
            .gap(px(10.))
            .border_b_1()
            .border_color(p.border)
            .child(
                h_flex()
                    .gap_3()
                    .items_end()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(px(4.))
                            .child(ui::eyebrow("Что есть → как дойти → чем доказано", p, theme))
                            .child(ui::page_title("Флоу", p))
                            .when_some(stats, |d, (areas, features, paths, missing, stale)| {
                                d.child(
                                    h_flex()
                                        .gap(px(14.))
                                        .text_size(px(13.))
                                        .text_color(p.muted)
                                        .child(format!("{areas} обл. · {features} фич · {paths} путей"))
                                        .when(missing > 0, |d| d.child(ui::dot_badge(format!("без доказательств: {missing}"), p.red)))
                                        .when(stale > 0, |d| d.child(ui::dot_badge(format!("устарело: {stale}"), p.amber)))
                                        .when(missing + stale == 0 && features > 0, |d| d.child(ui::dot_badge("всё доказано", p.green))),
                                )
                            }),
                    )
                    .when(has_map, |d| d.child(div().w(px(260.)).child(Input::new(&self.search).small())))
                    .when(pending > 0, |d| {
                        d.child(
                            PressButton::new("flows-proposals")
                                .small()
                                .icon(Icon::empty().path("icons/list-checks.svg"))
                                .label(format!("Предложения · {pending}"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.selected = if this.selected == Some(Selection::Proposals) { None } else { Some(Selection::Proposals) };
                                    cx.notify();
                                })),
                        )
                    })
                    .when(self.project.is_some(), |d| {
                        d.child(
                            PressButton::new("flows-build")
                                .small()
                                .map(|b| if has_map { b } else { b.primary() })
                                .icon(IconName::RefreshCw)
                                .loading(building)
                                .label(match (building, has_map) {
                                    (true, _) => "Карта строится…",
                                    (false, true) => "Обновить карту",
                                    (false, false) => "Построить карту",
                                })
                                .on_click(cx.listener(|this, _, _, cx| this.build(cx))),
                        )
                    })
                    .when(building, |d| {
                        d.child(
                            PressButton::new("flows-stop")
                                .small()
                                .icon(IconName::Close)
                                .label("Остановить")
                                .on_click(cx.listener(|this, _, _, cx| this.stop_build(cx))),
                        )
                    }),
            )
            .when_some(self.lag.clone().filter(|_| !building), |d, lag| {
                d.child(
                    h_flex()
                        .gap(px(10.))
                        .px(px(12.))
                        .py(px(8.))
                        .rounded(px(8.))
                        .border_1()
                        .border_color(ui::mix(p.amber.solid, p.border, 0.5))
                        .bg(ui::mix(p.amber.soft, p.surface, 0.6))
                        .child(Icon::new(IconName::TriangleAlert).small().text_color(p.amber.text))
                        .child(
                            div().flex_1().min_w_0().text_size(px(13.)).text_color(p.fg).child(format!(
                                "Карта отстала{} · изменено файлов карты: {}",
                                if lag.commits > 0 { format!(" на {} {}", lag.commits, commits_word(lag.commits)) } else { " (незакоммиченные правки)".into() },
                                lag.relevant.len()
                            )),
                        )
                        .child(ui::mono(lag.relevant.iter().take(3).cloned().collect::<Vec<_>>().join(", "), theme).max_w(px(360.)).truncate().text_color(p.muted))
                        .child(
                            PressButton::new("flows-sync")
                                .primary()
                                .small()
                                .icon(IconName::RefreshCw)
                                .label("Синхронизировать")
                                .on_click(cx.listener(|this, _, _, cx| this.sync(cx))),
                        ),
                )
            })
            .child(self.render_projects(p, theme, cx))
    }

    fn render_projects(&self, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let mut list: Vec<PathBuf> = self.projects.iter().take(10).cloned().collect();
        if let Some(cur) = &self.project {
            if !list.contains(cur) {
                list.insert(0, cur.clone());
            }
        }
        list.dedup_by(|a, b| a.file_name() == b.file_name());
        list.truncate(9);
        h_flex().gap(px(6.)).flex_wrap().children(list.into_iter().enumerate().map(|(i, dir)| {
            let active = self.project.as_ref() == Some(&dir);
            let has = flows::path(&dir).exists();
            let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| dir.display().to_string());
            let pick = dir.clone();
            h_flex()
                .id(SharedString::from(format!("flows-project-{i}")))
                .h(px(26.))
                .px(px(10.))
                .gap(px(6.))
                .rounded(px(13.))
                .border_1()
                .cursor_pointer()
                .text_size(px(12.5))
                .map(|d| {
                    if active {
                        d.bg(p.ink).border_color(p.ink).text_color(theme.background)
                    } else {
                        d.bg(p.surface).border_color(p.border).text_color(p.fg).hover(|s| s.border_color(p.border_strong))
                    }
                })
                .on_click(cx.listener(move |this, _, _, cx| this.select_project(pick.clone(), cx)))
                .when(has, |d| d.child(div().size(px(6.)).rounded_full().bg(if active { p.accent.solid } else { p.green.solid })))
                .child(name)
        }))
    }

    fn render_body(&mut self, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        if self.project.is_none() {
            return empty(
                "Нет проекта",
                "Проекты появляются из сессий агентов. Запусти сессию в папке проекта — и здесь можно будет построить его карту флоу.",
                p,
            )
            .into_any_element();
        }
        if self.pipeline.is_some() && self.map.is_none() {
            // Nothing merged yet: the pipeline is the page.
            return v_flex()
                .size_full()
                .items_center()
                .pt(px(60.))
                .child(div().w(px(760.)).child(self.render_pipeline(p, theme, cx)))
                .into_any_element();
        }
        let Some(map) = self.map.clone() else {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap(px(14.))
                .child(empty(
                    "У проекта ещё нет карты флоу",
                    "Агент изучит маршруты, экраны, меню и тесты и опишет фичи словами пользователя: как до каждой дойти и что доказывает, что она работает. Карта ляжет в .stepan_verify/flows.yaml — её будут читать и агенты.",
                    p,
                ))
                .when_some(self.error.clone().or(self.build_error.clone()), |d, e| d.child(ui::badge(e, p.red)))
                .child(
                    PressButton::new("flows-build-empty")
                        .primary()
                        .icon(Icon::empty().path("icons/route.svg"))
                        .label("Построить карту")
                        .on_click(cx.listener(|this, _, _, cx| this.build(cx))),
                )
                .into_any_element();
        };
        let query = self.query.clone();
        let keep = move |f: &Feature| matches(f, &query);
        let (vw, _) = self.viewport_size();
        let lay = layout(&map, &keep, columns_for(vw));
        if !self.placed && self.viewport.get().is_some() {
            self.place();
        }
        let strip = self.pipeline.is_some().then(|| self.render_pipeline(p, theme, cx));
        v_flex()
            .size_full()
            .when_some(strip, |d, strip| d.child(div().px(px(20.)).pt(px(12.)).pb(px(8.)).child(strip)))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(div().flex_1().min_w_0().h_full().relative().child(self.render_canvas(&map, &lay, p, theme, cx)))
                    .when_some(self.selected.clone(), |d, sel| d.child(self.render_panel(&map, sel, p, theme, cx))),
            )
            .into_any_element()
    }

    /// Pipeline progress: stages, area batches, coverage.
    fn render_pipeline(&self, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let Some(pipe) = &self.pipeline else { return div().into_any_element() };
        let running = pipe.running();
        let secs = pipe.finished_at.map_or(pipe.started.elapsed().as_secs(), |f| (f - pipe.started_at).num_seconds().max(0) as u64);
        let done_batches = pipe.batches.iter().filter(|b| matches!(b.state, BatchState::Done(_))).count();
        let failed = pipe.batches.iter().filter(|b| matches!(b.state, BatchState::Failed(_))).count();
        let title = match (running, pipe.stopped, &pipe.error, pipe.stage) {
            (_, _, Some(_), _) => "Карта не достроена",
            (_, true, _, _) => "Построение остановлено",
            (true, ..) if pipe.update => "Обновляю карту флоу",
            (true, ..) => "Строю карту флоу",
            _ => "Карта построена",
        };
        let stage_chip = |stage: Stage| {
            let order = |s: Stage| Stage::PIPELINE.iter().position(|x| *x == s).unwrap_or(4);
            let (cur, me) = (order(pipe.stage), order(stage));
            let state = if pipe.stage == Stage::Done || me < cur {
                if stage == Stage::Details && failed > 0 { 3 } else { 2 }
            } else if me == cur {
                if pipe.error.is_some() { 3 } else if running { 1 } else { 0 }
            } else {
                0
            };
            let label = match stage {
                Stage::Details if !pipe.batches.is_empty() => format!("Детали · {done_batches} из {}", pipe.batches.len()),
                Stage::Coverage if pipe.stage == Stage::Done => {
                    if pipe.uncovered.is_empty() { "Сверка · всё покрыто".into() } else { format!("Сверка · не покрыто {}", pipe.uncovered.len()) }
                }
                s => s.title().to_owned(),
            };
            let (bg, fg, icon): (Hsla, Hsla, AnyElement) = match state {
                1 => (p.blue.soft, p.blue.text, Spinner::new().xsmall().color(p.blue.solid).into_any_element()),
                2 => (p.green.soft, p.green.text, Icon::new(IconName::CircleCheck).xsmall().text_color(p.green.solid).into_any_element()),
                3 => (p.red.soft, p.red.text, Icon::new(IconName::TriangleAlert).xsmall().text_color(p.red.solid).into_any_element()),
                _ => (p.surface2, p.faint, div().size(px(10.)).rounded_full().border_1().border_dashed().border_color(p.faint).into_any_element()),
            };
            h_flex().h(px(26.)).px(px(9.)).gap(px(6.)).rounded(px(13.)).bg(bg).text_size(px(12.)).text_color(fg).child(icon).child(label)
        };
        let arrow = || div().text_size(px(12.)).text_color(p.faint).child("→");
        let jobs: Vec<(String, u64, String)> = pipe
            .jobs
            .iter()
            .map(|j| {
                let who = match j.key.as_str() {
                    "inventory" => "Опись".to_owned(),
                    "proofs" => "Доказательства".to_owned(),
                    k if k.starts_with("cover-") => "Сверка".to_owned(),
                    k => pipe.batches.iter().find(|b| b.batch.id == k).map(|b| b.batch.title.clone()).unwrap_or_else(|| k.to_owned()),
                };
                (who, j.started.elapsed().as_secs(), j.steps.last().cloned().unwrap_or_else(|| "запускается…".into()))
            })
            .collect();
        let tiles = pipe.batches.iter().map(|run| {
            let id = run.batch.id.clone();
            let (bg, border, badge): (Hsla, Hsla, AnyElement) = match &run.state {
                BatchState::Waiting => (p.surface, p.border, ui::mono("ждёт", theme).text_color(p.faint).into_any_element()),
                BatchState::Running => (
                    ui::mix(p.blue.soft, p.surface, 0.5),
                    ui::mix(p.blue.solid, p.border, 0.5),
                    h_flex().gap(px(4.)).child(Spinner::new().xsmall().color(p.blue.solid)).child(ui::mono("идёт", theme).text_color(p.blue.text)).into_any_element(),
                ),
                BatchState::Done(secs) => (p.surface, ui::mix(p.green.solid, p.border, 0.4), ui::mono(format!("✓ {}", crate::live_card::mmss(*secs)), theme).text_color(p.green.text).into_any_element()),
                BatchState::Failed(_) => (
                    ui::mix(p.red.soft, p.surface, 0.5),
                    ui::mix(p.red.solid, p.border, 0.5),
                    PressButton::new(SharedString::from(format!("flows-retry-{id}")))
                        .xsmall()
                        .icon(IconName::RefreshCw)
                        .label("Повторить")
                        .on_click(cx.listener(move |this, _, _, cx| this.retry_batch(id.clone(), cx)))
                        .into_any_element(),
                ),
            };
            let error = match &run.state {
                BatchState::Failed(e) => Some(e.clone()),
                _ => None,
            };
            v_flex()
                .w(px(170.))
                .p(px(8.))
                .gap(px(4.))
                .rounded(px(8.))
                .border_1()
                .border_color(border)
                .bg(bg)
                .child(div().truncate().text_size(px(12.5)).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).child(run.batch.title.clone()))
                .child(h_flex().gap(px(6.)).child(ui::mono(format!("{} фич", run.batch.features.len()), theme).text_color(p.muted)).child(div().flex_1()).child(badge))
                .when_some(error, |d, e| d.child(div().text_size(px(11.)).text_color(p.red.text).line_height(px(14.)).max_h(px(28.)).overflow_hidden().child(e)))
        });
        let uncovered = flowscan::by_area(&pipe.uncovered.iter().collect::<Vec<_>>());
        let show_cover = pipe.stage == Stage::Done && !pipe.uncovered.is_empty();
        ui::frame("flows-pipeline", p)
            .child(
                ui::inner(p)
                    .p(px(14.))
                    .gap(px(10.))
                    .child(
                        h_flex()
                            .gap(px(10.))
                            .when(running, |d| d.child(Spinner::new().small().color(p.blue.solid)))
                            .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).child(title))
                            .child(ui::mono(format!("сборка #{} · {}", pipe.n, pipe.who), theme).text_color(p.faint))
                            .child(div().flex_1())
                            .child(ui::mono(crate::live_card::mmss(secs), theme).text_color(if running { p.blue.text } else { p.muted }))
                            .when(!running, |d| {
                                d.child(
                                    PressButton::new("flows-pipeline-close")
                                        .ghost()
                                        .xsmall()
                                        .icon(IconName::Close)
                                        .tooltip("Скрыть")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.pipeline = None;
                                            cx.notify();
                                        })),
                                )
                            }),
                    )
                    .child(
                        h_flex()
                            .gap(px(6.))
                            .flex_wrap()
                            .child(stage_chip(Stage::Inventory))
                            .child(arrow())
                            .child(stage_chip(Stage::Details))
                            .child(arrow())
                            .child(stage_chip(Stage::Proofs))
                            .child(arrow())
                            .child(stage_chip(Stage::Coverage)),
                    )
                    .when(pipe.stage == Stage::Inventory && pipe.inventory.is_none() && running, |d| {
                        d.child(div().text_size(px(12.5)).text_color(p.muted).child("Ищу точки входа по коду: маршруты, экраны, меню, тесты…"))
                    })
                    .when_some(pipe.inventory.as_ref(), |d, inv| {
                        d.child(
                            div().text_size(px(12.5)).text_color(p.muted).child(format!(
                                "Чек-лист покрытия: {} точек входа · {} тестов{}",
                                inv.entries.len(),
                                inv.tests.len(),
                                if inv.stacks.is_empty() { String::new() } else { format!(" · {}", inv.stacks.join(", ")) }
                            )),
                        )
                    })
                    .children(jobs.into_iter().map(|(who, secs, step)| {
                        h_flex()
                            .gap(px(8.))
                            .child(Spinner::new().xsmall().color(p.blue.solid))
                            .child(div().w(px(160.)).truncate().text_size(px(12.5)).text_color(p.fg).child(who))
                            .child(ui::mono(crate::live_card::mmss(secs), theme).text_color(p.blue.text))
                            .child(ui::mono(step, theme).flex_1().min_w_0().truncate().text_color(p.muted))
                    }))
                    .when(!pipe.batches.is_empty(), |d| d.child(h_flex().flex_wrap().gap(px(8.)).children(tiles)))
                    .when(show_cover, |d| {
                        d.child(
                            v_flex()
                                .gap(px(6.))
                                .p(px(10.))
                                .rounded(px(8.))
                                .bg(ui::mix(p.amber.soft, p.surface, 0.5))
                                .child(
                                    h_flex()
                                        .gap(px(8.))
                                        .child(div().flex_1().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(p.amber.text).child(format!(
                                            "Не покрыто картой: {} точек входа",
                                            pipe.uncovered.len()
                                        )))
                                        .child(
                                            PressButton::new("flows-cover-more")
                                                .xsmall()
                                                .icon(IconName::Plus)
                                                .label("Дополнить")
                                                .on_click(cx.listener(|this, _, _, cx| this.cover_more(cx))),
                                        ),
                                )
                                .children(uncovered.into_iter().take(8).map(|(area, items)| {
                                    let more = items.len().saturating_sub(4);
                                    v_flex()
                                        .gap(px(2.))
                                        .child(ui::mono(area, theme).text_color(p.muted))
                                        .children(items.into_iter().take(4).map(|i| ui::mono(i, theme).truncate().text_color(p.fg)))
                                        .when(more > 0, |d| d.child(ui::mono(format!("… ещё {more}"), theme).text_color(p.faint)))
                                })),
                        )
                    })
                    .when(pipe.stage == Stage::Done && pipe.uncovered.is_empty() && pipe.error.is_none() && !pipe.stopped, |d| {
                        d.child(div().text_size(px(12.5)).text_color(p.green.text).child("Все точки входа из чек-листа есть на карте (или исключены с причиной)."))
                    })
                    .when_some(pipe.error.clone(), |d, e| d.child(ui::badge(e, p.red))),
            )
            .into_any_element()
    }

    fn render_canvas(&self, map: &FlowMap, lay: &Layout, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let v = self.view;
        let z = v.zoom;
        let sx = move |x: f32| v.x + x * z;
        let sy = move |y: f32| v.y + y * z;
        let zl = move |l: f32| l * z;
        let text = z >= 0.32;
        let detail = z >= 0.55;
        let mono = theme.mono_font_family.clone();
        let selected = match &self.selected {
            Some(Selection::Feature(i)) => Some(*i),
            _ => None,
        };
        // Steps the newest sessions went through: marked on the canvas.
        let mut touched: HashMap<(String, String), Vec<String>> = HashMap::new();
        for s in self.sessions.iter().take(3) {
            for t in &s.touched {
                touched.entry((t.feature.clone(), t.path.clone())).or_default().extend(t.steps.iter().cloned());
            }
        }

        let lines: Vec<([(f32, f32); 4], Hsla, bool)> = lay
            .links
            .iter()
            .map(|&(a, b, kind)| {
                let mid = (a.0 + b.0) / 2.;
                let curve = [(sx(a.0), sy(a.1)), (sx(mid), sy(a.1)), (sx(mid), sy(b.1)), (sx(b.0), sy(b.1))];
                let (color, dashed) = match kind {
                    LinkKind::Path => (p.border_strong, false),
                    LinkKind::Proof => (p.green.solid.opacity(0.7), false),
                    LinkKind::Missing => (p.red.solid.opacity(0.6), true),
                };
                (curve, color, dashed)
            })
            .collect();
        let viewport = self.viewport.clone();
        let dot = p.border_strong.opacity(0.35);
        let grid = (v.x, v.y, z);
        // Dot grid under everything (it also measures the viewport).
        let grid_layer = canvas(
            move |bounds, _, _| viewport.set(Some(bounds)),
            move |bounds, _, window, _| {
                let o = bounds.origin;
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
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();
        // Links above the area frames, below the cards.
        let links = canvas(
            move |_, _, _| lines,
            move |bounds, lines, window, _| {
                let o = bounds.origin;
                let pt = |(x, y): (f32, f32)| point(px(x), px(y)) + o;
                for (c, color, dashed) in lines {
                    let mut path = PathBuilder::stroke(px(1.5));
                    if dashed {
                        path = path.dash_array(&[px(4.), px(3.)]);
                    }
                    path.move_to(pt(c[0]));
                    path.cubic_bezier_to(pt(c[3]), pt(c[1]), pt(c[2]));
                    if let Ok(path) = path.build() {
                        window.paint_path(path, color);
                    }
                }
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();

        let areas = lay.areas.iter().map(|(title, r)| {
            div()
                .absolute()
                .left(px(sx(r.x)))
                .top(px(sy(r.y)))
                .w(px(zl(r.w)))
                .h(px(zl(r.h)))
                .rounded(px(zl(14.).max(4.)))
                .border_1()
                .border_color(p.border)
                .bg(ui::mix(p.surface, p.sunken, 0.35))
                .when(text, |d| {
                    d.child(
                        h_flex()
                            .px(px(zl(AREA_PAD)))
                            .h(px(zl(AREA_HEAD)))
                            .gap(px(zl(10.)))
                            .child(div().text_size(px(zl(15.))).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).child(title.clone()))
                            .when(detail, |d| {
                                d.child(div().flex_1())
                                    .child(div().w(px(zl(PATH_W))).font_family(mono.clone()).text_size(px(zl(10.))).text_color(p.faint).child("КАК ДОЙТИ"))
                                    .child(div().w(px(zl(PROOF_W + COL_GAP * 0.2))).font_family(mono.clone()).text_size(px(zl(10.))).text_color(p.faint).child("ЧЕМ ДОКАЗАНО"))
                            }),
                    )
                })
        });

        let features = lay.features.iter().map(|&(fi, r)| {
            let f = &map.features[fi];
            let fresh = self.fresh.get(fi).copied().unwrap_or(Freshness::Missing);
            let (tone, label) = if f.status == "draft" { (p.blue, "описывается") } else { freshness_tone(fresh, p) };
            let is_sel = selected == Some(fi);
            let was_touched = touched.keys().any(|(feat, _)| feat == &f.id);
            div()
                .id(SharedString::from(format!("flow-feature-{fi}")))
                .absolute()
                .left(px(sx(r.x)))
                .top(px(sy(r.y)))
                .w(px(zl(r.w)))
                .h(px(zl(r.h)))
                .rounded(px(zl(10.).max(3.)))
                .border_1()
                .border_color(if is_sel { p.ink } else { ui::mix(p.accent.solid, p.border, 0.35) })
                .bg(p.surface)
                .shadow(ui::card_shadow())
                .overflow_hidden()
                .px(px(zl(12.)))
                .py(px(zl(10.)))
                .flex()
                .flex_col()
                .gap(px(zl(5.)))
                .cursor_pointer()
                .hover(|s| s.border_color(p.border_strong))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if this.dragged {
                        return;
                    }
                    this.selected = Some(Selection::Feature(fi));
                    cx.notify();
                }))
                .when(text, |d| {
                    d.child(
                        h_flex()
                            .gap(px(zl(6.)))
                            .child(div().flex_1().min_w_0().text_size(px(zl(14.))).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).line_height(px(zl(18.))).child(f.title.clone()))
                            .when(was_touched, |d| d.child(div().size(px(zl(8.))).rounded_full().bg(p.blue.solid))),
                    )
                    .when(detail && !f.summary.is_empty(), |d| {
                        d.child(div().text_size(px(zl(12.))).text_color(p.muted).line_height(px(zl(16.))).max_h(px(zl(32.))).overflow_hidden().child(f.summary.clone()))
                    })
                    .child(div().flex_1())
                    .child(
                        h_flex()
                            .gap(px(zl(6.)))
                            .child(div().px(px(zl(6.))).rounded(px(zl(4.))).bg(tone.soft).text_size(px(zl(11.))).text_color(tone.text).child(label))
                            .when(detail, |d| d.child(div().font_family(mono.clone()).text_size(px(zl(10.5))).text_color(p.faint).child(format!("{} пут.", f.paths.len())))),
                    )
                })
        });

        let paths = lay.paths.iter().map(|&(fi, pi, r)| {
            let f = &map.features[fi];
            let path = &f.paths[pi];
            let hot = touched.get(&(f.id.clone(), path.id.clone())).cloned().unwrap_or_default();
            div()
                .id(SharedString::from(format!("flow-path-{fi}-{pi}")))
                .absolute()
                .left(px(sx(r.x)))
                .top(px(sy(r.y)))
                .w(px(zl(r.w)))
                .h(px(zl(r.h)))
                .rounded(px(zl(10.).max(3.)))
                .border_1()
                .border_color(if hot.is_empty() { p.border } else { ui::mix(p.blue.solid, p.border, 0.5) })
                .bg(p.surface)
                .shadow(ui::card_shadow())
                .overflow_hidden()
                .cursor_pointer()
                .hover(|s| s.border_color(p.border_strong))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if this.dragged {
                        return;
                    }
                    this.selected = Some(Selection::Feature(fi));
                    cx.notify();
                }))
                .when(text, |d| {
                    d.child(
                        h_flex()
                            .h(px(zl(PATH_HEAD)))
                            .px(px(zl(12.)))
                            .gap(px(zl(6.)))
                            .border_b_1()
                            .border_color(p.border)
                            .child(Icon::empty().path("icons/footprints.svg").size(px(zl(13.))).text_color(p.muted))
                            .child(div().flex_1().min_w_0().truncate().text_size(px(zl(12.5))).font_weight(FontWeight::MEDIUM).text_color(p.fg).child(if path.title.is_empty() { "Путь".to_owned() } else { path.title.clone() })),
                    )
                    .children(path.steps.iter().enumerate().map(|(si, s)| {
                        let lit = hot.contains(&s.id);
                        h_flex()
                            .h(px(zl(STEP_H)))
                            .px(px(zl(12.)))
                            .gap(px(zl(9.)))
                            .when(si > 0, |d| d.border_t_1().border_color(ui::mix(p.border, p.surface, 0.6)))
                            .when(lit, |d| d.bg(ui::mix(p.blue.soft, p.surface, 0.6)))
                            .child(
                                div()
                                    .size(px(zl(20.)))
                                    .flex_shrink_0()
                                    .rounded_full()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .bg(if lit { p.blue.solid } else { p.sunken })
                                    .font_family(mono.clone())
                                    .text_size(px(zl(10.5)))
                                    .text_color(if lit { gpui_kit::white() } else { p.muted })
                                    .child((si + 1).to_string()),
                            )
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().truncate().text_size(px(zl(10.5))).text_color(p.faint).child(s.screen.clone()))
                                    .child(div().truncate().text_size(px(zl(12.5))).text_color(p.fg).child(if s.result.is_empty() { s.action.clone() } else { format!("{} → {}", s.action, s.result) })),
                            )
                    }))
                })
        });

        let proofs = lay.proofs.iter().map(|&(fi, slot, r)| {
            let f = &map.features[fi];
            let base = div()
                .id(SharedString::from(format!("flow-proof-{fi}-{slot:?}")))
                .absolute()
                .left(px(sx(r.x)))
                .top(px(sy(r.y)))
                .w(px(zl(r.w)))
                .h(px(zl(r.h)))
                .rounded(px(zl(10.).max(3.)))
                .border_1()
                .overflow_hidden()
                .px(px(zl(12.)))
                .py(px(zl(9.)))
                .flex()
                .flex_col()
                .gap(px(zl(4.)))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| {
                    if this.dragged {
                        return;
                    }
                    this.selected = Some(Selection::Feature(fi));
                    cx.notify();
                }));
            match slot {
                ProofSlot::Missing => base
                    .border_dashed()
                    .border_color(ui::mix(p.red.solid, p.border, 0.55))
                    .bg(ui::mix(p.red.soft, p.sunken, 0.45))
                    .when(text, |d| {
                        d.child(div().text_size(px(zl(12.5))).font_weight(FontWeight::SEMIBOLD).text_color(p.red.text).child("Нет доказательств"))
                            .when(detail, |d| d.child(div().text_size(px(zl(11.5))).text_color(p.muted).child("ни теста, ни скриншота — работает ли фича, неизвестно")))
                    }),
                ProofSlot::Proof(qi) => {
                    let proof = &f.proofs[qi];
                    let (kind, icon) = proof_label(proof.kind);
                    let fresh = self.fresh.get(fi).copied().unwrap_or(Freshness::Missing);
                    let stale = fresh == Freshness::Stale;
                    base.border_color(if stale { ui::mix(p.amber.solid, p.border, 0.5) } else { ui::mix(p.green.solid, p.border, 0.45) })
                        .bg(p.surface)
                        .shadow(ui::card_shadow())
                        .when(text, |d| {
                            d.child(
                                h_flex()
                                    .gap(px(zl(6.)))
                                    .child(Icon::empty().path(icon).size(px(zl(13.))).text_color(if stale { p.amber.text } else { p.green.text }))
                                    .child(div().font_family(mono.clone()).text_size(px(zl(10.))).text_color(p.faint).child(kind.to_uppercase()))
                                    .child(div().flex_1())
                                    .when(stale, |d| d.child(div().px(px(zl(5.))).rounded(px(zl(4.))).bg(p.amber.soft).text_size(px(zl(10.5))).text_color(p.amber.text).child("устарело"))),
                            )
                            .child(div().truncate().text_size(px(zl(12.5))).text_color(p.fg).child(if proof.title.is_empty() { proof.reference.clone() } else { proof.title.clone() }))
                            .when(detail, |d| d.child(div().truncate().font_family(mono.clone()).text_size(px(zl(10.5))).text_color(p.faint).child(proof.reference.clone())))
                        })
                }
            }
        });

        let world = (lay.width, lay.height);
        let controls = h_flex()
            .absolute()
            .top(px(10.))
            .right(px(12.))
            .gap(px(2.))
            .p(px(3.))
            .rounded(px(8.))
            .border_1()
            .border_color(p.border)
            .bg(p.surface)
            .shadow(ui::card_shadow())
            .child(PressButton::new("flows-zoom-out").ghost().xsmall().icon(IconName::Minus).on_click(cx.listener(|this, _, _, cx| this.zoom_center(1. / 1.25, cx))))
            .child(
                div()
                    .id("flows-zoom-100")
                    .px(px(6.))
                    .cursor_pointer()
                    .font_family(mono.clone())
                    .text_size(px(11.5))
                    .text_color(p.muted)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.place();
                        cx.notify();
                    }))
                    .child(format!("{:.0}%", z * 100.)),
            )
            .child(PressButton::new("flows-zoom-in").ghost().xsmall().icon(IconName::Plus).on_click(cx.listener(|this, _, _, cx| this.zoom_center(1.25, cx))))
            .child(PressButton::new("flows-fit").ghost().xsmall().icon(Icon::empty().path("icons/scan.svg")).label("Вписать").on_click(cx.listener(move |this, _, _, cx| this.fit(world, cx))));
        let legend = h_flex()
            .absolute()
            .left(px(12.))
            .bottom(px(12.))
            .gap(px(12.))
            .px(px(10.))
            .py(px(6.))
            .rounded(px(9.))
            .border_1()
            .border_color(p.border)
            .bg(p.surface.opacity(0.94))
            .text_size(px(11.))
            .text_color(p.muted)
            .child(h_flex().gap(px(4.)).child(div().w(px(14.)).h(px(2.)).bg(p.green.solid)).child("доказывает"))
            .child(h_flex().gap(px(4.)).child(div().w(px(14.)).h(px(0.)).border_t_1().border_dashed().border_color(p.red.solid)).child("нет доказательства"))
            .child(h_flex().gap(px(4.)).child(div().size(px(8.)).rounded_full().bg(p.blue.solid)).child("шли недавние сессии"))
            .child(ui::mono("тащи — двигать · ⌘ + колесо — масштаб · двойной клик — вписать", theme).text_size(px(10.5)).text_color(p.faint));
        let grabbing = self.drag.is_some_and(|d| d.moved);

        div()
            .id("flows-canvas")
            .relative()
            .size_full()
            .bg(p.sunken)
            .overflow_hidden()
            .cursor(if grabbing { CursorStyle::ClosedHand } else { CursorStyle::OpenHand })
            .on_mouse_down(MouseButton::Left, cx.listener(|this, e: &MouseDownEvent, _, cx| {
                this.drag = Some(canvas::Drag::new((f32::from(e.position.x), f32::from(e.position.y)), this.view));
                this.dragged = false;
                cx.notify();
            }))
            .on_mouse_down(MouseButton::Middle, cx.listener(|this, e: &MouseDownEvent, _, cx| {
                this.drag = Some(canvas::Drag::new((f32::from(e.position.x), f32::from(e.position.y)), this.view));
                this.dragged = false;
                cx.notify();
            }))
            .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, _, cx| {
                let Some(mut drag) = this.drag else { return };
                if e.pressed_button.is_none() {
                    this.drag = None;
                    cx.notify();
                    return;
                }
                this.view = drag.update((f32::from(e.position.x), f32::from(e.position.y)));
                this.dragged = drag.moved;
                this.drag = Some(drag);
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
                cx.stop_propagation();
                let delta = match e.delta {
                    ScrollDelta::Pixels(d) => (f32::from(d.x), f32::from(d.y)),
                    ScrollDelta::Lines(l) => (l.x * 24., l.y * 24.),
                };
                let at = this.local(e.position);
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
                this.view = this.view.zoom_at(1. + e.delta, at);
                cx.notify();
            }))
            .on_click(cx.listener(move |this, e: &ClickEvent, _, cx| {
                if this.dragged {
                    return;
                }
                if e.click_count() >= 2 {
                    this.fit(world, cx);
                }
            }))
            .child(grid_layer)
            .children(areas)
            .child(links)
            .children(features)
            .children(paths)
            .children(proofs)
            .when(lay.features.is_empty(), |d| {
                d.child(div().absolute().top(px(60.)).left(px(40.)).text_size(px(13.)).text_color(p.muted).child("Ничего не нашлось"))
            })
            .when_some(self.build_error.clone(), |d, e| d.child(div().absolute().top(px(10.)).left(px(12.)).child(ui::badge(e, p.red))))
            .child(legend)
            .child(controls)
    }

    fn render_panel(&self, map: &FlowMap, sel: Selection, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let body: AnyElement = match sel {
            Selection::Proposals => self.render_proposals(p, theme, cx).into_any_element(),
            Selection::Feature(fi) => match map.features.get(fi) {
                Some(f) => self.render_feature(fi, f, p, theme, cx).into_any_element(),
                None => div().into_any_element(),
            },
        };
        v_flex()
            .id("flows-panel")
            .w(px(400.))
            .h_full()
            .flex_shrink_0()
            .border_l_1()
            .border_color(p.border)
            .bg(p.surface)
            .child(
                h_flex()
                    .h(px(40.))
                    .px(px(14.))
                    .border_b_1()
                    .border_color(p.border)
                    .child(div().flex_1())
                    .child(PressButton::new("flows-panel-close").ghost().xsmall().icon(IconName::Close).on_click(cx.listener(|this, _, _, cx| {
                        this.selected = None;
                        cx.notify();
                    }))),
            )
            .child(div().id("flows-panel-scroll").flex_1().min_h_0().overflow_y_scroll().child(body))
    }

    fn render_feature(&self, fi: usize, f: &Feature, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let repo = self.project.clone().unwrap_or_default();
        let fresh = self.fresh.get(fi).copied().unwrap_or(Freshness::Missing);
        let (tone, label) = freshness_tone(fresh, p);
        let mono = theme.mono_font_family.clone();
        let recent: Vec<&SessionFlows> = self.sessions.iter().filter(|s| s.touched.iter().any(|t| t.feature == f.id)).take(6).collect();
        let file_link = |id: String, rel: String| {
            let full = repo.join(&rel);
            div()
                .id(SharedString::from(id))
                .truncate()
                .font_family(mono.clone())
                .text_size(px(11.))
                .text_color(p.blue.text)
                .cursor_pointer()
                .hover(|s| s.underline())
                .on_click(move |_, window, cx| crate::preview::open_in_app(full.clone(), None, window, cx))
                .child(rel)
        };
        v_flex()
            .p(px(16.))
            .gap(px(14.))
            .child(
                v_flex()
                    .gap(px(6.))
                    .child(ui::eyebrow(map_area(self.map.as_ref(), &f.area), p, theme))
                    .child(div().text_size(px(18.)).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).child(f.title.clone()))
                    .child(h_flex().gap(px(6.)).child(ui::badge(label, tone)).child(ui::mono(f.id.clone(), theme).text_color(p.faint)))
                    .when(!f.summary.is_empty(), |d| d.child(div().text_size(px(13.)).text_color(p.fg).child(f.summary.clone())))
                    .when(!f.aliases.is_empty(), |d| d.child(div().text_size(px(12.)).text_color(p.muted).child(format!("ещё называют: {}", f.aliases.join(", "))))),
            )
            .children(f.paths.iter().enumerate().map(|(pi, path)| {
                v_flex()
                    .gap(px(8.))
                    .child(ui::section_title(if path.title.is_empty() { "Путь".to_owned() } else { path.title.clone() }, Some(path.steps.len().to_string()), p, theme))
                    .children(path.steps.iter().enumerate().map(|(si, s)| {
                        v_flex()
                            .gap(px(4.))
                            .pl(px(10.))
                            .border_l_2()
                            .border_color(p.border)
                            .child(div().text_size(px(11.)).text_color(p.faint).child(format!("{} · {}", si + 1, s.screen)))
                            .child(div().text_size(px(13.)).text_color(p.fg).child(s.action.clone()))
                            .when(!s.result.is_empty(), |d| d.child(div().text_size(px(12.5)).text_color(p.muted).child(format!("→ {}", s.result))))
                            .when(!s.entities.is_empty(), |d| d.child(h_flex().flex_wrap().gap(px(4.)).children(s.entities.iter().map(|e| ui::chip(e.clone(), p, theme)))))
                            .children(s.files.iter().enumerate().map(|(k, rel)| file_link(format!("flow-file-{fi}-{pi}-{si}-{k}"), rel.clone())))
                    }))
            }))
            .child(
                v_flex()
                    .gap(px(8.))
                    .child(ui::section_title("Чем доказано", Some(f.proofs.len().to_string()), p, theme))
                    .when(f.proofs.is_empty(), |d| d.child(ui::badge("Нет ни теста, ни скриншота: работает ли фича — неизвестно", p.red)))
                    .children(f.proofs.iter().enumerate().map(|(qi, proof)| {
                        let (kind, icon) = proof_label(proof.kind);
                        let full = repo.join(&proof.reference);
                        let openable = full.exists();
                        v_flex()
                            .id(SharedString::from(format!("flow-proof-panel-{fi}-{qi}")))
                            .gap(px(3.))
                            .p(px(10.))
                            .rounded(px(8.))
                            .border_1()
                            .border_color(p.border)
                            .when(openable, |d| d.cursor_pointer().hover(|s| s.border_color(p.border_strong)))
                            .when(openable, |d| d.on_click(move |_, window, cx| crate::preview::open_in_app(full.clone(), None, window, cx)))
                            .child(h_flex().gap(px(6.)).child(Icon::empty().path(icon).xsmall().text_color(p.muted)).child(ui::mono(kind.to_uppercase(), theme).text_size(px(10.)).text_color(p.faint)))
                            .child(div().text_size(px(13.)).text_color(p.fg).child(if proof.title.is_empty() { proof.reference.clone() } else { proof.title.clone() }))
                            .child(ui::mono(proof.reference.clone(), theme).text_size(px(11.)).text_color(if openable { p.blue.text } else { p.faint }))
                            .when(!proof.covers.is_empty(), |d| d.child(div().text_size(px(11.5)).text_color(p.muted).child(format!("покрывает шаги: {}", proof.covers.join(", ")))))
                    })),
            )
            .child(
                v_flex()
                    .gap(px(6.))
                    .child(ui::section_title("Недавние сессии", Some(recent.len().to_string()), p, theme))
                    .when(recent.is_empty(), |d| d.child(div().text_size(px(12.5)).text_color(p.muted).child("Сессии с отчётом «Изменения» этот флоу пока не трогали.")))
                    .children(recent.into_iter().map(|s| {
                        let id = s.session.clone();
                        h_flex()
                            .id(SharedString::from(format!("flow-session-{}", s.id())))
                            .gap(px(8.))
                            .py(px(6.))
                            .cursor_pointer()
                            .hover(|d| d.bg(p.surface2))
                            .on_click(cx.listener(move |_, _, _, cx| cx.emit(FlowsEvent::OpenSession(id.clone()))))
                            .child(div().size(px(7.)).rounded_full().bg(p.blue.solid))
                            .child(div().flex_1().min_w_0().truncate().text_size(px(12.5)).text_color(p.fg).child(if s.title.is_empty() { s.session.clone() } else { s.title.clone() }))
                            .when_some(s.at, |d, at| d.child(ui::mono(ago(at), theme).text_color(p.faint)))
                    })),
            )
    }

    fn render_proposals(&self, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let pending: Vec<SessionFlows> = self.pending().into_iter().cloned().collect();
        v_flex()
            .p(px(16.))
            .gap(px(12.))
            .child(div().text_size(px(18.)).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).child("Предложения изменить карту"))
            .child(div().text_size(px(12.5)).text_color(p.muted).child("Их пишет ревьюер «Изменений» после сессий: какие фичи и пути появились или поменялись."))
            .when(pending.is_empty(), |d| d.child(div().text_size(px(13.)).text_color(p.muted).child("Новых предложений нет.")))
            .children(pending.into_iter().map(|s| {
                let (accept, reject) = (s.id(), s.id());
                let notes: Vec<String> = if s.proposal.notes.is_empty() {
                    s.proposal
                        .upsert
                        .iter()
                        .map(|f| format!("± {}", if f.title.is_empty() { &f.id } else { &f.title }))
                        .chain(s.proposal.remove.iter().map(|id| format!("− {id}")))
                        .collect()
                } else {
                    s.proposal.notes.clone()
                };
                ui::card(p)
                    .p(px(12.))
                    .gap(px(6.))
                    .child(h_flex().gap(px(6.)).child(div().flex_1().min_w_0().truncate().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).child(if s.title.is_empty() { s.session.clone() } else { s.title.clone() })).when_some(s.at, |d, at| d.child(ui::mono(ago(at), theme).text_color(p.faint))))
                    .children(notes.into_iter().map(|n| div().text_size(px(12.5)).text_color(p.fg).child(n)))
                    .child(
                        h_flex()
                            .gap(px(6.))
                            .pt(px(4.))
                            .child(PressButton::new(SharedString::from(format!("flow-accept-{}", s.id()))).primary().xsmall().icon(IconName::Check).label("Принять").on_click(cx.listener(move |this, _, window, cx| this.decide(accept.clone(), true, window, cx))))
                            .child(PressButton::new(SharedString::from(format!("flow-reject-{}", s.id()))).xsmall().label("Отклонить").on_click(cx.listener(move |this, _, window, cx| this.decide(reject.clone(), false, window, cx)))),
                    )
            }))
    }
}

fn map_area(map: Option<&FlowMap>, id: &str) -> String {
    map.and_then(|m| m.areas.iter().find(|a| a.id == id)).map(|a| a.title.clone()).filter(|t| !t.is_empty()).unwrap_or_else(|| if id.is_empty() { "Другое".into() } else { id.to_owned() })
}

fn empty(title: &'static str, text: &'static str, p: &Palette) -> impl IntoElement {
    v_flex()
        .max_w(px(520.))
        .items_center()
        .gap(px(8.))
        .child(Icon::empty().path("icons/route.svg").size(px(28.)).text_color(p.faint))
        .child(div().text_size(px(17.)).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).child(title))
        .child(div().text_size(px(13.)).text_color(p.muted).text_center().child(text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use stepan_verify_core::flows::{Area, FlowPath, Proof, Step};

    fn feature(id: &str, area: &str, paths: usize, steps: usize, proofs: usize) -> Feature {
        Feature {
            id: id.into(),
            title: id.into(),
            area: area.into(),
            paths: (0..paths)
                .map(|p| FlowPath {
                    id: format!("p{p}"),
                    steps: (0..steps).map(|s| Step { id: format!("s{p}{s}"), action: "go".into(), ..Default::default() }).collect(),
                    ..Default::default()
                })
                .collect(),
            proofs: (0..proofs).map(|_| Proof { kind: ProofKind::Test, covers: vec!["s00".into()], ..Default::default() }).collect(),
            ..Default::default()
        }
    }

    fn map() -> FlowMap {
        FlowMap {
            areas: vec![Area { id: "a".into(), title: "Награды".into() }, Area { id: "b".into(), title: "Вход".into() }],
            features: vec![feature("a1", "a", 2, 3, 1), feature("b1", "b", 1, 2, 0), feature("x1", "zzz", 1, 1, 0)],
            ..Default::default()
        }
    }

    #[core::prelude::v1::test]
    fn three_columns_per_feature_row() {
        let m = map();
        let lay = layout(&m, &|_| true, 1);
        assert_eq!(lay.areas.len(), 3, "unknown area gets its own block");
        let (_, f) = lay.features[0];
        let paths: Vec<&Rect> = lay.paths.iter().filter(|(fi, _, _)| *fi == 0).map(|(_, _, r)| r).collect();
        assert_eq!(paths.len(), 2);
        assert!(paths[0].x > f.x + f.w && paths[1].y > paths[0].y, "paths right of the feature, stacked");
        let proof = lay.proofs.iter().find(|(fi, _, _)| *fi == 0).unwrap().2;
        assert!(proof.x > paths[0].x + paths[0].w);
        // The test covers only s00 → linked from the first path only.
        assert_eq!(lay.links.iter().filter(|l| l.2 == LinkKind::Proof).count(), 1);
        // A feature without proofs gets a red placeholder linked from its path.
        assert!(lay.proofs.iter().any(|(fi, s, _)| *fi == 1 && *s == ProofSlot::Missing));
        assert!(lay.links.iter().any(|l| l.2 == LinkKind::Missing));
        // Rows do not overlap.
        let b = lay.features.iter().find(|(fi, _)| *fi == 1).unwrap().1;
        assert!(b.y >= paths[1].y + paths[1].h);
    }

    #[core::prelude::v1::test]
    fn blocks_fill_columns_by_width() {
        let m = map();
        assert_eq!(columns_for(800.), 1);
        assert_eq!(columns_for(BLOCK_W * 2. + AREA_GAP + canvas::FIT_MARGIN * 2. + 1.), 2);
        let lay = layout(&m, &|_| true, 2);
        let xs: Vec<f32> = lay.areas.iter().map(|(_, r)| r.x).collect();
        assert!(xs.contains(&0.) && xs.iter().any(|&x| x > BLOCK_W), "areas spread over two columns");
        assert!(lay.width > BLOCK_W && lay.height < layout(&m, &|_| true, 1).height);
    }

    #[core::prelude::v1::test]
    fn search_filters_features() {
        let mut m = map();
        m.features[1].aliases = vec!["логин".into()];
        let lay = layout(&m, &|f| matches(f, "ЛОГИН"), 1);
        assert_eq!(lay.features.len(), 1);
        assert_eq!(lay.areas[0].0, "Вход");
        assert!(matches(&m.features[0], ""));
    }

    #[core::prelude::v1::test]
    fn pass_prompts_carry_marker_and_outputs() {
        let repo = Path::new("/tmp/demo");
        let inv = inventory_prompt(repo, Path::new("/tmp/demo/.stepan_verify/flows.parts/checklist.json"), false);
        assert!(inv.starts_with("[iv3 flows]") && inv.contains("flows.parts/inventory.json") && inv.contains("checklist.json") && inv.contains("\"status\": \"draft\""));
        assert!(inventory_prompt(repo, Path::new("x"), true).contains("remove"));
        let m = map();
        let batch = flows::Batch { id: "a-1".into(), title: "Награды".into(), features: vec!["a1".into()] };
        let d = details_prompt(repo, &m, &batch);
        assert!(d.starts_with("[iv3 flows]") && d.contains("flows.parts/a-1.json") && d.contains("- a1 ") && !d.contains("- b1 "));
        let pr = proofs_prompt(repo, &m, &Inventory { tests: vec!["x.test.ts".into()], ..Default::default() }, &[]);
        assert!(pr.starts_with("[iv3 flows]") && pr.contains("x.test.ts") && pr.contains("proofs.json"));
        let e = EntryPoint { kind: flowscan::EntryKind::Route, id: "/z".into(), file: "app/z/page.tsx".into(), area: "z".into() };
        let c = cover_prompt(repo, &[&e], "cover-1");
        assert!(c.starts_with("[iv3 flows]") && c.contains("/z") && c.contains("cover-1.json") && c.contains("attach"));
    }

    #[core::prelude::v1::test]
    fn inventory_then_details_merge() {
        let mut m = map();
        let inv: FlowProposal = serde_yaml::from_str(
            "areas: [{id: a, title: Награды}]\nupsert:\n  - {id: a1, title: Новое имя, area: a, entries: [/a]}\n  - {id: c1, title: Новая, area: a, entries: [/c], status: draft}\nignored: [{entry: /debug, reason: отладка}]",
        )
        .unwrap();
        merge_inventory(&mut m, &inv);
        let a1 = m.feature("a1").unwrap();
        assert_eq!(a1.title, "Новое имя");
        assert_eq!(a1.paths.len(), 2, "existing paths kept by the inventory pass");
        assert!(a1.status.is_empty());
        assert_eq!(m.feature("c1").unwrap().status, "draft");
        assert_eq!(m.ignored.len(), 1);
        let details: FlowProposal =
            serde_yaml::from_str("upsert:\n  - id: c1\n    title: Новая\n    paths: [{id: p, steps: [{id: s, action: go, files: [app/c/page.tsx]}]}]").unwrap();
        merge_details(&mut m, &details, false);
        let c1 = m.feature("c1").unwrap();
        assert!(c1.status.is_empty() && c1.paths.len() == 1);
        assert_eq!(c1.entries, vec!["/c".to_owned()], "entries survive the detail pass");
        assert_eq!(c1.area, "a", "area kept when the agent omits it");
    }

    #[core::prelude::v1::test]
    fn pass_results_parse_json_yaml_and_fenced() {
        let json = r#"{"upsert": [{"id": "x", "entries": ["/catalog/[id]", "app/(shop)/catalog/[id]/page.tsx"]}]}"#;
        let p: FlowProposal = read_part(json).unwrap();
        assert_eq!(p.upsert[0].entries[0], "/catalog/[id]");
        let fenced = format!("```json\n{json}\n```");
        assert!(read_part::<FlowProposal>(&fenced).is_ok());
        assert!(read_part::<FlowProposal>("upsert:\n  - id: y").is_ok(), "YAML still accepted");
        // The exact failure seen live: an unquoted route with brackets in a YAML flow list.
        let bad = "upsert:\n  - id: z\n    entries: [/catalog/[id], \"app/x.tsx\"]";
        assert!(check_part("catalog-1", bad).is_err());
        assert!(repair_prompt(Path::new("/r/.stepan_verify/flows.parts/catalog-1.json"), "boom").starts_with("[iv3 flows]"));
    }

    #[core::prelude::v1::test]
    fn deleted_page_touches_features_that_lead_to_it() {
        let repo = std::env::temp_dir().join(format!("sv-del-{}", std::process::id()));
        let _ = fs::create_dir_all(repo.join("app/auth/login"));
        let _ = fs::write(repo.join("app/auth/login/page.tsx"), "x");
        let changed = vec!["app/(auth)/auth/reset/page.tsx".to_owned(), "app/auth/login/page.tsx".to_owned(), "lib/a.ts".to_owned()];
        assert_eq!(deleted_routes(&repo, &changed), vec!["/auth/reset".to_owned()]);
        let mut login = feature("auth.login", "a", 1, 1, 0);
        login.paths[0].steps[0].result = "Открывается страница /auth/reset".into();
        assert!(mentions(&login, "/auth/reset"));
        assert!(!mentions(&login, "/auth/res"));
        let _ = fs::remove_dir_all(repo);
    }

    #[core::prelude::v1::test]
    fn cover_part_attaches_adds_and_ignores() {
        let mut m = map();
        let part: CoverPart = serde_yaml::from_str(
            "attach: [{feature: b1, entries: [/b/x]}]\nupsert: [{id: n1, title: Ещё, area: b}]\nignored: [{entry: /old, reason: редирект}]",
        )
        .unwrap();
        assert_eq!(merge_cover(&mut m, part), 3);
        assert!(m.feature("b1").unwrap().entries.contains(&"/b/x".to_owned()));
        assert!(m.feature("n1").is_some() && m.ignored.iter().any(|i| i.entry == "/old"));
    }
}
