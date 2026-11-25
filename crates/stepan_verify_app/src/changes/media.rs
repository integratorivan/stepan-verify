//! Before/after media of a session («Было → Стало»). When an agent finishes a
//! turn that touched UI files, a helper agent of the same family runs the
//! pr-before-after skill: the old state comes up in a throwaway git worktree,
//! the new one is the work tree as is; screenshots, pairs and GIFs land in
//! `~/.stepan_verify/tmp/sessions/<key>/media/<n>/` with a `manifest.json`.
//!
//! Jobs live in a global (not in a view) so a pass runs even when the
//! «Изменения» tab was never opened; views observe the global to redraw.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, Theme, h_flex, spinner::Spinner, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use crate::zoom::px;
use stepan_verify_agents::{AgentEvent, AgentHandle, ItemKind, Mode, SessionSpec};
use stepan_verify_core::config::MediaMode;
use serde::{Deserialize, Serialize};

use super::{ChangesInput, ChangesView, LogItem, reviewer_name, runs};
use crate::ui::{self, Palette, PressButton};

// ------------------------------------------------------------------ storage

/// `~/.stepan_verify/tmp/sessions/<key>/media`.
pub fn media_dir(key: &str) -> PathBuf {
    stepan_verify_core::Home::locate().tmp().join("sessions").join(key).join("media")
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Trigger {
    #[default]
    Auto,
    Manual,
}

/// Written by the app for every pass.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Meta {
    pub at: Option<DateTime<Utc>>,
    pub trigger: Trigger,
    /// «Было»: the commit the old state is rebuilt from.
    pub base: Option<String>,
    /// The work tree when the pass started: the next pass's «Было».
    pub after: Option<String>,
    /// Files changed since the previous pass.
    pub files: Vec<String>,
    /// Session log entries covered (the next auto pass starts after them).
    pub upto: usize,
    pub reviewer: String,
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaKind {
    #[default]
    Image,
    Gif,
    Video,
}

/// One scene of the manifest the helper agent writes.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Item {
    pub kind: MediaKind,
    pub before: Option<String>,
    pub after: Option<String>,
    /// A ready before/after card (pair.mjs) instead of two frames.
    pub pair: Option<String>,
    pub caption: String,
    /// Route or scenario the scene shows.
    #[serde(alias = "route")]
    pub scenario: String,
    /// Which part of the task the scene proves.
    pub why: String,
    pub created_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Default)]
pub struct Manifest {
    pub summary: String,
    pub items: Vec<Item>,
    /// Why something is missing («Было» could not be built, nothing visible…).
    pub notes: String,
}

/// The manifest may be a bare list of items or an object with `items`.
#[derive(Deserialize)]
#[serde(untagged)]
enum ManifestRepr {
    List(Vec<Item>),
    Object {
        #[serde(default)]
        summary: String,
        #[serde(default)]
        items: Vec<Item>,
        #[serde(default)]
        notes: String,
    },
}

pub fn parse_manifest(text: &str) -> Result<Manifest, String> {
    match serde_json::from_str::<ManifestRepr>(text).map_err(|e| format!("manifest.json не читается: {e}"))? {
        ManifestRepr::List(items) => Ok(Manifest { items, ..Manifest::default() }),
        ManifestRepr::Object { summary, items, notes } => Ok(Manifest { summary, items, notes }),
    }
}

pub struct Run {
    pub n: usize,
    pub dir: PathBuf,
    pub meta: Meta,
    pub manifest: Option<Result<Manifest, String>>,
}

pub fn load_all(key: &str) -> Vec<Run> {
    let root = media_dir(key);
    let Ok(entries) = fs::read_dir(&root) else { return Vec::new() };
    let mut out: Vec<Run> = entries
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_str()?.parse::<usize>().ok()?;
            let dir = e.path();
            let meta = fs::read_to_string(dir.join("meta.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
            let manifest = fs::read_to_string(dir.join("manifest.json")).ok().map(|t| parse_manifest(&t));
            Some(Run { n, dir, meta, manifest })
        })
        .collect();
    out.sort_by_key(|r| r.n);
    out
}

fn save_meta(dir: &Path, meta: &Meta) {
    let _ = fs::write(dir.join("meta.json"), serde_json::to_string_pretty(meta).unwrap_or_default());
}

