//! Which features you and your agents use, and which nobody touches.
//!
//! Two sources, one per feature, so nothing is counted twice:
//! - disk: what Stepan already keeps with dates — the board journal, document
//!   comments, memory, «Изменения» runs and «Просмотрено» marks, session
//!   summaries, skill calls. Their whole history counts from day one.
//! - live: clicks that leave no file behind (pages, tabs, previews, zoom),
//!   written to the `usage` table of `state/stepan.db` as they happen.
//!
//! Nothing leaves the machine.

use std::{
    collections::HashMap,
    path::Path,
    sync::{Mutex, OnceLock},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Result, bail};
use chrono::{DateTime, Local, TimeZone as _, Utc};
use serde_json::Value;

use crate::{Home, Store, Task, events};

/// Who used a feature: you, or an agent / Stepan on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Actor {
    Me,
    Agent,
}

impl Actor {
    fn as_str(self) -> &'static str {
        match self {
            Actor::Me => "me",
            Actor::Agent => "agent",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// Recorded as it happens ([`hit`]); history starts with 0.2.10.
    Live,
    /// Read from Stepan's own files ([`from_disk`]); full history.
    Disk,
}

#[derive(Debug)]
pub struct Feature {
    pub id: &'static str,
    pub group: &'static str,
    pub name: &'static str,
    /// What it is, in a few words.
    pub hint: &'static str,
    /// The release it came in ("0.1" for everything before 0.2).
    pub since: &'static str,
    pub source: Source,
}

pub const GROUPS: [&str; 8] = ["Сессии", "Задачи", "Документы", "Изменения", "Память", "Флоу", "Скиллы", "Вид"];

const fn f(id: &'static str, group: &'static str, name: &'static str, hint: &'static str, since: &'static str, source: Source) -> Feature {
    Feature { id, group, name, hint, since, source }
}

use Source::{Disk, Live};

pub const FEATURES: &[Feature] = &[
    f("page.dashboard", "Сессии", "Дашборд", "главная: живые сессии и что ждёт тебя", "0.1", Live),
    f("sessions.open", "Сессии", "Сессия во вкладке", "открыть разговор агента в Stepan", "0.1", Live),
    f("sessions.new", "Сессии", "Новая сессия", "запуск Claude или Codex из Stepan", "0.1", Live),
    f("sessions.send", "Сессии", "Сообщение агенту", "написать агенту из окна сессии", "0.1", Live),
    f("sessions.preview", "Сессии", "Превью HTML", "show-me и другие HTML в панели превью", "0.2.6", Live),
    f("sessions.queue", "Сессии", "Сообщение в очередь", "написал, пока агент работает", "0.2.12", Live),
    f("sessions.send_now", "Сессии", "«Отправить сейчас»", "сообщение из очереди без ожидания", "0.2.12", Live),
    f("sessions.resume_interrupted", "Сессии", "«Продолжить» после обрыва", "агент умер посреди хода", "0.2.12", Live),
    f("page.tasks", "Задачи", "Доска задач", "страница «Задачи»", "0.1", Live),
    f("tasks.create", "Задачи", "Новая задача", "задача на доске", "0.1", Disk),
    f("tasks.plan_first", "Задачи", "«Сначала план»", "задача начинается с плана", "0.2.9", Disk),
    f("tasks.worktree", "Задачи", "Отдельная копия", "задача в своём git worktree", "0.2.8", Disk),
    f("tasks.claim", "Задачи", "Агент взял задачу", "claim с доски", "0.1", Disk),
    f("tasks.complete", "Задачи", "Сдача на ревью", "агент закончил задачу", "0.1", Disk),
    f("tasks.block", "Задачи", "Вопрос человеку", "агент остановился и спросил", "0.1", Disk),
    f("tasks.status", "Задачи", "Перенос по колонкам", "статус задачи руками", "0.1", Disk),
    f("tasks.plan_ready", "Задачи", "План готов", "агент сдал план задачи", "0.2.9", Disk),
    f("tasks.plan_open", "Задачи", "Открыть план", "карта «станет» у задачи", "0.2.9", Live),
    f("tasks.plan_approved", "Задачи", "Утвердить план", "старт реализации по плану", "0.2.9", Disk),
    f("page.docs", "Документы", "Полка", "страница «Документы»", "0.1", Live),
    f("docs.comment", "Документы", "Комментарий к документу", "тред под блоком или цитатой", "0.2.9", Disk),
    f("docs.send", "Документы", "Отправить агенту", "комментарии ушли в сессию", "0.2.9", Disk),
    f("docs.resolve", "Документы", "Закрыть тред", "комментарий решён", "0.2.9", Disk),
    f("docs.versions", "Документы", "Дифф версии", "что поменялось в документе", "0.2.9", Live),
    f("docs.distill", "Документы", "Итоги сессии", "дистиллятор пишет решения и факты", "0.1", Disk),
    f("changes.open", "Изменения", "Открыть «Изменения»", "отчёт о том, что сделала сессия", "0.1", Live),
    f("changes.run", "Изменения", "Собрать карту", "прогон ревьюера (ты — полный, сам — батч)", "0.1", Disk),
    f("changes.follow", "Изменения", "Следить за сессией", "карта обновляется после каждого хода", "0.1", Live),
    f("changes.arch", "Изменения", "Вкладка «Архитектура»", "клик по вкладке; она же открыта по умолчанию", "0.2.9", Live),
    f("changes.contracts", "Изменения", "Вкладка «Контракты»", "типы, сигнатуры, зависимости", "0.2.9", Live),
    f("changes.media", "Изменения", "Вкладка «До / После»", "скриншоты и GIF", "0.2.9", Live),
    f("changes.map", "Изменения", "Карта-канвас", "старый холст изменений", "0.1", Live),
    f("changes.views", "Изменения", "«Как устроено»", "деревья, псевдокод, было → стало", "0.2.6", Live),
    f("changes.reviewed", "Изменения", "«Просмотрено»", "отметка на узле карты", "0.2.9", Disk),
    f("changes.only_new", "Изменения", "«Только новое»", "скрыть просмотренное", "0.2.9", Live),
    f("page.memory", "Память", "Страница «Память»", "что агенты помнят о тебе", "0.2.0", Live),
    f("memory.add", "Память", "Запись в память", "новое правило или факт", "0.2.0", Disk),
    f("memory.edit", "Память", "Правка памяти", "исправить или удалить запись", "0.2.0", Disk),
    f("page.flows", "Флоу", "Карта сценариев", "страница «Флоу»", "0.2.0", Live),
    f("flows.check", "Флоу", "flows check", "проверка карты сценариев из CLI", "0.2.0", Live),
    f("page.skills", "Скиллы", "Страница «Скиллы»", "какие скиллы есть и где", "0.1", Live),
    f("skills.show_me", "Скиллы", "/show-me", "наглядные виды кода в сессии", "0.2.6", Disk),
    f("skills.describe_pr", "Скиллы", "/describe-pr", "PR и цикл CI", "0.2.8", Disk),
    f("page.settings", "Вид", "Настройки", "страница «Настройки»", "0.1", Live),
    f("app.zoom", "Вид", "Масштаб ⌘+ / ⌘−", "размер интерфейса", "0.2.4", Live),
    f("app.update", "Вид", "Обновление из окна", "кнопка «Обновить»", "0.1", Live),
    f("page.usage", "Вид", "«Чем пользуюсь»", "эта страница", "0.2.10", Live),
];

pub fn feature(id: &str) -> Option<&'static Feature> {
    FEATURES.iter().find(|f| f.id == id)
}

