//! «Изменения»: one «Было → Стало» story of a session, drawn as two Mermaid
//! diagrams rendered by Pele. An agent of the session's family reads the diff
//! and writes `changes.json` (the diagrams and a description of every block);
//! `render.mjs` turns it into pictures and block boxes. Here: a switch «Было /
//! Стало», «Разница», and a click on any block opens its description.

use std::{
    cell::Cell,
    fs,
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, Instant},
};

use gpui_kit::component::{ActiveTheme, IconName, Sizable as _, Theme, h_flex, spinner::Spinner, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde::Deserialize;
use stepan_verify_agents::{AgentEvent, AgentHandle, ItemKind, Mode, Provider, SessionSpec};

use crate::{
    format::ago,
    ui::{self, Palette, PressButton, Tone},
    zoom::{px, units},
};

const RENDER_MJS: &str = include_str!("render.mjs");

/// Widest a diagram is drawn: past this it would look blown up.
const MAX_SCALE: f32 = 1.25;
const PANEL_W: f32 = 360.;

/// What the session did, for the agent that draws it.
#[derive(Clone)]
pub struct Input {
    /// Native session id (or the task id): the cache key.
    pub key: String,
    pub provider: Provider,
    pub title: String,
    pub cwd: PathBuf,
    /// What you asked, in order.
    pub requests: Vec<String>,
    /// Files the session edited, first touched first.
    pub files: Vec<String>,
}

// ------------------------------------------------------------------ files

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Changes {
    pub title: String,
    pub summary: String,
    pub before: String,
    pub after: String,
    pub blocks: std::collections::HashMap<String, Block>,
}

/// The description of one block: what it is, and how it changed.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Block {
    pub name: String,
    pub about: String,
    pub file: String,
    pub change: String,
    pub fields: Vec<Field>,
    pub signature: Option<Signature>,
    pub code: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Field {
    pub name: String,
    pub before: String,
    pub after: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Signature {
    pub before: String,
    pub after: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Layout {
    pub before: Picture,
    pub after: Picture,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Picture {
    pub w: f32,
    pub h: f32,
    pub stats: Stats,
    pub blocks: std::collections::HashMap<String, BlockBox>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Stats {
    pub added: usize,
    pub removed: usize,
    pub changed: usize,
    pub edges: usize,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct BlockBox {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// added | removed | changed | same
    pub status: String,
    pub label: String,
}

/// A finished build on disk.
struct Built {
    dir: PathBuf,
    at: Option<chrono::DateTime<chrono::Utc>>,
    changes: Changes,
    layout: Layout,
}

fn root(key: &str) -> PathBuf {
    stepan_verify_core::Home::locate().state().join("before_after").join(key)
}

/// The newest build of a session with a rendered layout.
fn load_latest(key: &str) -> Option<Built> {
    let mut runs: Vec<(u64, PathBuf)> = fs::read_dir(root(key))
        .ok()?
        .flatten()
        .filter_map(|e| Some((e.file_name().to_str()?.parse().ok()?, e.path())))
        .collect();
    runs.sort_by_key(|(n, _)| std::cmp::Reverse(*n));
    runs.into_iter().find_map(|(_, dir)| load(&dir))
}

fn load(dir: &Path) -> Option<Built> {
    let changes: Changes = serde_json::from_str(&fs::read_to_string(dir.join("changes.json")).ok()?).ok()?;
    let layout: Layout = serde_json::from_str(&fs::read_to_string(dir.join("layout.json")).ok()?).ok()?;
    let at = fs::metadata(dir.join("layout.json")).and_then(|m| m.modified()).ok().map(chrono::DateTime::<chrono::Utc>::from);
    Some(Built { dir: dir.to_owned(), at, changes, layout })
}

/// render.mjs and the app's fonts, next to the builds: the agent and the app run the same script.
fn tools() -> std::io::Result<(PathBuf, PathBuf)> {
    let dir = stepan_verify_core::Home::locate().state().join("before_after");
    let fonts = dir.join("fonts");
    fs::create_dir_all(&fonts)?;
    let script = dir.join("render.mjs");
    if fs::read_to_string(&script).ok().as_deref() != Some(RENDER_MJS) {
        fs::write(&script, RENDER_MJS)?;
    }
    for (name, bytes) in [
        ("InstrumentSans-Regular.ttf", crate::icons::FONTS[0]),
        ("InstrumentSans-Medium.ttf", crate::icons::FONTS[2]),
        ("InstrumentSans-SemiBold.ttf", crate::icons::FONTS[4]),
        ("InstrumentSans-Bold.ttf", crate::icons::FONTS[6]),
    ] {
        let path = fonts.join(name);
        if !path.exists() {
            fs::write(path, bytes)?;
        }
    }
    Ok((script, fonts))
}

/// Runs render.mjs on a build folder; the error text is what node printed.
fn render(dir: &Path) -> Result<(), String> {
    let (script, fonts) = tools().map_err(|e| e.to_string())?;
    let out = std::process::Command::new(node())
        .arg(&script)
        .arg(dir)
        .arg(&fonts)
        .output()
        .map_err(|e| format!("node не запустился: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
    }
}

/// `node` from PATH, or from the usual places an app started from Finder does not see.
fn node() -> PathBuf {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    let candidates = [home.join(".local/bin/node"), PathBuf::from("/opt/homebrew/bin/node"), PathBuf::from("/usr/local/bin/node")];
    candidates.into_iter().find(|p| p.exists()).unwrap_or_else(|| PathBuf::from("node"))
}

/// The commit the session's changes start from: where the branch left the
/// default branch, or HEAD on the default branch itself (then the change is
/// the uncommitted work).
fn base(cwd: &Path) -> String {
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .filter(|s| !s.is_empty())
    };
    let upstream = git(&["rev-parse", "--abbrev-ref", "origin/HEAD"]).or_else(|| ["main", "master"].iter().find(|b| git(&["rev-parse", "--verify", b]).is_some()).map(|b| b.to_string()));
    upstream.and_then(|u| git(&["merge-base", "HEAD", &u])).or_else(|| git(&["rev-parse", "HEAD"])).unwrap_or_else(|| "HEAD".into())
}

fn prompt(input: &Input, out: &Path, script: &Path, fonts: &Path, base: &str) -> String {
    let clip = |s: &str, n: usize| if s.chars().count() > n { format!("{}…", s.chars().take(n).collect::<String>()) } else { s.to_owned() };
    let requests = input.requests.iter().map(|r| format!("- {}", clip(r.trim(), 600).replace('\n', " "))).collect::<Vec<_>>().join("\n");
    let files = if input.files.is_empty() { "(сессия не правила файлы инструментами — смотри весь дифф)".to_owned() } else { input.files.iter().map(|f| format!("- {f}")).collect::<Vec<_>>().join("\n") };
    let out = out.display();
    format!(
        "Ты рисуешь раздел «Изменения» для сессии «{title}»: одну историю «Было → Стало» в двух диаграммах Mermaid. \
Человек открывает её и за 10 секунд понимает, что поменялось в устройстве кода, а по клику на блок читает его описание.

Проект: {cwd}
Что просил пользователь:
{requests}

Файлы, которые меняла сессия:
{files}

Дифф: `git -C {cwd} diff {base}` (это и коммиты сессии, и незакоммиченное; новые файлы смотри в `git status`). Смотри прежде всего файлы сессии.

Что сделать:
1. Найди главное изменение в устройстве: какие части есть, как связаны, что появилось, что ушло, как изменился путь данных или событий. Одна история, без второстепенного.
2. Выбери тип диаграммы под историю, не flowchart по умолчанию: sequenceDiagram — меняется порядок вызовов или событий; stateDiagram-v2 — меняются состояния; flowchart — модули и связи; classDiagram — типы и поля; erDiagram — схема данных.
3. Нарисуй «Было» (before) и «Стало» (after) одного типа, 4–12 блоков на сторону. Одна и та же вещь — один и тот же id и тот же текст на обеих сторонах, иначе она засчитается изменённой. Новое есть только в «Стало», убранное — только в «Было», изменённое — тот же id, другой текст. Подписи короткие, по-русски; имена файлов, типов и функций — как в коде. Без classDef, style и цветов: оформление сделает рендер.
4. Для КАЖДОГО блока обеих диаграмм (ключ — его id в Mermaid; у участников sequence — их алиас) напиши описание в `blocks`.
5. Запиши {out}/changes.json и проверь рендер: `node \"{script}\" \"{out}\" \"{fonts}\"`. Он должен напечатать ok. Ошибка — поправь Mermaid и запусти снова.

Формат changes.json:
{{
  \"title\": \"суть изменения, до 60 знаков\",
  \"summary\": \"1–2 предложения: что стало иначе и зачем\",
  \"before\": \"…Mermaid…\",
  \"after\": \"…Mermaid…\",
  \"blocks\": {{
    \"<id>\": {{
      \"name\": \"имя как в коде или как на схеме\",
      \"about\": \"что это и зачем в этой истории, 1–3 строки простым языком\",
      \"file\": \"путь от корня репозитория, если есть\",
      \"change\": \"что поменялось, одной строкой; пусто, если блок не менялся\",
      \"fields\": [{{\"name\": \"id\", \"before\": \"\", \"after\": \"String\"}}],
      \"signature\": {{\"before\": \"fn old(…)\", \"after\": \"fn new(…)\"}},
      \"code\": \"ханк диффа: строка @@ … и строки с + / - / пробелом, до 25 строк\"
    }}
  }}
}}
`fields` — для типов, DTO и сообщений с данными (пустое before — поле новое, пустое after — убрано). `signature` — для функций, чья сигнатура поменялась. `code` — когда важна сама правка. Чего нет, не пиши.

Не меняй файлы проекта и не коммить: пиши только в {out}.",
        title = input.title,
        cwd = input.cwd.display(),
        script = script.display(),
        fonts = fonts.display(),
    )
}

/// Tool steps the way a person reads them.
fn humanize(title: &str) -> String {
    let known = [
        ("render.mjs", "Рисую схемы"),
        ("changes.json", "Пишу описание блоков"),
        ("git diff", "Читаю дифф"),
        ("git status", "Смотрю git status"),
        ("git log", "Смотрю историю"),
    ];
    known.iter().find(|(needle, _)| title.contains(needle)).map(|(_, l)| (*l).to_owned()).unwrap_or_else(|| "Читаю код".to_owned())
}

// ------------------------------------------------------------------ view

#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Before,
    After,
}

struct Running {
    dir: PathBuf,
    started: Instant,
    step: String,
    _handle: AgentHandle,
    _events: Task<()>,
}

pub struct BeforeAfterView {
    input: Input,
    built: Option<Built>,
    running: Option<Running>,
    error: Option<String>,
    side: Side,
    diff: bool,
    selected: Option<String>,
    hovered: Option<String>,
    /// Width the diagram can take, measured while painting.
    width: Rc<Cell<f32>>,
    _ticker: Task<()>,
}

impl BeforeAfterView {
    /// Shows the last build, or starts one.
    pub fn new(input: Input, cx: &mut Context<Self>) -> Self {
        stepan_verify_core::usage::hit("changes.open");
        let built = load_latest(&input.key);
        let mut this = Self {
            input,
            built,
            running: None,
            error: None,
            side: Side::After,
            diff: true,
            selected: None,
            hovered: None,
            width: Rc::new(Cell::new(0.)),
            _ticker: Task::ready(()),
        };
        // Debug hook: STEPAN_VERIFY_CHANGES_BLOCK=<id> opens a block's description (clicks
        // from automation do not reach gpui); STEPAN_VERIFY_CHANGES_SIDE=before shows «Было».
        this.selected = std::env::var("STEPAN_VERIFY_CHANGES_BLOCK").ok();
        if std::env::var("STEPAN_VERIFY_CHANGES_SIDE").as_deref() == Ok("before") {
            this.side = Side::Before;
        }
        if this.built.is_none() {
            this.start(cx);
        }
        this
    }

    /// The session moved on: the next build sees the new turns.
    pub fn update_input(&mut self, input: Input) {
        self.input = input;
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        if self.running.is_some() {
            return;
        }
        let dir = root(&self.input.key).join(chrono::Utc::now().timestamp_millis().to_string());
        let (script, fonts) = match tools().and_then(|t| fs::create_dir_all(&dir).map(|_| t)) {
            Ok(t) => t,
            Err(err) => {
                self.error = Some(format!("Не создать папку: {err}"));
                return;
            }
        };
        let text = prompt(&self.input, &dir, &script, &fonts, &base(&self.input.cwd));
        let spec = SessionSpec {
            provider: self.input.provider,
            native_id: None,
            transcript: None,
            cwd: self.input.cwd.clone(),
            mode: Mode::FullAccess,
            image_cache: Some(stepan_verify_core::Home::locate().state().join("images")),
            env: vec![("STEPAN_VERIFY_AGENT".to_owned(), "авто".to_owned())],
            model: crate::settings::report_model(self.input.provider, cx),
            instructions: None,
        };
        let (handle, events) = stepan_verify_agents::open(spec);
        handle.send(text);
        let events = cx.spawn(async move |this, cx| {
            while let Ok(event) = events.recv().await {
                let done = matches!(event, AgentEvent::TurnCompleted { .. } | AgentEvent::Exited | AgentEvent::Error(_));
                if this.update(cx, |this, cx| this.on_event(event, cx)).is_err() || done {
                    break;
                }
            }
        });
        self.error = None;
        self.running = Some(Running { dir, started: Instant::now(), step: "Читаю дифф".into(), _handle: handle, _events: events });
        self._ticker = cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            if this.update(cx, |this, cx| if this.running.is_some() { cx.notify() }).is_err() {
                break;
            }
        });
        cx.notify();
    }

    fn on_event(&mut self, event: AgentEvent, cx: &mut Context<Self>) {
        let Some(running) = &mut self.running else { return };
        match event {
            AgentEvent::Upsert(item) if item.kind == ItemKind::Tool && !item.title.is_empty() => {
                running.step = humanize(&item.title);
                cx.notify();
            }
            AgentEvent::TurnCompleted { error } => self.finish(error, cx),
            AgentEvent::Exited => self.finish(Some("Агент завершился раньше времени".into()), cx),
            AgentEvent::Error(err) => self.finish(Some(err), cx),
            _ => {}
        }
    }

    /// The agent is done: show what it built, rendering it here if it did not.
    fn finish(&mut self, error: Option<String>, cx: &mut Context<Self>) {
        let Some(running) = self.running.take() else { return };
        self._ticker = Task::ready(());
        let dir = running.dir;
        if !dir.join("changes.json").exists() {
            self.error = Some(error.unwrap_or_else(|| "Агент не записал changes.json".into()));
            cx.notify();
            return;
        }
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn({
                    let dir = dir.clone();
                    async move { if dir.join("layout.json").exists() { Ok(()) } else { render(&dir) } }
                })
                .await;
            this.update(cx, |this, cx| {
                match result.and_then(|_| load(&dir).ok_or_else(|| "changes.json или layout.json не читаются".to_owned())) {
                    Ok(built) => {
                        this.built = Some(built);
                        this.selected = None;
                    }
                    Err(err) => this.error = Some(err),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn picture<'a>(&self, b: &'a Built) -> &'a Picture {
        match self.side {
            Side::Before => &b.layout.before,
            Side::After => &b.layout.after,
        }
    }

    fn image(&self, b: &Built, dark: bool) -> PathBuf {
        let side = match self.side {
            Side::Before => "before",
            Side::After => "after",
        };
        let theme = if dark { "dark" } else { "light" };
        let plain = if self.diff { "" } else { "-plain" };
        b.dir.join(format!("{side}-{theme}{plain}.png"))
    }
}

impl Render for BeforeAfterView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let p = Palette::of(&theme);
        let panel = self.selected.clone().and_then(|id| {
            let b = self.built.as_ref()?;
            Some(self.render_panel(&id, b, &p, &theme, window, cx))
        });
        let body = match (&self.built, &self.running) {
            (_, Some(r)) if self.built.is_none() => self.render_running(r, &p, &theme).into_any_element(),
            (Some(b), _) => self.render_built(b, &p, &theme, cx).into_any_element(),
            (None, _) => self.render_empty(&p, cx).into_any_element(),
        };
        // A narrow window keeps the diagram whole: the description slides over it.
        let overlay = units(window.viewport_size().width) < 1300.;
        h_flex()
            .size_full()
            .relative()
            .items_start()
            .child(div().id("before-after").flex_1().min_w_0().h_full().overflow_y_scroll().child(v_flex().px(px(24.)).pt(px(20.)).pb(px(32.)).gap(px(16.)).child(body)))
            .when_some(panel, |d, panel| {
                if overlay {
                    d.child(div().absolute().top_0().right_0().h_full().shadow(ui::popover_shadow()).child(panel))
                } else {
                    d.child(panel)
                }
            })
    }
}