fn next_dir(key: &str) -> std::io::Result<(usize, PathBuf)> {
    let n = load_all(key).last().map_or(1, |r| r.n + 1);
    let dir = media_dir(key).join(n.to_string());
    fs::create_dir_all(dir.join("raw"))?;
    Ok((n, dir))
}

/// The work tree when the session's first turn started here: the «Было» of
/// the first pass. Kept once; later turns don't move it.
pub fn turn_started(key: &str, cwd: &Path) {
    let file = media_dir(key).join("base");
    if file.exists() || !runs::is_repo(cwd) {
        return;
    }
    if let Some(snap) = runs::snapshot(cwd) {
        let _ = fs::create_dir_all(media_dir(key)).and_then(|_| fs::write(&file, snap));
    }
}

fn session_base(key: &str) -> Option<String> {
    fs::read_to_string(media_dir(key).join("base")).ok().map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
}

// ------------------------------------------------------------------ what changed

/// What kind of before/after a set of changed files deserves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Need {
    /// Something you can see: screens, styles, templates, native UI.
    Ui,
    /// Code without its own UI: the skill shows terminal output or a diagram.
    Code,
    None,
}

pub fn classify<S: AsRef<str>>(files: &[S]) -> Need {
    const UI_EXT: [&str; 22] = [
        "tsx", "jsx", "vue", "svelte", "astro", "html", "htm", "css", "scss", "sass", "less", "styl", "mdx", "swift", "xib", "storyboard",
        "dart", "kt", "svg", "png", "jpg", "webp",
    ];
    const UI_DIRS: [&str; 9] = ["components", "pages", "views", "screens", "ui", "routes", "layouts", "styles", "templates"];
    const CODE_EXT: [&str; 14] = ["ts", "js", "mjs", "cjs", "rs", "py", "go", "rb", "java", "php", "cs", "ex", "sql", "graphql"];
    let mut code = false;
    for f in files {
        let f = f.as_ref();
        let path = Path::new(f);
        let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        let in_ui_dir = path.components().any(|c| UI_DIRS.contains(&c.as_os_str().to_string_lossy().to_lowercase().as_str()));
        let test = f.contains(".test.") || f.contains(".spec.") || f.contains("__tests__") || f.contains("/tests/");
        if test {
            continue;
        }
        if UI_EXT.contains(&ext.as_str()) || (in_ui_dir && CODE_EXT.contains(&ext.as_str())) {
            return Need::Ui;
        }
        if CODE_EXT.contains(&ext.as_str()) {
            code = true;
        }
    }
    if code { Need::Code } else { Need::None }
}

/// Files changed since `base` (git) plus edits the session made after `upto`.
fn changed_files(input: &ChangesInput, base: Option<&str>, upto: usize) -> Vec<String> {
    let mut files: Vec<String> = Vec::new();
    for item in input.log.get(upto..).unwrap_or(&[]) {
        if let LogItem::Edit { path, .. } = item {
            let rel = Path::new(path).strip_prefix(&input.cwd).map(|p| p.display().to_string()).unwrap_or_else(|_| path.clone());
            if !files.contains(&rel) {
                files.push(rel);
            }
        }
    }
    if runs::is_repo(&input.cwd) {
        let base = base.unwrap_or("HEAD");
        for name in git_lines(&input.cwd, &["diff", "--name-only", base]).into_iter().chain(git_lines(&input.cwd, &["ls-files", "--others", "--exclude-standard"])) {
            if !files.contains(&name) {
                files.push(name);
            }
        }
    }
    // Tool state (.omc, .claude, .git…) and vendored deps are not the change.
    files.retain(|f| !Path::new(f).components().any(|c| {
        let c = c.as_os_str().to_string_lossy();
        (c.starts_with('.') && c.len() > 1 && c != "..") || c == "node_modules"
    }));
    files
}

fn git_lines(cwd: &Path, args: &[&str]) -> Vec<String> {
    std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_owned).collect())
        .unwrap_or_default()
}

// ------------------------------------------------------------------ jobs

struct Job {
    n: usize,
    started: Instant,
    reviewer: String,
    steps: Vec<String>,
    handle: AgentHandle,
    /// One more pass asked for while this one runs (only the latest is kept).
    queued: Option<(ChangesInput, Trigger)>,
    _pump: Task<()>,
    _ticker: Task<()>,
}

