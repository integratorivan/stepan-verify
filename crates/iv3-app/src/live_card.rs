//! Dashboard card of a live session: what the agent did, what it does now,
//! what comes next, and where it stopped for you. Built from transcript items
//! so it works for sessions open in iv3 and for sessions peeked from disk.

use gpui_kit::component::{
    Icon, IconName, Sizable as _, Theme,
    spinner::Spinner,
    h_flex, v_flex,
};

use crate::ui::{Palette, mix};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use iv3_agents::{Item, ItemKind, ItemStatus};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepState {
    Done,
    Failed,
    Current,
    Pending,
    /// The agent asked you something and stopped.
    NeedsAnswer,
    /// A command or edit waits for your approval.
    NeedsApproval,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub label: String,
    pub detail: String,
    pub state: StepState,
    pub secs: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Card {
    pub steps: Vec<Step>,
    /// Steps not shown (older done ones and far pending ones).
    pub more: usize,
    /// "Шаг 3 из 9" when the agent keeps a plan, else "5 действий".
    pub progress: String,
    /// Done / total of the plan, for the bar on the current step.
    pub fraction: Option<f32>,
    /// Older done steps cut from `steps`.
    pub done_hidden: usize,
    /// (done, total) when the agent keeps a plan: the segmented progress bar.
    pub plan: Option<(usize, usize)>,
    /// Time spent on all done steps, when known.
    pub done_secs: Option<u64>,
}

impl Card {
    /// Leading done steps of `steps` (they fold into one "N шагов готово" row).
    pub fn leading_done(&self) -> usize {
        self.steps.iter().take_while(|s| s.state == StepState::Done).count()
    }

    /// All finished steps, shown or not.
    pub fn done_total(&self) -> usize {
        self.done_hidden + self.leading_done()
    }
}

const SHOW_DONE: usize = 2;
const SHOW_PENDING: usize = 2;

/// `running`: a turn is in progress. `approvals`: titles of pending approvals.
pub fn build(items: &[Item], running: bool, approvals: &[String], timing: &dyn Fn(&str) -> Option<u64>) -> Card {
    // The current turn: everything after your last message.
    let turn_start = items.iter().rposition(|i| i.kind == ItemKind::User).map_or(0, |i| i + 1);
    let turn = &items[turn_start..];
    let plan = turn.iter().rev().find(|i| i.kind == ItemKind::Plan).or_else(|| {
        // A plan from an earlier turn still describes the work while running.
        running.then(|| items.iter().rev().find(|i| i.kind == ItemKind::Plan)).flatten()
    });

    let mut card = match plan {
        Some(plan) => from_plan(&plan.body, running),
        None => from_tools(turn, running, timing),
    };

    for a in approvals {
        let (label, detail) = split_title(a);
        card.steps.push(Step { label: format!("Ждёт подтверждения: {}", label.to_lowercase()), detail, state: StepState::NeedsApproval, secs: None });
    }
    if !running && approvals.is_empty() {
        if let Some(last) = turn.iter().rev().find(|i| i.kind == ItemKind::Assistant) {
            let tail = last.body.trim_end();
            if tail.ends_with('?') {
                let question = tail.lines().last().unwrap_or_default().trim().to_owned();
                card.steps.push(Step { label: "Ждёт твоего ответа".into(), detail: short(&question, 90), state: StepState::NeedsAnswer, secs: None });
            }
        }
    }
    card
}

fn from_plan(body: &str, running: bool) -> Card {
    let mut steps: Vec<Step> = body
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let (mark, text) = l.split_at(l.char_indices().nth(1).map_or(l.len(), |(i, _)| i));
            let state = match mark {
                "✓" => StepState::Done,
                "▸" => StepState::Current,
                _ => StepState::Pending,
            };
            Step { label: text.trim().to_owned(), detail: String::new(), state, secs: None }
        })
        .collect();
    let total = steps.len();
    let done = steps.iter().filter(|s| s.state == StepState::Done).count();
    // Plans that only mark done/pending: the first pending step is the current one.
    if running && !steps.iter().any(|s| s.state == StepState::Current) {
        if let Some(s) = steps.iter_mut().find(|s| s.state == StepState::Pending) {
            s.state = StepState::Current;
        }
    }
    let current = steps.iter().position(|s| s.state == StepState::Current);
    let anchor = current.unwrap_or(done.min(total.saturating_sub(1)));
    let from = anchor.saturating_sub(SHOW_DONE);
    let to = (anchor + 1 + SHOW_PENDING).min(total);
    let more = total - (to - from);
    let progress = match current {
        Some(i) => format!("Шаг {} из {total}", i + 1),
        None => format!("Готово {done} из {total}"),
    };
    Card {
        steps: steps[from..to].to_vec(),
        more,
        progress,
        fraction: (total > 0).then(|| done as f32 / total as f32),
        done_hidden: from,
        plan: (total > 0).then_some((done, total)),
        done_secs: None,
    }
}