// ------------------------------------------------------------------ live

static STORE: OnceLock<Store> = OnceLock::new();
/// Last write per feature: a double click or a re-render is one use.
static RECENT: OnceLock<Mutex<HashMap<&'static str, Instant>>> = OnceLock::new();
const SAME_USE_SECS: u64 = 2;

const SINCE_KEY: &str = "usage.since";

/// The app's store; [`hit`] writes nothing before this. The first call remembers
/// when live counting began.
pub fn init(store: Store) {
    if store.get::<i64>(SINCE_KEY).is_none() {
        let _ = store.put(SINCE_KEY, &now());
    }
    let _ = STORE.set(store);
}

/// When live counting began on this machine.
pub fn live_since(store: &Store) -> Option<i64> {
    store.get(SINCE_KEY)
}

/// You used a live feature.
pub fn hit(id: &'static str) {
    debug_assert!(feature(id).is_some_and(|f| f.source == Live), "usage: unknown or disk feature {id}");
    let Some(store) = STORE.get() else { return };
    if !fresh(id) {
        return;
    }
    if let Err(err) = store.add_usage(now(), id, Actor::Me.as_str()) {
        eprintln!("stepan_verify: usage {id}: {err:#}");
    }
}

fn fresh(id: &'static str) -> bool {
    let mut recent = RECENT.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    if recent.get(id).is_some_and(|t| now.duration_since(*t).as_secs() < SAME_USE_SECS) {
        return false;
    }
    recent.insert(id, now);
    true
}