/// Running before/after passes by session key.
#[derive(Default)]
pub struct MediaJobs {
    jobs: HashMap<String, Job>,
}

impl Global for MediaJobs {}

fn ensure(cx: &mut App) {
    if !cx.has_global::<MediaJobs>() {
        cx.set_global(MediaJobs::default());
    }
}

/// Redraw views that show media when jobs change.
pub fn observe(cx: &mut Context<ChangesView>) {
    ensure(cx);
    cx.observe_global::<MediaJobs>(|_, cx| cx.notify()).detach();
}

pub struct Status {
    pub n: usize,
    pub elapsed: u64,
    pub reviewer: String,
    pub step: Option<String>,
    pub queued: bool,
}

pub fn status(key: &str, cx: &App) -> Option<Status> {
    let job = cx.try_global::<MediaJobs>()?.jobs.get(key)?;
    Some(Status {
        n: job.n,
        elapsed: job.started.elapsed().as_secs(),
        reviewer: job.reviewer.clone(),
        step: job.steps.last().cloned(),
        queued: job.queued.is_some(),
    })
}

/// A turn ended: with the setting on «после хода», UI edits since the last
/// pass get a before/after pass.
pub fn turn_finished(input: ChangesInput, cx: &mut App) {
    if crate::settings::config(cx).changes.media != MediaMode::Auto {
        return;
    }
    let prev = load_all(&input.key).into_iter().rev().find(|r| r.meta.error.is_none());
    let upto = prev.as_ref().map_or(0, |r| r.meta.upto);
    let edited_now = input.log.get(upto..).unwrap_or(&[]).iter().any(|i| matches!(i, LogItem::Edit { .. } | LogItem::Command(_)));
    if !edited_now || !runs::is_repo(&input.cwd) {
        return;
    }
    let base = prev.and_then(|r| r.meta.after).or_else(|| session_base(&input.key));
    if classify(&changed_files(&input, base.as_deref(), upto)) == Need::Ui {
        start(input, Trigger::Auto, cx);
    }
}

/// Start a pass now, or queue it behind the running one.
pub fn start(input: ChangesInput, trigger: Trigger, cx: &mut App) {
    ensure(cx);
    let key = input.key.clone();
    let mut now = None;
    cx.update_global::<MediaJobs, _>(|jobs, _| {
        let job = jobs.jobs.get_mut(&key);
        let running = job.is_some();
        let mut dummy = None;
        let slot = match job {
            Some(job) => &mut job.queued,
            None => &mut dummy,
        };
        now = queue_or_run(running, slot, (input, trigger));
    });
    if let Some((input, trigger)) = now {
        launch(input, trigger, cx);
    }
}

/// One pass per session: while one runs, the newest request waits in a single
/// slot (older waiting requests are replaced); otherwise it runs now.
fn queue_or_run<T>(running: bool, slot: &mut Option<T>, request: T) -> Option<T> {
    if running {
        *slot = Some(request);
        None
    } else {
        Some(request)
    }
}

pub fn cancel(key: &str, cx: &mut App) {
    let Some(job) = cx.try_global::<MediaJobs>().and_then(|j| j.jobs.get(key)) else { return };
    job.handle.interrupt();
    let key = key.to_owned();
    cx.update_global::<MediaJobs, _>(|jobs, _| {
        if let Some(job) = jobs.jobs.get_mut(&key) {
            job.queued = None;
        }
    });
    finish(&key, Some("Отменено".into()), cx);
}

