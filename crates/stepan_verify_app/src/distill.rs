//! «Итоги сессии»: when a session goes quiet, a subagent of the same family
//! reads what happened since the last pass and writes what outlives the
//! session (decisions, project facts, open questions) into
//! `~/.stepan_verify/docs/<project>/`, by the same rules agents get at start.
//!
//! Per session: `~/.stepan_verify/state/distill/<key>/state.json` (how far the log was
//! read, past passes) and `runs/<n>/{context.md, result.json}`.

use std::{
    collections::HashMap,
    fs,
    hash::{DefaultHasher, Hash, Hasher},
    path::{Path, PathBuf},
    time::Duration,
};

use chrono::{DateTime, Utc};
use gpui_kit::*;
use stepan_verify_agents::{AgentEvent, AgentHandle, Mode, Provider, SessionSpec};
use stepan_verify_core::config::DistillMode;
use serde::{Deserialize, Serialize};

use crate::changes::{ChangesInput, LogItem};

/// The new part of a session is worth a pass only with this much talk in it.
const MIN_TEXT: usize = 600;
/// Caps for context.md: one request, one answer, the whole new part.
const REQUEST_LIMIT: usize = 2500;
const ANSWER_LIMIT: usize = 5000;
const CONTEXT_LIMIT: usize = 120_000;

// ------------------------------------------------------------------ state