/// A live feature used from the command line, by you or by an agent's shell.
pub fn hit_cli(home: &Home, id: &'static str) {
    let actor = if cli_is_agent() { Actor::Agent } else { Actor::Me };
    if let Ok(store) = Store::open(home) {
        let _ = store.add_usage(now(), id, actor.as_str());
    }
}

/// Agents Stepan starts get STEPAN_VERIFY_AGENT; Claude Code and Codex mark their shells too.
fn cli_is_agent() -> bool {
    ["STEPAN_VERIFY_AGENT", "CLAUDECODE", "CODEX_SANDBOX", "CODEX_THREAD_ID"].iter().any(|v| std::env::var_os(v).is_some())
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

// ------------------------------------------------------------------ disk

/// One use of a feature.
#[derive(Clone, Debug, PartialEq)]
pub struct Use {
    pub at: i64,
    pub feature: &'static str,
    pub actor: Actor,
}

fn actor_of(who: &str) -> Actor {
    match who {
        "me" | "ты" | "" => Actor::Me,
        _ => Actor::Agent,
    }
}

fn ts(v: &Value) -> Option<i64> {
    v.as_str()?.parse::<DateTime<Utc>>().ok().map(|t| t.timestamp())
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn jsonl(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path).map(|t| t.lines().filter_map(|l| serde_json::from_str(l).ok()).collect()).unwrap_or_default()
}

fn subdirs(dir: &Path) -> Vec<std::path::PathBuf> {
    std::fs::read_dir(dir).map(|r| r.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect()).unwrap_or_default()
}

/// Every use Stepan's own files remember.
pub fn from_disk(home: &Home) -> Vec<Use> {
    let mut out = Vec::new();
    let mut push = |at: Option<i64>, feature: &'static str, actor: Actor| {
        if let Some(at) = at {
            out.push(Use { at, feature, actor });
        }
    };

    // The board journal: who created, took, finished, moved and planned what.
    let mut created: HashMap<String, i64> = HashMap::new();
    for e in events::read(home, None) {
        let at = e.at.timestamp();
        if e.kind == "create" {
            if let Some(t) = &e.task {
                created.insert(t.clone(), at);
            }
        }
        let feature = match e.kind.as_str() {
            "create" => "tasks.create",
            "claim" => "tasks.claim",
            "complete" => "tasks.complete",
            "block" => "tasks.block",
            "status" => "tasks.status",
            "plan_ready" => "tasks.plan_ready",
            "plan_approved" => "tasks.plan_approved",
            _ => continue,
        };
        push(Some(at), feature, actor_of(&e.actor));
    }
    for t in Task::load_all(&home.tasks()) {
        let at = created.get(&t.id).copied().or_else(|| mtime(&t.path));
        if t.meta.plan {
            push(at, "tasks.plan_first", Actor::Me);
        }
        if t.meta.worktree.is_some() {
            push(at, "tasks.worktree", Actor::Me);
        }
    }

    // Document comments: state/doc-comments/<rel>.jsonl.
    for file in walk(&home.state().join("doc-comments"), "jsonl") {
        for op in jsonl(&file) {
            match op["op"].as_str() {
                Some("add") => push(ts(&op["created_at"]), "docs.comment", actor_of(op["author"].as_str().unwrap_or(""))),
                Some("sent") => push(ts(&op["at"]), "docs.send", Actor::Me),
                Some("resolve") => push(ts(&op["at"]), "docs.resolve", actor_of(op["by"].as_str().unwrap_or(""))),
                _ => {}
            }
        }
    }

    // Memory: memory/log.jsonl. «авто» is the session distiller.
    for op in jsonl(&home.memory().join("log.jsonl")) {
        let feature = match op["op"].as_str() {
            Some("add") => "memory.add",
            Some("edit" | "remove") => "memory.edit",
            _ => continue,
        };
        push(ts(&op["at"]), feature, actor_of(op["by"].as_str().unwrap_or("")));
    }

    // «Изменения»: runs and «Просмотрено» marks per session.
    for session in subdirs(&home.state().join("changes")) {
        for run in subdirs(&session.join("runs")) {
            if let Some(meta) = read_json(&run.join("meta.json")) {
                let actor = if meta["kind"] == "batch" { Actor::Agent } else { Actor::Me };
                push(ts(&meta["at"]), "changes.run", actor);
            }
        }
        if let Some(Value::Object(marks)) = read_json(&session.join("reviewed.json")) {
            for mark in marks.values() {
                push(ts(&mark["at"]), "changes.reviewed", Actor::Me);
            }
        }
    }

    // Session summaries: state/distill/<session>/state.json.
    for session in subdirs(&home.state().join("distill")) {
        if let Some(state) = read_json(&session.join("state.json")) {
            for pass in state["passes"].as_array().into_iter().flatten() {
                push(ts(&pass["at"]), "docs.distill", Actor::Agent);
            }
        }
    }

    // Skill calls the skills page found in transcripts: you typed /name, or the model called it.
    if let Some(index) = read_json(&home.state().join("skills-usage.json")) {
        for file in index["files"].as_object().into_iter().flat_map(|m| m.values()) {
            for u in file["uses"].as_array().into_iter().flatten() {
                let feature = match u["skill"].as_str() {
                    Some("show-me") => "skills.show_me",
                    Some("describe-pr") => "skills.describe_pr",
                    _ => continue,
                };
                let actor = if u["method"] == "Command" { Actor::Me } else { Actor::Agent };
                push(ts(&u["at"]), feature, actor);
            }
        }
    }
    out
}