fn launch(input: ChangesInput, trigger: Trigger, cx: &mut App) {
    let key = input.key.clone();
    let (n, dir) = match next_dir(&key) {
        Ok(v) => v,
        Err(err) => {
            eprintln!("stepan_verify: media: {err}");
            return;
        }
    };
    let prev = load_all(&key).into_iter().rev().find(|r| r.n != n && r.meta.error.is_none());
    let base = prev.as_ref().and_then(|r| r.meta.after.clone()).or_else(|| session_base(&key));
    let upto_prev = prev.as_ref().map_or(0, |r| r.meta.upto);
    let files = changed_files(&input, base.as_deref(), upto_prev);
    let provider = input.provider;
    let model = crate::settings::report_model(provider, cx);
    let reviewer = reviewer_name(provider, model.as_deref());
    let meta = Meta {
        at: Some(Utc::now()),
        trigger,
        base: base.clone(),
        after: runs::snapshot(&input.cwd),
        files: files.clone(),
        upto: input.log.len(),
        reviewer: reviewer.clone(),
        error: None,
    };
    save_meta(&dir, &meta);
    let Some(skill) = super::find_skill() else {
        let meta = Meta { error: Some("Скилл pr-before-after не найден (~/.stepan_verify/skills, ~/.claude/skills, ~/.agents/skills)".into()), ..meta };
        save_meta(&dir, &meta);
        cx.update_global::<MediaJobs, _>(|_, _| {});
        return;
    };
    if let Err(err) = fs::write(dir.join("brief.md"), brief(&input, &meta, &skill, &dir)) {
        save_meta(&dir, &Meta { error: Some(format!("Не удалось подготовить задание: {err}")), ..meta });
        cx.update_global::<MediaJobs, _>(|_, _| {});
        return;
    }
    let spec = SessionSpec {
        provider,
        native_id: None,
        transcript: None,
        cwd: input.cwd.clone(),
        // Starts dev servers, browsers and a worktree; must not stop to ask.
        mode: Mode::FullAccess,
        image_cache: Some(stepan_verify_core::Home::locate().state().join("images")),
        env: vec![("STEPAN_VERIFY_MEDIA_OUT".into(), dir.display().to_string())],
        model,
        instructions: None,
    };
    let (handle, events) = stepan_verify_agents::open(spec);
    handle.send(prompt(&dir));
    let pump_key = key.clone();
    let pump = cx.spawn(async move |cx| {
        while let Ok(event) = events.recv().await {
            let done = matches!(event, AgentEvent::TurnCompleted { .. } | AgentEvent::Exited);
            let key = pump_key.clone();
            let _ = cx.update(|cx| on_event(&key, event, cx));
            if done {
                break;
            }
        }
    });
    let tick_key = key.clone();
    let ticker = cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            let alive = cx.update(|cx| {
                let alive = cx.try_global::<MediaJobs>().is_some_and(|j| j.jobs.contains_key(&tick_key));
                if alive {
                    cx.update_global::<MediaJobs, _>(|_, _| {});
                }
                alive
            });
            if !alive {
                break;
            }
        }
    });
    let job = Job { n, started: Instant::now(), reviewer, steps: Vec::new(), handle, queued: None, _pump: pump, _ticker: ticker };
    cx.update_global::<MediaJobs, _>(|jobs, _| {
        jobs.jobs.insert(key, job);
    });
}

fn on_event(key: &str, event: AgentEvent, cx: &mut App) {
    match event {
        AgentEvent::Upsert(item) if item.kind == ItemKind::Tool && !item.title.is_empty() => {
            let step = humanize(&item.title);
            cx.update_global::<MediaJobs, _>(|jobs, _| {
                if let Some(job) = jobs.jobs.get_mut(key) {
                    if job.steps.last() != Some(&step) {
                        job.steps.push(step);
                    }
                }
            });
        }
        AgentEvent::TurnCompleted { error } => finish(key, error, cx),
        AgentEvent::Exited => finish(key, Some("Агент before/after завершился".into()), cx),
        AgentEvent::Error(err) => finish(key, Some(err), cx),
        _ => {}
    }
}

fn finish(key: &str, error: Option<String>, cx: &mut App) {
    let Some(job) = cx.try_global::<MediaJobs>().and_then(|j| j.jobs.get(key)).map(|j| j.n) else { return };
    let dir = media_dir(key).join(job.to_string());
    let manifest = fs::read_to_string(dir.join("manifest.json")).ok().map(|t| parse_manifest(&t));
    if !matches!(manifest, Some(Ok(_))) {
        let mut meta: Meta = fs::read_to_string(dir.join("meta.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        meta.error = Some(match (manifest, error) {
            (Some(Err(e)), _) => e,
            (_, Some(e)) => e,
            _ => "Агент закончил, но не записал manifest.json".into(),
        });
        save_meta(&dir, &meta);
    }
    let mut queued = None;
    cx.update_global::<MediaJobs, _>(|jobs, _| {
        if let Some(job) = jobs.jobs.remove(key) {
            queued = job.queued;
        }
    });
    if let Some((input, trigger)) = queued {
        launch(input, trigger, cx);
    }
}

