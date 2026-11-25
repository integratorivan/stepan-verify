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
    flows::{self, Decisions, Feature, FlowMap, Freshness, ProofKind, SessionFlows},
};

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
}

impl EventEmitter<FlowsEvent> for FlowsView {}

#[derive(Clone, PartialEq)]
enum Selection {
    Feature(usize),
    Proposals,
}

struct Building {
    started: Instant,
    who: String,
    update: bool,
    steps: Vec<String>,
    _handle: AgentHandle,
    _events: Task<()>,
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
    building: Option<Building>,
    build_error: Option<String>,
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
            building: None,
            build_error: None,
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
                    if mtime != this.mtime && this.building.is_none() {
                        this.load(cx);
                    } else if n % 10 == 0 {
                        this.load_sessions();
                        cx.notify();
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

    pub(crate) fn build(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.project.clone() else { return };
        if self.building.is_some() {
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
        let who = crate::changes::reviewer_name(provider, model.as_deref());
        let update = flows::path(&repo).exists();
        let out = flows::path(&repo);
        let spec = SessionSpec {
            provider,
            native_id: None,
            transcript: None,
            cwd: repo.clone(),
            // It reads the whole project and writes one file; it must not stop to ask.
            mode: Mode::FullAccess,
            image_cache: Some(self.home.state().join("images")),
            env: vec![("STEPAN_VERIFY_FLOWS_OUT".into(), out.display().to_string())],
            model,
            instructions: None,
        };
        let (handle, events) = stepan_verify_agents::open(spec);
        handle.send(prompt(&repo, update));
        let events = cx.spawn(async move |this, cx| {
            while let Ok(event) = events.recv().await {
                let done = matches!(event, AgentEvent::TurnCompleted { .. } | AgentEvent::Exited | AgentEvent::Error(_));
                let alive = this.update(cx, |this, cx| {
                    this.on_event(event, cx);
                    cx.notify();
                });
                if alive.is_err() || done {
                    break;
                }
            }
        });
        self.building = Some(Building { started: Instant::now(), who, update, steps: Vec::new(), _handle: handle, _events: events });
        self.build_error = None;
        self._ticker = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let going = this.update(cx, |this, cx| {
                    cx.notify();
                    this.building.is_some()
                });
                if !matches!(going, Ok(true)) {
                    break;
                }
            }
        });
        cx.notify();
    }

    fn on_event(&mut self, event: AgentEvent, cx: &mut Context<Self>) {
        let Some(b) = &mut self.building else { return };
        match event {
            AgentEvent::Upsert(item) if item.kind == ItemKind::Tool && !item.title.is_empty() => {
                let step = humanize(&item.title);
                if b.steps.last() != Some(&step) {
                    b.steps.push(step);
                }
            }
            AgentEvent::TurnCompleted { error } => self.finish(error, cx),
            AgentEvent::Exited => self.finish(Some("Агент карты завершился раньше времени".into()), cx),
            AgentEvent::Error(err) => self.finish(Some(err), cx),
            _ => {}
        }
    }

    fn finish(&mut self, error: Option<String>, cx: &mut Context<Self>) {
        let Some(b) = self.building.take() else { return };
        let before = self.mtime;
        self.placed = false;
        self.load(cx);
        // Proofs the agent found describe the code as it is now: stamp them so
        // later edits to the covered files show up as «устарело».
        if let (Some(repo), Some(map)) = (self.project.clone(), self.map.as_mut()) {
            let now = chrono::Utc::now();
            let mut stamped = false;
            for proof in map.features.iter_mut().flat_map(|f| f.proofs.iter_mut()).filter(|p| p.at.is_none()) {
                proof.at = Some(now + chrono::Duration::seconds(2));
                stamped = true;
            }
            if stamped && map.save(&repo).is_ok() {
                self.load(cx);
            }
        }
        let wrote = self.mtime.is_some() && self.mtime != before || (b.update && self.mtime.is_some() && error.is_none());
        self.build_error = match (&self.error, wrote, error) {
            (Some(e), _, _) => Some(format!("Карта не читается: {e}")),
            (None, false, Some(e)) => Some(e),
            (None, false, None) => Some("Агент закончил, но не записал .stepan_verify/flows.yaml".into()),
            _ => None,
        };
        cx.notify();
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
    known.iter().find(|(needle, _)| t.contains(needle)).map(|(_, l)| (*l).to_owned()).unwrap_or_else(|| title.chars().take(90).collect())
}