fn mtime(path: &Path) -> Option<i64> {
    let t = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(t.duration_since(UNIX_EPOCH).ok()?.as_secs() as i64)
}

fn walk(dir: &Path, ext: &str) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == ext) {
                out.push(p);
            }
        }
    }
    out
}

/// Disk history plus live uses from the store.
pub fn collect(home: &Home, store: Option<&Store>) -> Vec<Use> {
    let mut uses = from_disk(home);
    if let Some(store) = store {
        for (at, id, actor) in store.usage_since(0) {
            if let Some(f) = feature(&id) {
                uses.push(Use { at, feature: f.id, actor: if actor == "agent" { Actor::Agent } else { Actor::Me } });
            }
        }
    }
    uses
}

// ---------------------------------------------------------------- report

/// Days in the sparkline.
pub const DAYS: usize = 30;

#[derive(Debug)]
pub struct Row {
    pub feature: &'static Feature,
    pub last: Option<i64>,
    /// Your uses over the last 7 and 30 days.
    pub me: [u32; 2],
    /// Agents' (and Stepan's own) uses over the last 7 and 30 days.
    pub agent: [u32; 2],
    /// Uses per local day, oldest first, today last.
    pub days: [u32; DAYS],
}

impl Row {
    pub fn total30(&self) -> u32 {
        self.me[1] + self.agent[1]
    }

    /// Nobody touched it for `days`. A click-counted feature with no uses is
    /// not idle until live counting (`live_since`) has run that long.
    pub fn idle(&self, now: i64, days: i64, live_since: Option<i64>) -> bool {
        match self.last {
            Some(t) => now - t > days * 86_400,
            None if self.feature.source == Live => live_since.is_some_and(|s| now - s > days * 86_400),
            None => true,
        }
    }
}