fn humanize(title: &str) -> String {
    let known = [
        ("worktree add", "Поднимаю «Было» в отдельном worktree"),
        ("worktree remove", "Убираю worktree"),
        ("shoot.mjs", "Снимаю кадр"),
        ("pair.mjs", "Склеиваю пару «Было → Стало»"),
        ("record.mjs", "Записываю сценарий"),
        ("diagram.mjs", "Рисую схему"),
        ("ffmpeg", "Собираю видео"),
        ("npm install", "Ставлю зависимости"),
        ("pnpm install", "Ставлю зависимости"),
        ("bun install", "Ставлю зависимости"),
        ("npm run dev", "Запускаю dev-сервер"),
        ("pnpm dev", "Запускаю dev-сервер"),
        ("vite", "Запускаю dev-сервер"),
        ("manifest.json", "Пишу manifest"),
        ("SKILL.md", "Читаю скилл pr-before-after"),
        ("brief.md", "Читаю задание"),
        ("git diff", "Смотрю, что изменилось"),
    ];
    known.iter().find(|(needle, _)| title.contains(needle)).map(|(_, l)| (*l).to_owned()).unwrap_or_else(|| title.chars().take(90).collect())
}

// ------------------------------------------------------------------ prompt

fn brief(input: &ChangesInput, meta: &Meta, skill: &Path, out: &Path) -> String {
    let base = meta.base.as_deref().unwrap_or("HEAD");
    let stat = git_lines(&input.cwd, &["diff", "--stat", base]).join("\n");
    let report = runs::load_all(&super::out_dir(&input.key)).into_iter().rev().find(|r| r.ok().is_some()).map(|r| r.dir.join("report.json"));
    let request: String = input
        .log
        .iter()
        .rev()
        .find_map(|i| if let LogItem::Request(t) = i { Some(t.clone()) } else { None })
        .unwrap_or_default()
        .chars()
        .take(1500)
        .collect();
    let files = if meta.files.is_empty() { "(git не показал изменений — посмотри сам)".into() } else { meta.files.iter().map(|f| format!("- {f}")).collect::<Vec<_>>().join("\n") };
    format!(
        "# Задание: «Было → Стало» для сессии агента\n\n\
         Сессия: {title}\nПроект (рабочее дерево, «Стало»): {cwd}\n\
         База «Было» (коммит, из которого поднимать старое состояние): {base}\n\
         Скилл: {skill}/SKILL.md, скрипты: {skill}/scripts\n\
         Папка результата: {out}\n\
         Карта изменений ревьюера (если есть): {report}\n\n\
         ## Последний запрос пользователя к агенту\n\n{request}\n\n\
         ## Изменённые файлы с прошлого прохода\n\n{files}\n\n```\n{stat}\n```\n",
        title = input.title,
        cwd = input.cwd.display(),
        skill = skill.display(),
        out = out.display(),
        report = report.map(|p| p.display().to_string()).unwrap_or_else(|| "нет".into()),
    )
}

const MANIFEST: &str = r#"{"summary": "1–2 предложения: что видно на кадрах",
 "items": [{"kind": "image|gif|video", "pair": "01-сцена.png (карточка pair.mjs, если склеивал)",
            "before": "01-сцена-before.png|gif|mp4", "after": "01-сцена-after.png|gif|mp4",
            "caption": "что изменилось — коротко", "scenario": "маршрут или сценарий", "why": "какое требование подтверждает"}],
 "notes": "почему чего-то нет (нет «Было», изменение не видно и т.п.), иначе пусто"}"#;