fn from_tools(turn: &[Item], running: bool, timing: &dyn Fn(&str) -> Option<u64>) -> Card {
    let tools: Vec<&Item> = turn.iter().filter(|i| i.kind == ItemKind::Tool).collect();
    let mut steps: Vec<Step> = tools
        .iter()
        .map(|i| {
            let (label, detail) = split_title(&i.title);
            let state = match i.status {
                ItemStatus::Running => StepState::Current,
                ItemStatus::Failed => StepState::Failed,
                ItemStatus::Done => StepState::Done,
            };
            Step { label, detail, state, secs: timing(&i.id) }
        })
        .collect();
    let total = steps.len();
    if running && !steps.iter().any(|s| s.state == StepState::Current) {
        let writing = turn.last().is_some_and(|i| i.kind == ItemKind::Assistant && i.status == ItemStatus::Running);
        steps.push(Step {
            label: if writing { "Пишет ответ".into() } else { "Думает".into() },
            detail: String::new(),
            state: StepState::Current,
            secs: None,
        });
    }
    let done_secs: u64 = steps.iter().filter(|s| s.state == StepState::Done).filter_map(|s| s.secs).sum();
    let keep = SHOW_DONE + 2;
    let more = steps.len().saturating_sub(keep);
    let steps = steps.split_off(more);
    Card {
        steps,
        more,
        progress: format!("{total} {}", actions_word(total)),
        fraction: None,
        done_hidden: more,
        plan: None,
        done_secs: (done_secs > 0).then_some(done_secs),
    }
}

/// Tool title → (what the agent does, the object), e.g. `$ npm test` → ("Команда", "npm test").
pub fn split_title(title: &str) -> (String, String) {
    let base = |p: &str| p.rsplit('/').next().unwrap_or(p).to_owned();
    let pairs: [(&str, &str); 9] = [
        ("$ ", "Команда"),
        ("Read ", "Читает"),
        ("Edit ", "Правит"),
        ("MultiEdit ", "Правит"),
        ("Write ", "Создаёт"),
        ("Grep ", "Ищет"),
        ("Glob ", "Ищет файлы"),
        ("WebFetch ", "Читает страницу"),
        ("WebSearch ", "Ищет в сети"),
    ];
    for (prefix, label) in pairs {
        if let Some(rest) = title.strip_prefix(prefix) {
            let detail = match label {
                "Читает" | "Правит" | "Создаёт" => base(rest),
                "Команда" => command_core(rest),
                _ => rest.to_owned(),
            };
            return (label.to_owned(), short(&detail, 80));
        }
    }
    if let Some(rest) = title.strip_prefix("Сабагент: ") {
        return ("Сабагент".into(), short(rest, 80));
    }
    if let Some(rest) = title.strip_prefix("Правки: ") {
        return ("Правит".into(), short(rest, 80));
    }
    if let Some((tool, hint)) = title.split_once(" — ") {
        return (tool.to_owned(), short(hint, 80));
    }
    (short(title, 80), String::new())
}

/// The part of a shell line that says what it does: drops `VAR=x;`, `cd dir &&`
/// and env assignments in front, e.g. `S=/tmp; cd /p && cargo build` → `cargo build`.
fn command_core(cmd: &str) -> String {
    let mut rest = cmd.lines().next().unwrap_or_default().trim();
    loop {
        let before = rest;
        if rest.starts_with("cd ") {
            if let Some(i) = rest.find("&&") {
                rest = rest[i + 2..].trim_start();
            }
        }
        let first = rest.split_whitespace().next().unwrap_or_default();
        if first.contains('=') && !first.starts_with('-') && !first.starts_with('"') {
            let first = first.trim_end_matches(';');
            rest = rest[first.len()..].trim_start().trim_start_matches(';').trim_start();
        }
        if rest == before || rest.is_empty() {
            break;
        }
    }
    if rest.is_empty() { cmd.to_owned() } else { rest.to_owned() }
}

fn short(s: &str, max: usize) -> String {
    let s = s.lines().next().unwrap_or_default().trim();
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_owned(),
    }
}