impl BeforeAfterView {
    fn render_running(&self, r: &Running, p: &Palette, theme: &Theme) -> impl IntoElement {
        ui::frame("ba-running", p).child(
            ui::inner(p).p(px(20.)).gap(px(8.)).child(
                h_flex()
                    .gap(px(10.))
                    .child(Spinner::new().small().color(p.blue.solid))
                    .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).child("Рисую «Было → Стало»"))
                    .child(ui::mono(format!("{} с", r.started.elapsed().as_secs()), theme).text_color(p.faint)),
            )
            .child(div().text_size(px(13.)).text_color(p.muted).child(r.step.clone())),
        )
    }

    fn render_empty(&self, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let err = self.error.clone();
        ui::frame("ba-empty", p).child(
            ui::inner(p)
                .p(px(20.))
                .gap(px(10.))
                .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).child(if err.is_some() { "Схема не собралась" } else { "Схемы пока нет" }))
                .when_some(err, |d, e| d.child(div().text_size(px(12.5)).font_family("Menlo").text_color(p.red.text).child(e)))
                .child(h_flex().child(PressButton::new("ba-build").small().primary().icon(IconName::RefreshCw).label("Собрать").on_click(cx.listener(|this, _, _, cx| this.start(cx))))),
        )
    }

    fn render_built(&self, b: &Built, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let pic = self.picture(b);
        let dark = theme.is_dark();
        // The diagram fits the width at one scale; never blown up past MAX_SCALE.
        let avail = self.width.get();
        let avail = if avail > 0. { avail } else { 700. };
        let scale = ((avail - 2.) / pic.w.max(1.)).min(MAX_SCALE);
        let (w, h) = (pic.w * scale, pic.h * scale);
        let image = self.image(b, dark);
        let width = self.width.clone();
        let stat = |n: usize, sign: &str, word: &str, t: Tone| (n > 0).then(|| ui::mono(format!("{sign}{n} {word}"), theme).text_color(t.text));
        let s = &pic.stats;
        let blocks: Vec<(String, BlockBox)> = pic.blocks.iter().map(|(k, v)| (k.clone(), v.clone())).collect();

        v_flex()
            .gap(px(16.))
            // Title and what changed, full width.
            .child(
                v_flex()
                    .gap(px(6.))
                    .child(div().text_size(px(20.)).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).child(if b.changes.title.is_empty() { self.input.title.clone() } else { b.changes.title.clone() }))
                    .when(!b.changes.summary.is_empty(), |d| d.child(div().text_size(px(13.5)).line_height(px(20.)).text_color(p.muted).child(b.changes.summary.clone()))),
            )
            .child(
                ui::frame("ba-frame", p)
                    .child(
                        h_flex()
                            .px(px(6.))
                            .py(px(5.))
                            .gap(px(12.))
                            .flex_wrap()
                            .child(
                                ui::segmented(p)
                                    .child(ui::segment("ba-before", "Было", self.side == Side::Before, p).gap(px(6.)).h(px(26.)).px(px(12.)).text_size(px(13.)).child(ui::dot(p.red.solid)).on_click(cx.listener(|this, _, _, cx| {
                                        this.side = Side::Before;
                                        cx.notify();
                                    })))
                                    .child(ui::segment("ba-after", "Стало", self.side == Side::After, p).gap(px(6.)).h(px(26.)).px(px(12.)).text_size(px(13.)).child(ui::dot(p.green.solid)).on_click(cx.listener(|this, _, _, cx| {
                                        this.side = Side::After;
                                        cx.notify();
                                    }))),
                            )
                            .child(
                                h_flex()
                                    .id("ba-diff")
                                    .gap(px(7.))
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.diff = !this.diff;
                                        cx.notify();
                                    }))
                                    .child(div().size(px(14.)).rounded(px(4.)).border_1().border_color(if self.diff { p.accent.solid } else { p.border_strong }).when(self.diff, |d| d.bg(p.accent.solid)))
                                    .child(div().text_size(px(13.)).text_color(p.fg).child("Разница")),
                            )
                            .when(self.diff, |d| {
                                d.children(stat(s.added, "+", "новое", p.green))
                                    .children(stat(s.changed, "~", "изменено", p.amber))
                                    .children(stat(s.removed, "−", "убрано", p.red))
                            })
                            .child(div().flex_1())
                            .when_some(b.at, |d, at| d.child(ui::mono(format!("собрано {}", ago(at)), theme).text_color(p.faint)))
                            .child(match &self.running {
                                Some(r) => h_flex().gap(px(6.)).child(Spinner::new().xsmall().color(p.blue.solid)).child(ui::mono(r.step.clone(), theme).text_color(p.muted)).into_any_element(),
                                None => PressButton::new("ba-rebuild").xsmall().ghost().icon(IconName::RefreshCw).label("Пересобрать").on_click(cx.listener(|this, _, _, cx| this.start(cx))).into_any_element(),
                            }),
                    )
                    .when_some(self.error.clone(), |d, e| d.child(div().px(px(10.)).pb(px(6.)).text_size(px(12.)).text_color(p.red.text).child(e)))
                    .child(
                        ui::inner(p).p(px(16.)).items_center().child(
                            div()
                                .w_full()
                                .relative()
                                // Measures the room the diagram has.
                                .child(
                                    canvas(
                                        move |bounds, window, _| {
                                            let w = units(bounds.size.width);
                                            if (w - width.get()).abs() > 1. {
                                                width.set(w);
                                                window.refresh();
                                            }
                                        },
                                        |_, _, _, _| {},
                                    )
                                        .absolute()
                                        .size_full(),
                                )
                                .child(
                                    div()
                                        .relative()
                                        .mx_auto()
                                        .w(px(w))
                                        .h(px(h))
                                        .child(img(image).w(px(w)).h(px(h)))
                                        .children(blocks.into_iter().map(|(id, bx)| {
                                            let on = self.selected.as_deref() == Some(id.as_str());
                                            let hover = self.hovered.as_deref() == Some(id.as_str());
                                            let (hid, cid) = (id.clone(), id.clone());
                                            div()
                                                .id(SharedString::from(format!("ba-block-{id}")))
                                                .absolute()
                                                .left(px(bx.x * scale - 3.))
                                                .top(px(bx.y * scale - 3.))
                                                .w(px(bx.w * scale + 6.))
                                                .h(px(bx.h * scale + 6.))
                                                .rounded(px(9.))
                                                .border_2()
                                                .border_color(if on { p.accent.solid } else if hover { p.border_strong } else { transparent_black() })
                                                .cursor_pointer()
                                                .on_hover(cx.listener(move |this, over: &bool, _, cx| {
                                                    let now = over.then(|| hid.clone());
                                                    if this.hovered != now {
                                                        this.hovered = now;
                                                        cx.notify();
                                                    }
                                                }))
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.selected = if this.selected.as_deref() == Some(cid.as_str()) { None } else { Some(cid.clone()) };
                                                    cx.notify();
                                                }))
                                        })),
                                ),
                        ),
                    ),
            )
            .child(ui::mono("клик по блоку — что это и что в нём поменялось", theme).text_color(p.faint))
    }

    fn render_panel(&self, id: &str, b: &Built, p: &Palette, theme: &Theme, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let block = b.changes.blocks.get(id).cloned().unwrap_or_default();
        let here = self.picture(b).blocks.get(id).cloned();
        let elsewhere = match self.side {
            Side::Before => b.layout.after.blocks.get(id),
            Side::After => b.layout.before.blocks.get(id),
        }
        .cloned();
        let status = here.as_ref().or(elsewhere.as_ref()).map(|x| x.status.clone()).unwrap_or_default();
        // A block only on the other side keeps its status from there.
        let status = match (here.is_some(), elsewhere.as_ref().map(|e| e.status.as_str())) {
            (false, Some(s)) => s.to_owned(),
            _ => status,
        };
        let name = if block.name.is_empty() { here.as_ref().or(elsewhere.as_ref()).map(|x| x.label.clone()).unwrap_or_else(|| id.to_owned()) } else { block.name.clone() };
        let (badge, tone) = match status.as_str() {
            "added" => ("новое", Some(p.green)),
            "removed" => ("убрано", Some(p.red)),
            "changed" => ("изменено", Some(p.amber)),
            _ => ("не менялось", None),
        };
        let missing = here.is_none().then(|| if self.side == Side::Before { "На схеме «Было» этого нет: блок появился в «Стало»." } else { "На схеме «Стало» этого нет: блок убран." });
        let mono = theme.mono_font_family.clone();
        let file_path = self.input.cwd.join(&block.file);
        let root = self.input.cwd.clone();
        let label = |t: &'static str| ui::eyebrow(t, p, theme);
        let code_box = |text: String, bg: Hsla| div().p(px(8.)).rounded(px(6.)).bg(bg).font_family(mono.clone()).text_size(px(12.)).line_height(px(18.)).text_color(p.fg).child(text);

        v_flex()
            .w(px(PANEL_W))
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
                    .child(match tone {
                        Some(t) => ui::dot_badge(badge, t).into_any_element(),
                        None => ui::chip(badge, p, theme).into_any_element(),
                    })
                    .child(div().flex_1())
                    .child(PressButton::new("ba-panel-close").ghost().small().icon(IconName::Close).on_click(cx.listener(|this, _, _, cx| {
                        this.selected = None;
                        cx.notify();
                    }))),
            )
            .child(
                div().id("ba-panel").flex_1().min_h_0().overflow_y_scroll().child(
                    v_flex()
                        .p(px(16.))
                        .gap(px(14.))
                        .child(div().font_family(mono.clone()).text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).when(status == "removed", |d| d.line_through()).child(name))
                        .when(!block.file.is_empty(), |d| {
                            d.child(
                                div()
                                    .id("ba-file")
                                    .cursor_pointer()
                                    .font_family(mono.clone())
                                    .text_size(px(11.5))
                                    .text_color(p.blue.text)
                                    .hover(|s| s.text_color(p.blue.solid))
                                    .on_click(move |_, window, cx| crate::preview::open_in_app(file_path.clone(), Some(root.clone()), window, cx))
                                    .child(block.file.clone()),
                            )
                        })
                        .when_some(missing, |d, m| d.child(div().text_size(px(12.5)).text_color(p.muted).child(m)))
                        .when(!block.about.is_empty(), |d| d.child(div().text_size(px(13.5)).line_height(px(20.)).text_color(p.fg).child(block.about.clone())))
                        .when(block.about.is_empty() && block.change.is_empty(), |d| d.child(div().text_size(px(12.5)).text_color(p.faint).child("Агент не описал этот блок — нажми «Пересобрать».")))
                        .when(!block.change.is_empty(), |d| {
                            d.child(v_flex().gap(px(4.)).child(label("что поменялось")).child(div().text_size(px(13.)).line_height(px(19.)).text_color(p.fg).child(block.change.clone())))
                        })
                        .when(!block.fields.is_empty(), |d| {
                            d.child(v_flex().gap(px(4.)).child(label("поля")).children(block.fields.iter().map(|f| {
                                let (sign, t) = match (f.before.is_empty(), f.after.is_empty()) {
                                    (true, false) => ("+", Some(p.green)),
                                    (false, true) => ("−", Some(p.red)),
                                    _ if f.before != f.after => ("~", Some(p.amber)),
                                    _ => (" ", None),
                                };
                                let ty = match (f.before.is_empty(), f.after.is_empty()) {
                                    (false, false) if f.before != f.after => format!("{} → {}", f.before, f.after),
                                    (true, _) => f.after.clone(),
                                    _ => f.before.clone(),
                                };
                                h_flex()
                                    .gap(px(8.))
                                    .px(px(8.))
                                    .py(px(4.))
                                    .rounded(px(5.))
                                    .when_some(t, |d, t| d.bg(t.soft))
                                    .font_family(mono.clone())
                                    .text_size(px(12.))
                                    .child(div().w(px(10.)).text_color(t.map_or(p.faint, |t| t.text)).child(sign))
                                    .child(div().text_color(p.ink).when(sign == "−", |d| d.line_through()).child(f.name.clone()))
                                    .child(div().flex_1().min_w_0().truncate().text_color(p.muted).child(ty))
                            })))
                        })
                        .when_some(block.signature.clone().filter(|s| !s.before.is_empty() || !s.after.is_empty()), |d, s| {
                            d.child(
                                v_flex()
                                    .gap(px(6.))
                                    .child(label("сигнатура"))
                                    .when(!s.before.is_empty(), |d| d.child(code_box(s.before.clone(), ui::mix(p.red.soft, p.surface, 0.6))))
                                    .when(!s.after.is_empty(), |d| d.child(code_box(s.after.clone(), ui::mix(p.green.soft, p.surface, 0.6)))),
                            )
                        })
                        .when(!block.code.is_empty(), |d| d.child(v_flex().gap(px(6.)).child(label("код")).child(render_hunk(&block.code, p, theme)))),
                ),
            )
    }
}