/// One row per feature, in [`FEATURES`] order.
pub fn report(uses: &[Use], now: i64) -> Vec<Row> {
    let today = local_day(now);
    let mut rows: Vec<Row> = FEATURES.iter().map(|feature| Row { feature, last: None, me: [0; 2], agent: [0; 2], days: [0; DAYS] }).collect();
    let index: HashMap<&str, usize> = FEATURES.iter().enumerate().map(|(i, f)| (f.id, i)).collect();
    for u in uses {
        let Some(&i) = index.get(u.feature) else { continue };
        let row = &mut rows[i];
        row.last = row.last.max(Some(u.at));
        let age = now - u.at;
        let counts = match u.actor {
            Actor::Me => &mut row.me,
            Actor::Agent => &mut row.agent,
        };
        if (0..7 * 86_400).contains(&age) {
            counts[0] += 1;
        }
        if (0..30 * 86_400).contains(&age) {
            counts[1] += 1;
        }
        let back = (today - local_day(u.at)).num_days();
        if (0..DAYS as i64).contains(&back) {
            row.days[DAYS - 1 - back as usize] += 1;
        }
    }
    rows
}

fn local_day(at: i64) -> chrono::NaiveDate {
    Local.timestamp_opt(at, 0).single().map_or_else(chrono::NaiveDate::default, |t| t.date_naive())
}

// ------------------------------------------------------------------- cli

pub const USAGE: &str = "\
stepan_verify usage — чем пользуешься ты и агенты (всё локально)

  stepan_verify usage                 все фичи: за 7 и 30 дней, когда последний раз
  stepan_verify usage --idle [DAYS]   только то, чем не пользовались DAYS дней (14)
";

pub fn cli(home: &Home, args: &[String]) -> Result<String> {
    let idle = match args.first().map(String::as_str) {
        None => None,
        Some("--idle") => Some(args.get(1).map(|d| d.parse::<i64>()).transpose()?.unwrap_or(14)),
        Some("help" | "--help" | "-h") => return Ok(USAGE.to_owned()),
        Some(other) => bail!("не знаю {other}\n\n{USAGE}"),
    };
    let store = Store::open(home).ok();
    let now = now();
    let since = store.as_ref().and_then(live_since);
    let rows = report(&collect(home, store.as_ref()), now);
    let mut out = String::from("фича                                 ты 7д/30д  агенты 7д/30д  последний раз\n");
    for group in GROUPS {
        let part: Vec<&Row> = rows.iter().filter(|r| r.feature.group == group).filter(|r| idle.is_none_or(|d| r.idle(now, d, since))).collect();
        if part.is_empty() {
            continue;
        }
        out.push_str(&format!("\n{group}\n"));
        for r in part {
            let last = r.last.map_or_else(|| "никогда".to_owned(), |t| ago(now - t));
            let since = if r.feature.source == Live { " ·live" } else { "" };
            out.push_str(&format!(
                "  {:<35}{:>5}/{:<5}{:>8}/{:<6}  {last}{since}\n",
                r.feature.name,
                r.me[0],
                r.me[1],
                r.agent[0],
                r.agent[1]
            ));
        }
    }
    out.push_str("\n·live — считается с 0.2.10, у остальных вся история из файлов Stepan.");
    Ok(out)
}