fn actions_word(n: usize) -> &'static str {
    match (n % 10, n % 100) {
        (1, r) if r != 11 => "действие",
        (2..=4, r) if !(12..=14).contains(&r) => "действия",
        _ => "действий",
    }
}

pub fn mmss(secs: u64) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

// ---------------------------------------------------------------- rendering

fn step_icon(state: StepState, p: &Palette) -> AnyElement {
    match state {
        StepState::Done => Icon::new(IconName::CircleCheck).small().text_color(p.muted).into_any_element(),
        StepState::Failed => Icon::new(IconName::CircleX).small().text_color(p.red.solid).into_any_element(),
        StepState::Current => Spinner::new().small().color(p.blue.solid).into_any_element(),
        StepState::Pending => div()
            .size(px(14.))
            .m(px(1.))
            .flex_shrink_0()
            .rounded_full()
            .border_1()
            .border_dashed()
            .border_color(p.faint)
            .into_any_element(),
        StepState::NeedsAnswer => Icon::empty().path("icons/circle-question-mark.svg").small().text_color(p.amber.text).into_any_element(),
        StepState::NeedsApproval => Icon::empty().path("icons/shield-alert.svg").small().text_color(p.red.solid).into_any_element(),
    }
}

/// The steps inside a session card (reui "Approval Run Console" layout).
/// `gate` / `ask`: buttons for a pending approval and for a question.
/// `fold`: the "N шагов готово" row and whether it hides the leading done steps.
pub fn render_steps(
    key: &str,
    card: &Card,
    p: &Palette,
    theme: &Theme,
    fold: Option<(AnyElement, bool)>,
    mut gate: Option<AnyElement>,
    mut ask: Option<AnyElement>,
) -> impl IntoElement {
    let mono = theme.mono_font_family.clone();
    let collapse = fold.as_ref().is_some_and(|(_, c)| *c);
    let skip = if collapse { card.leading_done() } else { 0 };
    let more = if collapse { card.more.saturating_sub(card.done_hidden) } else { card.more };
    v_flex()
        .children(fold.map(|(row, _)| row))
        .when(card.steps.is_empty(), |this| {
            this.child(div().px_3().py(px(10.)).text_size(px(13.)).text_color(p.faint).child("Пока без действий"))
        })
        .children(card.steps.iter().enumerate().skip(skip).map(|(i, s)| {
            let id = SharedString::from(format!("step:{key}:{i}"));
            match s.state {
                StepState::NeedsAnswer | StepState::NeedsApproval => {
                    let approval = s.state == StepState::NeedsApproval;
                    let (tone, actions) = if approval { (p.red, gate.take()) } else { (p.amber, ask.take()) };
                    h_flex()
                        .id(id)
                        .items_start()
                        .gap(px(9.))
                        .px_3()
                        .py(px(9.))
                        .border_t_1()
                        .border_color(p.border)
                        .bg(mix(tone.soft, p.surface, if approval { 0.5 } else { 0.55 }))
                        .child(div().mt(px(1.)).child(step_icon(s.state, p)))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap_2()
                                .child(
                                    div()
                                        .text_size(px(13.))
                                        .text_color(p.fg)
                                        .child(div().font_weight(FontWeight::SEMIBOLD).text_color(p.ink).child(s.label.clone()))
                                        .when(!approval && !s.detail.is_empty(), |d| d.child(s.detail.clone())),
                                )
                                .when(approval && !s.detail.is_empty(), |this| {
                                    this.child(
                                        h_flex()
                                            .px(px(9.))
                                            .py(px(6.))
                                            .rounded(px(6.))
                                            .border_1()
                                            .border_color(p.border)
                                            .bg(theme.background)
                                            .font_family(mono.clone())
                                            .text_size(px(12.))
                                            .child(div().text_color(p.faint).child("$ "))
                                            .child(div().min_w_0().truncate().text_color(p.ink).child(s.detail.clone())),
                                    )
                                })
                                .when_some(actions, |this, a| this.child(a)),
                        )
                        .into_any_element()
                }
                state => {
                    let current = state == StepState::Current;
                    let label_color = match state {
                        StepState::Done => p.muted,
                        StepState::Pending => p.faint,
                        StepState::Current => p.ink,
                        _ => p.fg,
                    };
                    v_flex()
                        .id(id)
                        .relative()
                        .px_3()
                        .py(px(6.))
                        .min_h(px(30.))
                        .when(current, |d| d.min_h(px(36.)))
                        .justify_center()
                        .gap(px(4.))
                        .when(current, |this| {
                            this.bg(p.surface2)
                                .border_y_1()
                                .border_color(p.border)
                                .child(div().absolute().left_0().top_0().bottom_0().w(px(2.)).bg(p.blue.solid))
                        })
                        .child(
                            h_flex()
                                .gap(px(9.))
                                .items_center()
                                .child(step_icon(state, p))
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .max_w(px(260.))
                                        .truncate()
                                        .text_size(px(13.))
                                        .text_color(label_color)
                                        .when(current, |d| d.font_weight(FontWeight::SEMIBOLD))
                                        .child(s.label.clone()),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .font_family(mono.clone())
                                        .text_size(px(11.5))
                                        .text_color(p.muted)
                                        .child(s.detail.clone()),
                                )
                                .when_some(s.secs, |this, secs| {
                                    this.child(
                                        div()
                                            .flex_shrink_0()
                                            .font_family(mono.clone())
                                            .text_size(px(11.5))
                                            .text_color(if current { p.blue.text } else { p.faint })
                                            .child(mmss(secs)),
                                    )
                                }),
                        )
                        .when(current && card.fraction.is_some(), |this| {
                            let f = card.fraction.unwrap_or(0.0);
                            this.child(
                                div()
                                    .ml(px(25.))
                                    .h(px(3.))
                                    .rounded_full()
                                    .bg(p.border)
                                    .child(div().h_full().w(relative(f.max(0.03))).rounded_full().bg(p.blue.solid)),
                            )
                        })
                        .into_any_element()
                }
            }
        }))
        .when(more > 0, |this| {
            this.child(
                div()
                    .pl(px(37.))
                    .pr_3()
                    .pt(px(7.))
                    .pb(px(8.))
                    .border_t_1()
                    .border_dashed()
                    .border_color(p.border)
                    .font_family(mono.clone())
                    .text_size(px(11.5))
                    .text_color(p.faint)
                    .child(format!("ещё {} {}", more, steps_word(more))),
            )
        })
}