/// The map builder's brief. First line carries the `[iv3` marker that keeps
/// helper sessions off the dashboard.
pub fn prompt(repo: &Path, update: bool) -> String {
    let out = flows::path(repo);
    let task = if update {
        format!("Карта уже есть: {}. Прочитай её и ОБНОВИ под текущий код: добавь новые фичи и пути, убери исчезнувшие, поправь шаги, entities/files и доказательства. id существующих фич и шагов не меняй, если фича та же.", out.display())
    } else {
        format!("Карты ещё нет. Создай файл {} (папку .stepan_verify создай).", out.display())
    };
    format!(
        r#"[iv3 flows] Построй карту пользовательских флоу проекта {repo}.

Это «feature map» с точки зрения пользователя: какие фичи есть → как пользователь до них добирается → что доказывает, что фича работает. Карта — общий словарь продукта для людей и агентов: когда человек говорит «та штука в левом сайдбаре», агент по карте понимает, о чём речь.

{task}

Как работать:
1. Изучи проект: маршруты/страницы/экраны, меню и навигацию, диалоги, команды CLI, горячие клавиши, обработчики действий, сторы. Посмотри существующие тесты (unit/e2e/snapshot), скриншоты, видео и сценарии проверки в репозитории.
2. Фичи называй так, как их назвал бы пользователь («Выбор карты Джокера», «Вход по почте»), не именами компонентов. Группируй в области продукта (areas). Добавь aliases — как ещё люди называют фичу.
3. Для каждой фичи — 1–3 пути (paths): цепочка шагов, каждый шаг — screen (где пользователь), action (что делает), result (что видит после), entities (имена компонентов/функций/сторов, через которые идёт шаг), files (пути от корня репозитория).
4. proofs — только то, что реально есть в репозитории: тесты (reference = путь к файлу или команда запуска), скриншоты/видео (путь). covers — id шагов, которые доказательство покрывает (пусто = вся фича). at не заполняй. Если доказательств нет — оставь proofs пустым: это честно и видно на карте.
5. Не меняй код проекта, ничего не коммить и не пуш. Пиши только этот один файл.
6. Охват: все заметные пользователю фичи (обычно 8–40). Мелкие детали реализации не нужны.

Формат файла — YAML ровно по этой схеме:

version: 1
project: <имя проекта>
areas:
  - {{id: rewards, title: Награды}}
features:
  - id: rewards.joker-pick
    title: Выбор карты Джокера
    area: rewards
    summary: Пользователь выбирает карту, и она летит в коллекцию
    aliases: [джокер, диалог джокера]
    paths:
      - id: from-daily
        title: Из ежедневной карты
        steps:
          - id: open
            screen: Rewards Hub
            action: Нажать «Выбрать Джокера» на ежедневной карте
            result: Открывается диалог выбора
            entities: [GlobalDailyCard, JokerPickDialog]
            files: [apps/web/src/lib/components/RewardsHub/GlobalDailyCard.tsx]
          - id: pick
            screen: Диалог выбора Джокера
            action: Выбрать карту и закрыть диалог
            result: Карта летит в хаб и появляется в коллекции
            entities: [JokerPickDialog, takeFlight]
            files: [apps/web/src/lib/components/RewardsHub/JokerPickDialog.tsx]
    proofs:
      - {{kind: test, title: Диалог выбора, reference: apps/web/src/lib/components/RewardsHub/JokerPickDialog.test.tsx, covers: [pick]}}

kind в proofs: test | screenshot | video | cli | manual.
В конце коротко ответь: сколько областей, фич, путей и доказательств, и какие фичи остались без доказательств."#,
        repo = repo.display(),
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
        let building = self.building.is_some();
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
                                    (true, _) => "Агент строит карту…",
                                    (false, true) => "Обновить карту",
                                    (false, false) => "Построить карту",
                                })
                                .on_click(cx.listener(|this, _, _, cx| this.build(cx))),
                        )
                    }),
            )
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
        if let Some(b) = &self.building {
            if self.map.is_none() || b.update {
                // Keep the old map visible while it updates; show progress over it.
                if self.map.is_none() {
                    return self.render_building(p, theme).into_any_element();
                }
            }
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
        h_flex()
            .size_full()
            .child(div().flex_1().min_w_0().h_full().relative().child(self.render_canvas(&map, &lay, p, theme, cx)))
            .when_some(self.selected.clone(), |d, sel| d.child(self.render_panel(&map, sel, p, theme, cx)))
            .into_any_element()
    }

    fn render_building(&self, p: &Palette, theme: &Theme) -> impl IntoElement {
        let b = self.building.as_ref().expect("building");
        let secs = b.started.elapsed().as_secs();
        v_flex().size_full().items_center().justify_center().child(
            ui::frame("flows-building", p).w(px(520.)).child(
                ui::inner(p)
                    .p(px(16.))
                    .gap(px(10.))
                    .child(
                        h_flex()
                            .gap(px(10.))
                            .child(Spinner::new().small().color(p.blue.solid))
                            .child(div().flex_1().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).child(if b.update { "Обновляю карту флоу" } else { "Строю карту флоу" }))
                            .child(ui::mono(crate::live_card::mmss(secs), theme).text_color(p.blue.text)),
                    )
                    .child(div().text_size(px(12.5)).text_color(p.muted).child(format!("{} изучает маршруты, экраны и тесты проекта", b.who)))
                    .children(b.steps.iter().rev().take(6).collect::<Vec<_>>().into_iter().rev().map(|s| {
                        h_flex().gap(px(8.)).child(Icon::new(IconName::CircleCheck).xsmall().text_color(p.muted)).child(ui::mono(s.clone(), theme).truncate().text_color(p.muted))
                    })),
            ),
        )
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
            let (tone, label) = freshness_tone(fresh, p);
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
        let building = self.building.as_ref().map(|b| (b.started.elapsed().as_secs(), b.steps.last().cloned().unwrap_or_default()));

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
            .when_some(building, |d, (secs, step)| {
                d.child(
                    h_flex()
                        .absolute()
                        .top(px(10.))
                        .left(px(12.))
                        .gap(px(8.))
                        .px(px(10.))
                        .py(px(6.))
                        .rounded(px(8.))
                        .border_1()
                        .border_color(ui::mix(p.blue.solid, p.border, 0.5))
                        .bg(p.surface)
                        .child(Spinner::new().xsmall().color(p.blue.solid))
                        .child(div().text_size(px(12.)).text_color(p.fg).child("Агент обновляет карту"))
                        .child(ui::mono(format!("{} · {}", crate::live_card::mmss(secs), step), theme).max_w(px(320.)).truncate().text_color(p.muted)),
                )
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
    fn prompt_carries_marker_and_schema() {
        let p = prompt(Path::new("/tmp/demo"), false);
        assert!(p.starts_with("[iv3 flows]"));
        assert!(p.contains("/tmp/demo/.stepan_verify/flows.yaml") && p.contains("proofs:") && p.contains("covers"));
        assert!(prompt(Path::new("/tmp/demo"), true).contains("ОБНОВИ"));
    }
}