fn prompt(out: &Path) -> String {
    format!(
        "[stepan_verify media] Ты — фотограф изменений в stepan_verify. Агент в этом проекте закончил ход; покажи человеку, что изменилось, скриншотами и записью «Было → Стало».\n\n\
         Прочитай {out}/brief.md (что поменялось, база, где скилл), затем SKILL.md скилла pr-before-after по указанному пути и делай по нему шаги 1–5 \
         (сцены, «Было» из базы, «Стало» с теми же параметрами, пары pair.mjs --diff, GIF record.mjs для изменений взаимодействия, \
         картинки вывода/диаграммы — если UI нет). Шаги 6–8 (publish, PR) НЕ делай.\n\n\
         Жёсткие правила:\n\
         - Не создавай PR, не коммить, не пушь, не запускай publish.mjs.\n\
         - Не трогай рабочее дерево проекта: никаких git stash / checkout / reset / clean в нём. «Было» поднимай так: \
         `git worktree add --detach \"$TMPDIR/stepan_verify-before-$$\" <база>`, ставь там зависимости (если lock-файл не менялся — можно симлинк node_modules из рабочего дерева), \
         запускай обе версии на свободных портах. В конце обязательно останови все запущенные тобой процессы (dev-серверы, браузеры) по PID и выполни `git worktree remove --force` для своего worktree.\n\
         - Язык подписей и manifest — русский, ни о чём не спрашивай (запуск фоновый).\n\
         - Не подделывай «Было». Если честно поднять его нельзя или изменение не видно — так и напиши в notes.\n\
         - Не больше 4 сцен, не дольше ~10 минут. Данные — из сидов/фикстур, без секретов и персональных данных.\n\
         - Все файлы клади в {out}: сырые кадры в {out}/raw, карточки пар и GIF/MP4 — в {out} с именами NN-сцена(-before|-after).ext.\n\n\
         В конце запиши {out}/manifest.json (пути — относительно {out}) по схеме:\n{MANIFEST}\n\
         Когда manifest записан и процессы остановлены — закончи ход одной строкой итога.",
        out = out.display()
    )
}

// ------------------------------------------------------------------ view

fn ago_short(at: Option<DateTime<Utc>>) -> String {
    at.map(|t| t.with_timezone(&chrono::Local).format("%H:%M").to_string()).unwrap_or_default()
}

fn resolve(dir: &Path, p: &str) -> PathBuf {
    let path = Path::new(p);
    if path.is_absolute() { path.to_owned() } else { dir.join(path) }
}

fn is_video(path: &Path) -> bool {
    path.extension().is_some_and(|e| matches!(e.to_string_lossy().to_lowercase().as_str(), "mp4" | "webm" | "mov" | "m4v"))
}

/// «Было → Стало» section of the «Изменения» tab.
pub fn render(input: &ChangesInput, window: &mut Window, cx: &mut Context<ChangesView>) -> AnyElement {
    let theme = cx.theme().clone();
    let p = Palette::of(&theme);
    let key = input.key.clone();
    let runs = load_all(&key);
    let running = status(&key, cx);
    let mode = crate::settings::config(cx).changes.media;
    if runs.is_empty() && running.is_none() && mode == MediaMode::Off {
        return div().into_any_element();
    }
    let selected = window.use_keyed_state(SharedString::from(format!("media-sel:{key}")), cx, |_, _| None::<usize>);
    let sel_n = *selected.read(cx);
    let shown = sel_n.and_then(|n| runs.iter().find(|r| r.n == n)).or_else(|| runs.iter().rev().find(|r| matches!(r.manifest, Some(Ok(_))))).or(runs.last());

    let action = match &running {
        Some(_) => {
            let k = key.clone();
            PressButton::new("media-cancel").small().icon(IconName::Close).label("Остановить").on_click(move |_, _, cx| cancel(&k, cx))
        }
        None => {
            let again = input.clone();
            PressButton::new("media-shoot").small().icon(IconName::Frame).label("Снять before/after").on_click(move |_, _, cx| start(again.clone(), Trigger::Manual, cx))
        }
    };
    let hint = match mode {
        MediaMode::Auto => "снимается сам после хода с правками UI",
        MediaMode::Manual => "только по кнопке",
        MediaMode::Off => "автосъёмка выключена в настройках",
    };

    v_flex()
        .gap(px(12.))
        .child(
            h_flex()
                .gap_3()
                .child(div().flex_1().child(ui::section_title("Было → Стало", (!runs.is_empty()).then(|| runs.len().to_string()), &p, &theme)))
                .child(ui::mono(hint, &theme).text_color(p.faint))
                .child(action),
        )
        .when_some(running.as_ref(), |this, s| {
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
                    .child(div().flex_1().min_w_0().truncate().child(format!(
                        "#{} снимается · {} · {}:{:02} · {}{}",
                        s.n,
                        s.reviewer,
                        s.elapsed / 60,
                        s.elapsed % 60,
                        s.step.clone().unwrap_or_else(|| "читает задание".into()),
                        if s.queued { " · следующий в очереди" } else { "" }
                    ))),
            )
        })
        .when(runs.len() > 1, |this| {
            this.child(h_flex().flex_wrap().gap(px(6.)).children(runs.iter().map(|r| {
                let active = shown.is_some_and(|s| s.n == r.n);
                let frames = match &r.manifest {
                    Some(Ok(m)) => format!("{} сц.", m.items.len()),
                    _ if r.meta.error.is_some() => "ошибка".into(),
                    _ => "…".into(),
                };
                let sel = selected.clone();
                let n = r.n;
                h_flex()
                    .id(SharedString::from(format!("media-run-{n}")))
                    .gap(px(6.))
                    .px(px(9.))
                    .h(px(26.))
                    .rounded(px(6.))
                    .border_1()
                    .border_color(if active { p.border_strong } else { p.border })
                    .bg(if active { p.surface } else { p.sunken })
                    .cursor_pointer()
                    .on_click(move |_, _, cx| sel.update(cx, |v, cx| {
                        *v = Some(n);
                        cx.notify();
                    }))
                    .child(ui::mono(format!("#{n}"), &theme).text_color(p.ink))
                    .child(ui::mono(format!("{} · {} · {frames}", ago_short(r.meta.at), if r.meta.trigger == Trigger::Auto { "авто" } else { "вручную" }), &theme).text_color(p.muted))
            })))
        })
        .map(|this| match shown {
            None if running.is_none() => this.child(
                div()
                    .px(px(14.))
                    .py(px(12.))
                    .rounded(px(10.))
                    .border_1()
                    .border_dashed()
                    .border_color(p.border_strong)
                    .text_size(px(12.5))
                    .text_color(p.muted)
                    .child("Скриншотов ещё нет. После хода агента с правками интерфейса они снимутся сами (скилл pr-before-after), или нажми «Снять before/after»."),
            ),
            None => this,
            Some(run) => this.child(render_run(run, &input.cwd, &p, &theme)),
        })
        .into_any_element()
}