fn render_hunk(code: &str, p: &Palette, theme: &Theme) -> impl IntoElement {
    v_flex()
        .id("ba-hunk")
        .overflow_x_scroll()
        .rounded(px(6.))
        .border_1()
        .border_color(p.border)
        .bg(p.surface2)
        .py(px(4.))
        .overflow_hidden()
        .font_family(theme.mono_font_family.clone())
        .text_size(px(11.5))
        .line_height(px(17.))
        .children(code.lines().take(40).map(|line| {
            let (bg, fg) = match line.chars().next() {
                Some('+') => (Some(p.green.soft), p.green.text),
                Some('-') => (Some(p.red.soft), p.red.text),
                Some('@') => (None, p.blue.text),
                _ => (None, p.fg),
            };
            div().px(px(8.)).when_some(bg, |d, bg| d.bg(bg)).text_color(fg).whitespace_nowrap().child(line.to_owned())
        }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[core::prelude::v1::test]
    fn changes_and_layout_parse() {
        let c: Changes = serde_json::from_str(
            r#"{"title":"t","before":"flowchart LR\n a","after":"flowchart LR\n a --> b",
                "blocks":{"b":{"name":"host.rs","about":"держит агента","fields":[{"name":"id","after":"String"}],"signature":{"after":"fn run()"}}}}"#,
        )
        .unwrap();
        assert_eq!(c.blocks["b"].fields[0].after, "String");
        assert_eq!(c.blocks["b"].signature.as_ref().unwrap().after, "fn run()");
        let l: Layout = serde_json::from_str(r#"{"before":{"w":10,"h":5,"stats":{"added":0},"blocks":{"a":{"x":1,"y":2,"w":3,"h":4,"status":"same","label":"A"}}},"after":{"w":12,"h":5,"blocks":{}}}"#).unwrap();
        assert_eq!(l.before.blocks["a"].status, "same");
        assert_eq!(l.after.w, 12.);
    }

    #[core::prelude::v1::test]
    fn prompt_names_the_files_and_the_script() {
        let input = Input { key: "k".into(), provider: Provider::Claude, title: "Хозяин".into(), cwd: "/r".into(), requests: vec!["сделай".into()], files: vec!["src/a.rs".into()] };
        let text = prompt(&input, Path::new("/o"), Path::new("/s/render.mjs"), Path::new("/f"), "abc");
        assert!(text.contains("- src/a.rs") && text.contains("git -C /r diff abc") && text.contains("node \"/s/render.mjs\" \"/o\" \"/f\""));
    }
}