/// «3 дн назад».
fn ago(secs: i64) -> String {
    match secs {
        s if s < 3600 => format!("{} мин назад", (s / 60).max(1)),
        s if s < 86_400 => format!("{} ч назад", s / 3600),
        s => format!("{} дн назад", s / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> Home {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("stepan-usage-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = Home { root };
        home.ensure().unwrap();
        home
    }

    #[test]
    fn ids_are_unique_and_grouped() {
        let mut ids: Vec<&str> = FEATURES.iter().map(|f| f.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), FEATURES.len());
        assert!(FEATURES.iter().all(|f| GROUPS.contains(&f.group)), "every feature sits in a group");
    }

    #[test]
    fn disk_history_reads_stepans_own_files() {
        let home = home();
        let state = home.state();
        let w = |path: std::path::PathBuf, text: &str| {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        w(state.join("events.jsonl"), concat!(
            r#"{"at":"2026-11-20T10:00:00Z","task":"t1","actor":"me","kind":"create"}"#, "\n",
            r#"{"at":"2026-11-20T10:05:00Z","task":"t1","actor":"claude","kind":"claim"}"#, "\n",
            r#"{"at":"2026-11-20T11:00:00Z","task":"t1","actor":"me","kind":"plan_approved"}"#, "\n",
            r#"{"at":"2026-11-20T11:00:00Z","task":"t1","actor":"me","kind":"log"}"#, "\n",
        ));
        w(state.join("doc-comments/iv3/plan.md.jsonl"), concat!(
            r#"{"op":"add","id":"a","text":"x","author":"ты","created_at":"2026-11-21T09:00:00Z"}"#, "\n",
            r#"{"op":"add","id":"b","text":"y","author":"claude","reply_to":"a","created_at":"2026-11-21T09:10:00Z"}"#, "\n",
            r#"{"op":"sent","id":"a","session":"s","at":"2026-11-21T09:01:00Z"}"#, "\n",
        ));
        w(home.memory().join("log.jsonl"), r#"{"at":"2026-11-22T08:00:00Z","by":"авто","op":"add","id":"m","kind":"rule","text":"t"}"#);
        w(state.join("changes/s1/runs/1/meta.json"), r#"{"at":"2026-11-23T08:00:00Z","kind":"full"}"#);
        w(state.join("changes/s1/runs/2/meta.json"), r#"{"at":"2026-11-23T09:00:00Z","kind":"batch"}"#);
        w(state.join("changes/s1/reviewed.json"), r#"{"n1":{"run":1,"fingerprint":"f","at":"2026-11-23T08:30:00Z"},"n2":{"run":1,"fingerprint":"g"}}"#);
        w(state.join("distill/s1/state.json"), r#"{"passes":[{"n":1,"at":"2026-11-24T08:00:00Z"}]}"#);
        w(state.join("skills-usage.json"), r#"{"files":{"/x.jsonl":{"uses":[{"skill":"show-me","method":"Command","at":"2026-11-24T09:00:00Z"},{"skill":"describe-pr","method":"Skill","at":"2026-11-24T10:00:00Z"},{"skill":"model","method":"Command","at":"2026-11-24T10:00:00Z"}]}}}"#);

        let mut got: Vec<(&str, Actor)> = from_disk(&home).into_iter().map(|u| (u.feature, u.actor)).collect();
        got.sort_by_key(|(f, a)| (*f, *a == Actor::Agent));
        assert_eq!(
            got,
            [
                ("changes.reviewed", Actor::Me),
                ("changes.run", Actor::Me),
                ("changes.run", Actor::Agent),
                ("docs.comment", Actor::Me),
                ("docs.comment", Actor::Agent),
                ("docs.distill", Actor::Agent),
                ("docs.send", Actor::Me),
                ("memory.add", Actor::Agent),
                ("skills.describe_pr", Actor::Agent),
                ("skills.show_me", Actor::Me),
                ("tasks.claim", Actor::Agent),
                ("tasks.create", Actor::Me),
                ("tasks.plan_approved", Actor::Me),
            ]
        );
        let _ = std::fs::remove_dir_all(&home.root);
    }

    #[test]
    fn live_uses_are_stored_and_counted_with_disk_ones() {
        let home = home();
        let store = Store::open(&home).unwrap();
        let now = now();
        store.add_usage(now - 60, "page.docs", "me").unwrap();
        store.add_usage(now - 10 * 86_400, "page.docs", "me").unwrap();
        store.add_usage(now - 60, "flows.check", "agent").unwrap();
        store.add_usage(now - 60, "gone.feature", "me").unwrap();
        let rows = report(&collect(&home, Some(&store)), now);
        let row = |id| rows.iter().find(|r| r.feature.id == id).unwrap();
        assert_eq!((row("page.docs").me, row("page.docs").agent), ([1, 2], [0, 0]));
        assert_eq!(row("page.docs").days[DAYS - 1], 1, "today is the last bar");
        assert_eq!(row("flows.check").agent, [1, 1]);
        assert!(row("page.docs").last == Some(now - 60) && !row("page.docs").idle(now, 14, None));
        // No zoom yet: not idle while live counting is younger than 14 days.
        assert!(!row("app.zoom").idle(now, 14, Some(now - 86_400)) && row("app.zoom").idle(now, 14, Some(now - 20 * 86_400)));
        assert!(row("tasks.plan_first").idle(now, 14, None), "a disk feature with no history is idle at once");
        let _ = std::fs::remove_dir_all(&home.root);
    }

    #[test]
    fn a_double_click_is_one_use() {
        assert!(fresh("app.update"));
        assert!(!fresh("app.update"));
        assert!(fresh("app.zoom"));
    }
}