fn render_run(run: &Run, cwd: &Path, p: &Palette, theme: &Theme) -> AnyElement {
    let err = run.meta.error.clone().or_else(|| match &run.manifest {
        Some(Err(e)) => Some(e.clone()),
        _ => None,
    });
    let Some(Ok(m)) = &run.manifest else {
        return match err {
            Some(e) => div()
                .px(px(12.))
                .py(px(10.))
                .rounded(px(8.))
                .bg(p.red.soft)
                .text_size(px(12.5))
                .text_color(p.red.text)
                .child(format!("#{} не получилось: {e}", run.n))
                .into_any_element(),
            None => div().into_any_element(),
        };
    };
    v_flex()
        .gap(px(14.))
        .when(!m.summary.is_empty(), |this| this.child(div().text_size(px(13.5)).text_color(p.fg).child(m.summary.clone())))
        .children(m.items.iter().enumerate().map(|(i, item)| render_item(run, i, item, cwd, p, theme)))
        .when(!m.notes.is_empty(), |this| {
            this.child(
                h_flex()
                    .gap_2()
                    .items_start()
                    .text_size(px(12.5))
                    .text_color(p.muted)
                    .child(Icon::new(IconName::Info).small().text_color(p.faint))
                    .child(div().flex_1().child(m.notes.clone())),
            )
        })
        .when(m.items.is_empty() && m.notes.is_empty(), |this| this.child(div().text_size(px(12.5)).text_color(p.muted).child("Агент не нашёл, что показать.")))
        .into_any_element()
}