/// Where the last pass stopped: the log index and a hash of the item there,
/// so a resumed session (whose log is rebuilt from the transcript) is found again.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
struct Anchor {
    upto: usize,
    hash: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct PassRecord {
    n: usize,
    at: Option<DateTime<Utc>>,
    #[serde(default)]
    wrote: Vec<String>,
    /// Memory entries the pass added or changed.
    #[serde(default)]
    memory: Vec<String>,
    #[serde(default)]
    note: String,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct State {
    anchor: Option<Anchor>,
    #[serde(default)]
    passes: Vec<PassRecord>,
}

fn dir(key: &str) -> PathBuf {
    stepan_verify_core::Home::locate().state().join("distill").join(key)
}

fn load_state(key: &str) -> State {
    fs::read_to_string(dir(key).join("state.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn save_state(key: &str, state: &State) {
    let _ = fs::create_dir_all(dir(key));
    if let Ok(text) = serde_json::to_string_pretty(state) {
        let _ = fs::write(dir(key).join("state.json"), text);
    }
}

fn item_hash(item: &LogItem) -> u64 {
    let mut h = DefaultHasher::new();
    format!("{item:?}").hash(&mut h);
    h.finish()
}

fn anchor_of(log: &[LogItem]) -> Option<Anchor> {
    let last = log.last()?;
    Some(Anchor { upto: log.len(), hash: item_hash(last) })
}

/// Index of the first log item the next pass reads.
fn start_of(log: &[LogItem], anchor: Option<&Anchor>) -> usize {
    let Some(a) = anchor else { return 0 };
    // Same place as last time (the live session went on).
    if a.upto <= log.len() && a.upto > 0 && item_hash(&log[a.upto - 1]) == a.hash {
        return a.upto;
    }
    // The log was rebuilt (resumed session): find the item by its hash.
    match log.iter().rposition(|i| item_hash(i) == a.hash) {
        Some(i) => i + 1,
        None => a.upto.min(log.len()),
    }
}

/// Enough talk in the new part to have produced a decision or a fact.
fn worth(new: &[LogItem]) -> bool {
    let answers = new.iter().filter(|i| matches!(i, LogItem::Answer(_))).count();
    let text: usize = new
        .iter()
        .map(|i| match i {
            LogItem::Request(t) | LogItem::Answer(t) => t.chars().count(),
            _ => 0,
        })
        .sum();
    answers > 0 && text >= MIN_TEXT
}

fn clip(text: &str, limit: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let head: String = text.chars().take(limit).collect();
    format!("{head}\n…[обрезано]")
}

/// The new part of the session as markdown for the distiller.
fn context_md(input: &ChangesInput, project: Option<&str>, start: usize) -> String {
    let new = &input.log[start..];
    let mut body = String::new();
    let mut edits: Vec<&str> = Vec::new();
    let flush = |body: &mut String, edits: &mut Vec<&str>| {
        if !edits.is_empty() {
            edits.dedup();
            body.push_str(&format!("_Правки: {}_\n\n", edits.join(", ")));
            edits.clear();
        }
    };
    for item in new {
        match item {
            LogItem::Edit { path, .. } => edits.push(path),
            other => {
                flush(&mut body, &mut edits);
                match other {
                    LogItem::Request(t) => body.push_str(&format!("### Пользователь\n\n{}\n\n", clip(t, REQUEST_LIMIT))),
                    LogItem::Answer(t) => body.push_str(&format!("### Агент\n\n{}\n\n", clip(t, ANSWER_LIMIT))),
                    LogItem::Command(c) => body.push_str(&format!("- `$ {}`\n", clip(&c.replace('`', "'"), 200).replace('\n', " "))),
                    LogItem::Edit { .. } => {}
                }
            }
        }
    }
    flush(&mut body, &mut edits);
    // Too long: keep the end, where the outcome is.
    if body.len() > CONTEXT_LIMIT {
        let mut cut = body.len() - CONTEXT_LIMIT;
        while !body.is_char_boundary(cut) {
            cut += 1;
        }
        body = format!("…[начало новой части опущено]\n\n{}", &body[cut..]);
    }
    let earlier = if start > 0 {
        format!("Это продолжение: первые {start} шагов сессии уже разобраны прошлыми проходами. Ниже — только новое.\n\n")
    } else {
        String::new()
    };
    let project = project.map_or_else(|| "без проекта".to_owned(), |p| format!("{p} ({})", input.cwd.display()));
    format!(
        "# Сессия «{}»\n\nПроект: {project}\nСессия: {} · агент: {}\n\n{earlier}## Ход сессии\n\n{body}",
        input.title,
        input.key,
        match input.provider {
            Provider::Claude => "claude",
            Provider::Codex => "codex",
        },
    )
}

fn prompt(run: &Path, docs_dir: &Path, key: &str) -> String {
    format!(
        "Ты — летописец Stepan. Прочитай `{context}`: это новая часть рабочей сессии агента с пользователем. \
Сохрани то, что переживёт эту сессию, по правилам из твоих инструкций: решения и вопросы — на полку документов, поправки пользователя и фактуру проекта — в память.\n\n\
Ищи только это:\n\
1. **Решения.** Выбрали подход, библиотеку, схему, отказались от варианта — и понятно почему. `status: accepted`, если пользователь согласился или это уже сделано; `proposed`, если агент предложил, а ответа нет. Мелкие правки по ходу — не решения.\n\
2. **Память: поправки и запреты пользователя.** Пользователь поправил агента, что-то запретил, сказал, как ему нравится («не делай так», «лучше …», «я же просил …»). Каждое такое — одна запись: `stepan_verify memory add \"повелительно, одной мыслью\" --kind dont|rule --quote \"его слова дословно\"`; `--global`, если это про пользователя вообще, а не про этот проект. Это самое ценное в сессии — не пропусти.\n\
3. **Память: фактура проекта.** Как запускать и тестировать, где что лежит, инварианты, грабли, на которые наступили: `stepan_verify memory add \"…\" --kind fact`. Сначала посмотри, что уже есть (`stepan_verify memory list`), не дублируй; устаревшее поправь `memory edit` или убери `memory rm`.\n\
4. **Открытые вопросы.** Агент спросил пользователя о развилке, а ответа в сессии нет — разбор с полем `question`, вариантами и рекомендацией агента. Если вопрос из списка открытых получил ответ — поставь тому документу `status: answered` и запиши решение.\n\n\
Агент сессии мог уже записать документы сам — загляни в `{docs}/` и не дублируй, лучше дополни. \
Не выдумывай: каждое утверждение должно опираться на context.md. Не пиши отчёт о сессии и пересказ правок. \
Если записать нечего — ничего не пиши: это нормальный и частый исход.\n\n\
Во фронтматтере: `author: авто`, `session: {key}`.\n\n\
В конце обязательно запиши `{result}`: {{\"wrote\": [\"пути созданных и изменённых документов\"], \"memory\": [\"id записей памяти, которые добавил или поправил\"], \"note\": \"одна строка: что записал или почему ничего\"}}.",
        context = run.join("context.md").display(),
        docs = docs_dir.display(),
        result = run.join("result.json").display(),
    )
}

// ------------------------------------------------------------------ jobs

struct Pending {
    input: ChangesInput,
    project: Option<String>,
    _timer: Task<()>,
}

struct Running {
    title: String,
    n: usize,
    upto: Option<Anchor>,
    _handle: AgentHandle,
    _pump: Task<()>,
}

/// Quiet timers and running passes by session key.
#[derive(Default)]
pub struct Distill {
    pending: HashMap<String, Pending>,
    running: HashMap<String, Running>,
}

impl Global for Distill {}

fn ensure(cx: &mut App) {
    if !cx.has_global::<Distill>() {
        cx.set_global(Distill::default());
    }
}

/// Titles of sessions whose results are being written now.
pub fn running(cx: &App) -> Vec<String> {
    cx.try_global::<Distill>().map(|d| d.running.values().map(|r| r.title.clone()).collect()).unwrap_or_default()
}

/// Redraw `view` when passes start and end.
pub fn observe<T: 'static>(cx: &mut Context<T>) {
    ensure(cx);
    cx.observe_global::<Distill>(|_, cx| cx.notify()).detach();
}

/// A new turn: the session is not quiet, drop its timer.
pub fn turn_started(key: &str, cx: &mut App) {
    if cx.has_global::<Distill>() {
        cx.update_global::<Distill, _>(|d, _| {
            d.pending.remove(key);
        });
    }
}

fn quiet_after(cx: &App) -> Duration {
    // Debug hook: STEPAN_VERIFY_DISTILL_AFTER_SECS=5 to see a pass right away.
    if let Some(secs) = std::env::var("STEPAN_VERIFY_DISTILL_AFTER_SECS").ok().and_then(|v| v.parse().ok()) {
        return Duration::from_secs(secs);
    }
    Duration::from_secs(crate::settings::config(cx).docs.distill_after_min.max(1) * 60)
}

/// A turn ended: start (or restart) the quiet timer for this session.
pub fn turn_finished(input: ChangesInput, project: Option<String>, cx: &mut App) {
    if crate::settings::config(cx).docs.distill == DistillMode::Off || input.key.starts_with("unsaved-") {
        return;
    }
    ensure(cx);
    let key = input.key.clone();
    let delay = quiet_after(cx);
    let timer_key = key.clone();
    let timer = cx.spawn(async move |cx| {
        cx.background_executor().timer(delay).await;
        cx.update(|cx| fire(&timer_key, cx));
    });
    cx.update_global::<Distill, _>(|d, _| {
        d.pending.insert(key, Pending { input, project, _timer: timer });
    });
}

fn fire(key: &str, cx: &mut App) {
    if cx.try_global::<Distill>().is_some_and(|d| d.running.contains_key(key)) {
        // The previous pass still runs: keep the input, look again in a minute.
        let retry_key = key.to_owned();
        let retry = cx.spawn(async move |cx| {
            cx.background_executor().timer(Duration::from_secs(60)).await;
            cx.update(|cx| fire(&retry_key, cx));
        });
        cx.update_global::<Distill, _>(|d, _| {
            if let Some(p) = d.pending.get_mut(key) {
                p._timer = retry;
            }
        });
        return;
    }
    let mut pending = None;
    cx.update_global::<Distill, _>(|d, _| pending = d.pending.remove(key));
    if let Some(p) = pending {
        launch(p.input, p.project, cx);
    }
}

fn launch(input: ChangesInput, project: Option<String>, cx: &mut App) {
    if crate::settings::config(cx).docs.distill == DistillMode::Off {
        return;
    }
    let key = input.key.clone();
    let mut state = load_state(&key);
    let start = start_of(&input.log, state.anchor.as_ref());
    let new = input.log.get(start..).unwrap_or(&[]);
    if !worth(new) {
        return;
    }
    let home = stepan_verify_core::Home::locate();
    let docs_dir = match &project {
        Some(p) => home.docs().join(p),
        None => home.docs(),
    };
    let n = state.passes.iter().map(|p| p.n).max().unwrap_or(0) + 1;
    let run = dir(&key).join("runs").join(n.to_string());
    if let Err(err) = fs::create_dir_all(&run).and_then(|_| fs::create_dir_all(&docs_dir)) {
        eprintln!("stepan_verify: distill: {err}");
        return;
    }
    if let Err(err) = fs::write(run.join("context.md"), context_md(&input, project.as_deref(), start)) {
        eprintln!("stepan_verify: distill: context.md: {err}");
        return;
    }
    state.passes.push(PassRecord { n, at: Some(Utc::now()), ..Default::default() });
    save_state(&key, &state);

    let model = crate::settings::report_model(input.provider, cx);
    let spec = SessionSpec {
        provider: input.provider,
        native_id: None,
        transcript: None,
        // Its own folder: the pass reads context.md and writes only documents.
        cwd: docs_dir.clone(),
        mode: Mode::FullAccess,
        image_cache: Some(home.state().join("images")),
        // Memory entries it writes are signed «авто» and belong to the session's project, not to its cwd.
        env: vec![
            ("STEPAN_VERIFY_AGENT".to_owned(), "авто".to_owned()),
            ("STEPAN_VERIFY_PROJECT".to_owned(), project.clone().unwrap_or_default()),
        ],
        model,
        instructions: Some(stepan_verify_core::docs::agent_brief(&home, project.as_deref(), "авто", Some(&key))),
    };
    let (handle, events) = stepan_verify_agents::open(spec);
    handle.send(prompt(&run, &docs_dir, &key));
    let pump_key = key.clone();
    let pump = cx.spawn(async move |cx| {
        while let Ok(event) = events.recv().await {
            let error = match event {
                AgentEvent::TurnCompleted { error } => Some(error),
                AgentEvent::Exited => Some(Some("Агент завершился".into())),
                AgentEvent::Error(err) => Some(Some(err)),
                _ => None,
            };
            if let Some(error) = error {
                cx.update(|cx| finish(&pump_key, error, cx));
                break;
            }
        }
    });
    let running = Running { title: input.title.clone(), n, upto: anchor_of(&input.log), _handle: handle, _pump: pump };
    cx.update_global::<Distill, _>(|d, _| {
        d.running.insert(key, running);
    });
}

#[derive(Deserialize, Default)]
struct ResultFile {
    #[serde(default)]
    wrote: Vec<String>,
    #[serde(default)]
    memory: Vec<String>,
    #[serde(default)]
    note: String,
}

fn finish(key: &str, error: Option<String>, cx: &mut App) {
    let mut done = None;
    cx.update_global::<Distill, _>(|d, _| done = d.running.remove(key));
    let Some(done) = done else { return };
    let run = dir(key).join("runs").join(done.n.to_string());
    let result: Option<ResultFile> = fs::read_to_string(run.join("result.json")).ok().and_then(|t| serde_json::from_str(&t).ok());
    let mut state = load_state(key);
    let ok = error.is_none() && result.is_some();
    if let Some(pass) = state.passes.iter_mut().find(|p| p.n == done.n) {
        match (&result, error) {
            (Some(r), None) => {
                pass.wrote = r.wrote.clone();
                pass.memory = r.memory.clone();
                pass.note = r.note.clone();
            }
            (_, Some(e)) => pass.error = Some(e),
            (None, None) => pass.error = Some("Агент закончил, но не записал result.json".into()),
        }
    }
    // Only a finished pass moves the anchor; a failed one is retried next time.
    if ok {
        state.anchor = done.upto;
    }
    save_state(key, &state);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(log: Vec<LogItem>) -> ChangesInput {
        ChangesInput {
            key: "abc".into(),
            provider: Provider::Claude,
            title: "Поиск".into(),
            cwd: PathBuf::from("/p/outpoll-web"),
            requests: Vec::new(),
            final_answer: String::new(),
            patches: Vec::new(),
            commands: Vec::new(),
            screenshots: Vec::new(),
            log,
        }
    }

    fn talk(n: usize) -> Vec<LogItem> {
        (0..n)
            .flat_map(|i| [LogItem::Request(format!("вопрос {i} {}", "x".repeat(200))), LogItem::Answer(format!("ответ {i} {}", "y".repeat(200)))])
            .collect()
    }

    #[core::prelude::v1::test]
    fn anchor_follows_live_and_rebuilt_logs() {
        let log = talk(3);
        let a = anchor_of(&log[..4]).unwrap();
        assert_eq!(start_of(&log, Some(&a)), 4);
        // Rebuilt from the transcript with an extra item in front.
        let mut rebuilt = vec![LogItem::Command("ls".into())];
        rebuilt.extend(log.clone());
        assert_eq!(start_of(&rebuilt, Some(&a)), 5);
        assert_eq!(start_of(&log, None), 0);
        // Unknown anchor past the end: nothing new.
        let lost = Anchor { upto: 99, hash: 1 };
        assert_eq!(start_of(&log, Some(&lost)), log.len());
    }

    #[core::prelude::v1::test]
    fn small_talk_is_not_worth_a_pass() {
        assert!(!worth(&[LogItem::Request("ок".into()), LogItem::Answer("готово".into())]));
        assert!(!worth(&[LogItem::Request("x".repeat(2000))]), "no answer, nothing decided");
        assert!(worth(&talk(2)));
    }

    #[core::prelude::v1::test]
    fn context_lists_new_part_only() {
        let mut log = talk(1);
        log.push(LogItem::Edit { path: "src/a.rs".into(), patch: String::new() });
        log.push(LogItem::Edit { path: "src/a.rs".into(), patch: String::new() });
        log.push(LogItem::Command("cargo `test`".into()));
        log.push(LogItem::Answer("итог".into()));
        let md = context_md(&input(log), Some("outpoll-web"), 1);
        assert!(!md.contains("вопрос 0"), "already read");
        assert!(md.contains("ответ 0"));
        assert!(md.contains("_Правки: src/a.rs_"));
        assert!(md.contains("- `$ cargo 'test'`"));
        assert!(md.contains("первые 1 шагов"));
        assert!(md.contains("Проект: outpoll-web (/p/outpoll-web)"));
    }
}