pub fn steps_word(n: usize) -> &'static str {
    match (n % 10, n % 100) {
        (1, r) if r != 11 => "шаг",
        (2..=4, r) if !(12..=14).contains(&r) => "шага",
        _ => "шагов",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(id: &str, title: &str, status: ItemStatus) -> Item {
        Item::tool(id, title, "", status)
    }

    #[core::prelude::v1::test]
    fn plan_shows_window_around_current_step() {
        let plan = Item::message("p", ItemKind::Plan, "✓ a\n✓ b\n✓ c\n▸ d\n○ e\n○ f\n○ g\n○ h\n○ i");
        let card = build(&[Item::message("u", ItemKind::User, "go"), plan], true, &[], &|_| None);
        assert_eq!(card.progress, "Шаг 4 из 9");
        let labels: Vec<&str> = card.steps.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(labels, ["b", "c", "d", "e", "f"]);
        assert_eq!(card.more, 4);
        assert_eq!(card.steps[2].state, StepState::Current);
    }

    #[core::prelude::v1::test]
    fn tools_and_stops() {
        let items = vec![
            Item::message("u", ItemKind::User, "почини"),
            tool("1", "Read /p/src/a.ts", ItemStatus::Done),
            tool("2", "$ npm test", ItemStatus::Failed),
            Item::message("x", ItemKind::Assistant, "Тест падает. Обновить снапшоты?"),
        ];
        let card = build(&items, false, &[], &|id| (id == "1").then_some(8));
        assert_eq!(card.steps[0].label, "Читает");
        assert_eq!(card.steps[0].detail, "a.ts");
        assert_eq!(card.steps[0].secs, Some(8));
        assert_eq!(card.steps[1].state, StepState::Failed);
        assert_eq!(card.steps.last().unwrap().state, StepState::NeedsAnswer);

        let card = build(&items[..3], true, &["$ rm -rf build".into()], &|_| None);
        assert!(card.steps.iter().any(|s| s.state == StepState::Current && s.label == "Думает"));
        assert!(card.steps.iter().any(|s| s.state == StepState::NeedsApproval && s.detail == "rm -rf build"));
    }

    #[core::prelude::v1::test]
    fn titles_split() {
        assert_eq!(split_title("Browser · navigate — http://x"), ("Browser · navigate".into(), "http://x".into()));
        assert_eq!(split_title("Сабагент: ревью"), ("Сабагент".into(), "ревью".into()));
        assert_eq!(split_title("$ S=/tmp/x; cd /p && cargo build -p a").1, "cargo build -p a");
        assert_eq!(split_title("$ FOO=1 npm test").1, "npm test");
        assert_eq!(split_title("$ ls -la").1, "ls -la");
    }
}