fn render_item(run: &Run, i: usize, item: &Item, cwd: &Path, p: &Palette, theme: &Theme) -> AnyElement {
    let tile = |label: &'static str, rel: &str, wide: bool| -> AnyElement {
        let path = resolve(&run.dir, rel);
        let (open, root) = (path.clone(), cwd.to_owned());
        let id = SharedString::from(format!("media-{}-{i}-{label}", run.n));
        let body = if !path.exists() {
            div().h(px(160.)).w_full().flex().items_center().justify_center().bg(p.surface2).text_size(px(12.)).text_color(p.faint).child("нет файла").into_any_element()
        } else if is_video(&path) {
            v_flex()
                .h(px(160.))
                .w_full()
                .items_center()
                .justify_center()
                .gap_2()
                .bg(p.ink)
                .child(Icon::new(IconName::Play).large().text_color(p.surface))
                .child(ui::mono(path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), theme).text_color(p.surface.opacity(0.8)))
                .into_any_element()
        } else {
            div()
                .w_full()
                .flex()
                .justify_center()
                .bg(p.surface2)
                .child(crate::media::picture(SharedString::from(format!("{id}-{}", path.display())), path.clone(), if wide { 1040. } else { 520. }, 460.))
                .into_any_element()
        };
        v_flex()
            .id(id)
            .flex_1()
            .min_w_0()
            .rounded(px(12.))
            .border_1()
            .border_color(p.border)
            .bg(p.surface)
            .shadow(ui::card_shadow())
            .overflow_hidden()
            .cursor_pointer()
            .on_click(move |_, window, cx| crate::preview::open_in_app(open.clone(), Some(root.clone()), window, cx))
            .child(body)
            .child(
                h_flex()
                    .px(px(12.))
                    .py(px(7.))
                    .gap(px(8.))
                    .border_t_1()
                    .border_color(p.border)
                    .child(div().text_size(px(12.5)).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).child(label)),
            )
            .into_any_element()
    };
    let kind = match item.kind {
        MediaKind::Image => "кадр",
        MediaKind::Gif => "запись",
        MediaKind::Video => "видео",
    };
    v_flex()
        .gap(px(8.))
        .child(
            h_flex()
                .gap_2()
                .child(ui::badge(kind, p.blue))
                .child(div().min_w_0().truncate().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).child(item.caption.clone()))
                .when(!item.scenario.is_empty(), |this| this.child(ui::mono(item.scenario.clone(), theme).text_color(p.faint))),
        )
        .when(!item.why.is_empty(), |this| this.child(div().text_size(px(12.5)).text_color(p.muted).child(item.why.clone())))
        .map(|this| match (&item.pair, &item.before, &item.after) {
            (Some(pair), _, _) if item.kind == MediaKind::Image => this.child(tile("Было → Стало", pair, true)),
            (_, before, after) => this.child(
                h_flex()
                    .gap(px(12.))
                    .items_center()
                    .child(match before {
                        Some(b) => tile("Было", b, false),
                        None => div().flex_1().h(px(120.)).flex().items_center().justify_center().rounded(px(12.)).border_1().border_dashed().border_color(p.border_strong).text_size(px(12.)).text_color(p.faint).child("«Было» нет").into_any_element(),
                    })
                    .child(Icon::new(IconName::ArrowRight).small().text_color(p.faint))
                    .when_some(after.clone(), |this, a| this.child(tile("Стало", &a, false))),
            ),
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[core::prelude::v1::test]
    fn ui_files_need_media() {
        assert_eq!(classify(&["src/components/Card.tsx"]), Need::Ui);
        assert_eq!(classify(&["app/styles/main.scss", "README.md"]), Need::Ui);
        assert_eq!(classify(&["src/pages/index.ts"]), Need::Ui);
        assert_eq!(classify(&["server/api/users.ts", "Cargo.toml"]), Need::Code);
        assert_eq!(classify(&["src/components/Card.test.tsx"]), Need::None);
        assert_eq!(classify(&["README.md", "docs/plan.md"]), Need::None);
        assert_eq!(classify::<&str>(&[]), Need::None);
    }

    #[core::prelude::v1::test]
    fn manifest_object_or_list() {
        let obj = r#"{"summary":"кнопка","items":[{"kind":"gif","before":"a.gif","after":"b.gif","caption":"c","route":"/x"}],"notes":""}"#;
        let m = parse_manifest(obj).unwrap();
        assert_eq!(m.summary, "кнопка");
        assert_eq!(m.items[0].kind, MediaKind::Gif);
        assert_eq!(m.items[0].scenario, "/x");
        let list = r#"[{"kind":"image","pair":"01.png","caption":"c"}]"#;
        let m = parse_manifest(list).unwrap();
        assert_eq!(m.items[0].pair.as_deref(), Some("01.png"));
        assert!(parse_manifest("{oops").is_err());
    }

    #[core::prelude::v1::test]
    fn queue_keeps_one_latest() {
        let mut slot = None;
        assert_eq!(queue_or_run(false, &mut slot, 1), Some(1));
        assert_eq!(queue_or_run(true, &mut slot, 2), None);
        assert_eq!(queue_or_run(true, &mut slot, 3), None);
        assert_eq!(slot, Some(3));
    }
}
