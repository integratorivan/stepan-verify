//! The task board: columns by status, cards with the agent's live state,
//! a detail drawer with the journal, and the autopilot switch.

use std::collections::BTreeMap;

use chrono::Utc;
use gpui_kit::component::{
    ActiveTheme, Icon, IconName, Sizable as _, Theme,
    spinner::Spinner,
    switch::Switch,
    text::TextView,
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use crate::ui::PressButton;
use iv3_core::{Provider, Task, TaskStatus};

use crate::{
    app::AppView,
    attach,
    format::ago,
    session::Status,
    ui::{self, Palette, Tone},
};

struct Column {
    key: &'static str,
    title: &'static str,
    hint: &'static str,
    statuses: &'static [TaskStatus],
}

const COLUMNS: [Column; 5] = [
    Column { key: "inbox", title: "Входящие", hint: "Разбери: в работу или удали", statuses: &[TaskStatus::Inbox] },
    Column { key: "todo", title: "К работе", hint: "Агенты берут готовые", statuses: &[TaskStatus::Todo] },
    Column { key: "doing", title: "В работе", hint: "Агент работает", statuses: &[TaskStatus::InProgress] },
    Column {
        key: "waiting",
        title: "Ждёт тебя",
        hint: "Ревью и вопросы агентов",
        statuses: &[TaskStatus::Blocked, TaskStatus::Review],
    },
    Column { key: "done", title: "Готово", hint: "Принятые задачи", statuses: &[TaskStatus::Done] },
];

/// Lease length the ring is measured against.
const LEASE_MINUTES: f32 = 15.;

fn column_color(key: &str, p: &Palette) -> Hsla {
    match key {
        "todo" => p.blue.solid,
        "doing" => p.accent.solid,
        "waiting" => p.amber.solid,
        "done" => p.green.solid,
        _ => p.faint,
    }
}

/// Which agent a task belongs to: the live session, else the claim name.
fn task_provider(task: &Task, live: Option<Provider>) -> Option<Provider> {
    live.or_else(|| {
        let who = task.meta.claimed_by.as_deref()?.to_lowercase();
        if who.contains("codex") {
            Some(Provider::Codex)
        } else if who.contains("claude") {
            Some(Provider::Claude)
        } else {
            None
        }
    })
}

pub fn render(app: &AppView, window: &mut Window, cx: &mut Context<AppView>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let p = Palette::of(&theme);
    let selected = app.selected_task.as_ref().and_then(|id| app.tasks.iter().find(|t| &t.id == id)).cloned();
    // Project filter lives with the board element, not in the app model.
    let filter = window.use_keyed_state("board-project-filter", cx, |_, _| None::<String>);
    let current = filter.read(cx).clone();

    let mut projects: BTreeMap<String, usize> = BTreeMap::new();
    for t in &app.tasks {
        if let Some(name) = t.project_name() {
            *projects.entry(name).or_default() += 1;
        }
    }
    let mut projects: Vec<(String, usize)> = projects.into_iter().collect();
    projects.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

    let visible: Vec<&Task> = app
        .tasks
        .iter()
        .filter(|t| current.as_ref().is_none_or(|c| t.project_name().as_ref() == Some(c)))
        .collect();
    let waiting = visible.iter().filter(|t| matches!(t.meta.status, TaskStatus::Review | TaskStatus::Blocked)).count();
    let doing = visible.iter().filter(|t| t.meta.status == TaskStatus::InProgress).count();
    let limit = crate::settings::config(cx).agents.autopilot_limit;

    let filter_chip = |key: String, label: String, count: usize, active: bool, value: Option<String>, cx: &mut Context<AppView>| {
        let filter = filter.clone();
        h_flex()
            .id(SharedString::from(format!("board-filter:{key}")))
            .gap(px(6.))
            .px(px(10.))
            .py(px(3.))
            .rounded_full()
            .border_1()
            .cursor_pointer()
            .text_size(px(12.5))
            .font_weight(FontWeight::MEDIUM)
            .map(|d| {
                if active {
                    d.bg(p.ink).border_color(p.ink).text_color(p.surface)
                } else {
                    d.bg(p.surface).border_color(p.border).text_color(p.fg).hover(|s| s.border_color(p.border_strong))
                }
            })
            .on_click(cx.listener(move |_, _, _, cx| {
                let value = value.clone();
                filter.update(cx, |f, cx| {
                    *f = value;
                    cx.notify();
                });
            }))
            .child(label)
            .child(
                div()
                    .font_family(theme.mono_font_family.clone())
                    .text_size(px(10.5))
                    .text_color(if active { p.faint } else { p.muted })
                    .child(count.to_string()),
            )
    };

    let mut chips = vec![filter_chip("all".into(), "Все проекты".into(), app.tasks.len(), current.is_none(), None, cx).into_any_element()];
    for (name, n) in &projects {
        let active = current.as_deref() == Some(name.as_str());
        chips.push(filter_chip(name.clone(), name.clone(), *n, active, Some(name.clone()), cx).into_any_element());
    }

    div()
        .size_full()
        .relative()
        .child(
            v_flex()
                .size_full()
                .child(
                    h_flex()
                        .px(px(32.))
                        .pt(px(26.))
                        .pb(px(12.))
                        .gap_3()
                        .items_start()
                        .child(
                            v_flex()
                                .flex_1()
                                .gap_1()
                                .child(ui::page_title("Задачи", &p))
                                .child(
                                    h_flex()
                                        .gap(px(10.))
                                        .text_size(px(13.))
                                        .text_color(p.muted)
                                        .child(format!("{} {}", visible.len(), tasks_word(visible.len())))
                                        .child(
                                            h_flex()
                                                .gap(px(5.))
                                                .child(ui::dot(p.amber.solid))
                                                .child("ждут тебя")
                                                .child(div().font_weight(FontWeight::BOLD).text_color(p.ink).child(waiting.to_string())),
                                        )
                                        .child(
                                            h_flex()
                                                .gap(px(5.))
                                                .child("в работе")
                                                .child(div().font_weight(FontWeight::BOLD).text_color(p.ink).child(format!("{doing} из {limit}"))),
                                        ),
                                ),
                        )
                        .child(
                            h_flex()
                                .gap(px(10.))
                                .px(px(12.))
                                .py(px(7.))
                                .rounded(px(10.))
                                .border_1()
                                .border_color(if app.autopilot { ui::mix(p.green.solid, p.border, 0.5) } else { p.border })
                                .bg(p.surface)
                                .shadow(ui::card_shadow())
                                .child(
                                    Switch::new("autopilot").checked(app.autopilot).on_click(cx.listener(|this, on: &bool, _, cx| {
                                        this.autopilot = *on;
                                        cx.notify();
                                    })),
                                )
                                .child(
                                    v_flex()
                                        .child(div().text_size(px(13.)).font_weight(FontWeight::BOLD).text_color(p.ink).child("Автопилот"))
                                        .child(
                                            div()
                                                .font_family(theme.mono_font_family.clone())
                                                .text_size(px(10.5))
                                                .text_color(p.muted)
                                                .child(format!("сам берёт готовые · до {limit}")),
                                        ),
                                ),
                        )
                        .child(
                            PressButton::new("new-task")
                                .primary()
                                .small()
                                .icon(IconName::Plus)
                                .label("Новая задача")
                                .on_click(cx.listener(|this, _, window, cx| this.open_new_task_dialog(window, cx))),
                        ),
                )
                .child(h_flex().px(px(32.)).pb(px(14.)).gap(px(6.)).flex_wrap().children(chips))
                .child(
                    h_flex()
                        .flex_1()
                        .min_h_0()
                        .px(px(32.))
                        .pb(px(24.))
                        .gap(px(10.))
                        .items_start()
                        .children(COLUMNS.iter().map(|col| render_column(app, col, &visible, &p, &theme, cx))),
                ),
        )
        .when_some(selected, |this, task| {
            this.child(
                div()
                    .id("task-detail-backdrop")
                    .absolute()
                    .inset_0()
                    .bg(p.ink.opacity(0.08))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.selected_task = None;
                        cx.notify();
                    })),
            )
            .child(render_detail(app, &task, &p, &theme, cx))
        })
}

fn tasks_word(n: usize) -> &'static str {
    match (n % 10, n % 100) {
        (1, r) if r != 11 => "задача",
        (2..=4, r) if !(12..=14).contains(&r) => "задачи",
        _ => "задач",
    }
}

fn render_column(app: &AppView, col: &Column, visible: &[&Task], p: &Palette, theme: &Theme, cx: &mut Context<AppView>) -> impl IntoElement {
    let mut tasks: Vec<&Task> = visible.iter().copied().filter(|t| col.statuses.contains(&t.meta.status)).collect();
    // Newest activity first; the done column keeps only recent history.
    tasks.sort_by(|a, b| b.meta.updated_at.cmp(&a.meta.updated_at));
    let total = tasks.len();
    if col.key == "done" {
        tasks.truncate(12);
    }
    let color = column_color(col.key, p);
    v_flex()
        .id(SharedString::from(format!("col:{}", col.key)))
        .flex_1()
        .min_w(px(190.))
        .h_full()
        .rounded(px(12.))
        .border_1()
        .border_color(p.border)
        .bg(p.sunken)
        .child(
            v_flex()
                .px(px(12.))
                .pt(px(10.))
                .pb(px(8.))
                .gap(px(2.))
                .child(
                    h_flex()
                        .gap(px(7.))
                        .child(div().size(px(9.)).rounded(px(2.)).bg(color))
                        .child(div().text_size(px(13.5)).font_weight(FontWeight::BOLD).text_color(p.ink).child(col.title))
                        .child(ui::mono(total.to_string(), theme).text_color(p.muted)),
                )
                // Always one hint line, so every column's list starts at the same height.
                .child(div().h(px(16.)).text_size(px(11.5)).text_color(p.muted).child(col.hint)),
        )
        .child(
            v_flex()
                .id(SharedString::from(format!("col-scroll:{}", col.key)))
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .gap(px(7.))
                .px(px(6.))
                .pt(px(2.))
                .pb(px(8.))
                .when(tasks.is_empty(), |this| {
                    this.child(
                        div()
                            .m(px(2.))
                            .py(px(14.))
                            .flex()
                            .justify_center()
                            .rounded(px(8.))
                            .border_1()
                            .border_dashed()
                            .border_color(p.border_strong)
                            .text_size(px(12.))
                            .text_color(p.faint)
                            .child("Пусто"),
                    )
                })
                .children(tasks.into_iter().map(|t| render_card(app, t, col.key == "done", p, theme, cx))),
        )
}

fn render_card(app: &AppView, task: &Task, done: bool, p: &Palette, theme: &Theme, cx: &mut Context<AppView>) -> impl IntoElement {
    let id = task.id.clone();
    let selected = app.selected_task.as_deref() == Some(task.id.as_str());
    let open_deps = task.open_dependencies(&app.tasks);
    let live = app.task_session(&task.id, cx).map(|(_, v)| {
        let v = v.read(cx);
        (v.provider, v.status, v.activity())
    });
    let provider = task_provider(task, live.as_ref().map(|l| l.0));
    let body = task.body.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with('#')).map(str::to_owned);

    let mut card = v_flex()
        .id(SharedString::from(format!("task:{}", task.id)))
        .px(px(11.))
        .py(px(10.))
        .gap(px(7.))
        .rounded(px(8.))
        .border_1()
        .bg(p.surface)
        .cursor_pointer()
        .map(|d| {
            if selected {
                d.border_color(p.ink).shadow(vec![BoxShadow {
                    color: p.ink,
                    offset: point(px(0.), px(0.)),
                    blur_radius: px(0.),
                    spread_radius: px(1.),
                    inset: false,
                }])
            } else {
                d.border_color(p.border).shadow(ui::card_shadow()).hover(|s| s.border_color(p.border_strong))
            }
        })
        .when(done, |d| d.opacity(0.78))
        .on_click(cx.listener({
            let id = id.clone();
            move |this, _, _, cx| {
                this.selected_task = if this.selected_task.as_deref() == Some(id.as_str()) { None } else { Some(id.clone()) };
                cx.notify();
            }
        }))
        .child(
            div()
                .text_size(px(13.))
                .font_weight(FontWeight::SEMIBOLD)
                .line_height(px(17.))
                .text_color(p.ink)
                .when(done, |d| d.line_through().text_decoration_color(p.faint))
                .child(task.meta.title.clone()),
        )
        .when(!done, |this| {
            this.when_some(body, |this, b| this.child(div().text_size(px(12.)).line_height(px(17.)).text_color(p.muted).line_clamp(2).child(b)))
        });

    // Project, agent, labels; attachments and age on the right.
    let project = task.project_name();
    let files = task.meta.attachments.len();
    card = card.child(
        h_flex()
            .gap(px(5.))
            .items_start()
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(5.))
                    .flex_wrap()
                    .when_some(project, |this, name| this.child(ui::chip(name, p, theme)))
                    .when_some(provider, |this, pr| this.child(ui::provider_chip(pr, p, theme)))
                    .children(task.meta.labels.iter().map(|l| ui::chip(l.clone(), p, theme))),
            )
            .child(
                h_flex()
                    .flex_shrink_0()
                    .pt(px(1.))
                    .gap(px(6.))
                    .font_family(theme.mono_font_family.clone())
                    .text_size(px(11.))
                    .text_color(p.faint)
                    .when(files > 0, |this| this.child(h_flex().gap(px(2.)).child(Icon::new(gpui_kit::assets::IconName::Paperclip).xsmall()).child(files.to_string())))
                    .when_some(task.meta.updated_at, |this, t| this.child(ago(t))),
            ),
    );

    // State.
    card = match task.meta.status {
        TaskStatus::Todo if !open_deps.is_empty() => {
            let first = open_deps
                .first()
                .map(|d| app.tasks.iter().find(|t| t.id == *d).map_or_else(|| d.to_string(), |t| t.meta.title.clone()))
                .unwrap_or_default();
            card.child(
                h_flex()
                    .gap(px(5.))
                    .text_size(px(11.5))
                    .text_color(p.muted)
                    .child(Icon::new(gpui_kit::assets::IconName::Link).xsmall())
                    .child(format!("ждёт {} {}", open_deps.len(), if open_deps.len() == 1 { "задачу" } else { "задачи" }))
                    .child(div().min_w_0().truncate().font_weight(FontWeight::SEMIBOLD).text_color(p.fg).child(first)),
            )
        }
        TaskStatus::InProgress => {
            let who = task.meta.claimed_by.clone().unwrap_or_else(|| "агент".into());
            let lease = task.meta.lease_until.map(|t| (t - Utc::now()).num_minutes().max(0));
            let running = live.as_ref().is_some_and(|l| l.1 == Status::Running);
            let approvals = live.as_ref().is_some_and(|l| l.2.approvals > 0);
            card.child(lease_line(&who, lease, running, p, theme))
                .when_some(live.as_ref().filter(|l| !l.2.line.is_empty() && l.2.approvals == 0), |this, l| {
                    this.child(ui::mono(l.2.line.clone(), theme).text_color(p.muted).truncate())
                })
                .when(approvals, |this| this.child(h_flex().child(ui::badge("Нужно подтверждение", p.red))))
        }
        TaskStatus::Blocked => card
            .child(h_flex().child(ui::badge("Агенту нужен ответ", p.amber)))
            .when_some(task.meta.summary.clone(), |this, s| this.child(div().text_size(px(12.)).text_color(p.muted).line_clamp(3).child(s))),
        TaskStatus::Review => card
            .child(h_flex().child(ui::badge("Сдал на ревью", p.green)))
            .when_some(task.meta.summary.clone(), |this, s| this.child(div().text_size(px(12.)).text_color(p.muted).line_clamp(3).child(s))),
        _ => card,
    };

    // Actions.
    let actions = match task.meta.status {
        TaskStatus::Inbox => vec![action(&id, "to-todo", "В работу", IconName::ArrowRight, false, cx, |this, id, window, cx| {
            this.set_task_status(id, TaskStatus::Todo, window, cx)
        })],
        TaskStatus::Todo if open_deps.is_empty() => vec![
            action(&id, "run-claude", "Claude", IconName::Play, true, cx, |this, id, window, cx| {
                this.run_task(id, Provider::Claude, window, cx)
            }),
            action(&id, "run-codex", "Codex", IconName::Play, false, cx, |this, id, window, cx| {
                this.run_task(id, Provider::Codex, window, cx)
            }),
        ],
        TaskStatus::InProgress => vec![action(&id, "open", "Сессия", IconName::SquareTerminal, false, cx, |this, id, window, cx| {
            this.open_task_session(id, window, cx)
        })],
        TaskStatus::Blocked => vec![
            action(&id, "reply", "Ответить", IconName::SquareTerminal, true, cx, |this, id, window, cx| {
                this.open_task_session(id, window, cx)
            }),
            action(&id, "return", "Вернуть", IconName::Undo2, false, cx, |this, id, window, cx| {
                this.set_task_status(id, TaskStatus::Todo, window, cx)
            }),
        ],
        TaskStatus::Review => vec![
            action(&id, "accept", "Принять", IconName::Check, true, cx, |this, id, window, cx| {
                this.set_task_status(id, TaskStatus::Done, window, cx)
            }),
            action(&id, "reply", "Сессия", IconName::SquareTerminal, false, cx, |this, id, window, cx| {
                this.open_task_session(id, window, cx)
            }),
            action(&id, "return", "Вернуть", IconName::Undo2, false, cx, |this, id, window, cx| {
                this.set_task_status(id, TaskStatus::Todo, window, cx)
            }),
        ],
        _ => Vec::new(),
    };
    card.when(!actions.is_empty() && !done, |this| this.child(h_flex().flex_wrap().gap(px(5.)).children(actions)))
}

/// "◔ взял claude · ещё 12 мин": who holds the task and how long the lease lasts.
fn lease_line(who: &str, minutes: Option<i64>, running: bool, p: &Palette, theme: &Theme) -> impl IntoElement {
    let fraction = minutes.map_or(1., |m| (m as f32 / LEASE_MINUTES).clamp(0.08, 1.));
    h_flex()
        .gap(px(7.))
        .px(px(7.))
        .py(px(5.))
        .rounded(px(6.))
        .border_1()
        .border_color(p.border)
        .bg(p.surface2)
        .text_size(px(11.5))
        .text_color(p.muted)
        .child(if running {
            Spinner::new().xsmall().color(p.blue.solid).into_any_element()
        } else {
            lease_ring(fraction, p).into_any_element()
        })
        .child(h_flex().gap(px(4.)).child("взял").child(div().font_weight(FontWeight::SEMIBOLD).text_color(p.fg).child(who.to_owned())))
        .child(div().flex_1())
        .when_some(minutes, |this, m| this.child(ui::mono(format!("ещё {m} мин"), theme).text_size(px(10.5))))
}

/// A 14px ring whose inner disc shrinks as the lease runs out.
fn lease_ring(fraction: f32, p: &Palette) -> impl IntoElement {
    div()
        .size(px(14.))
        .flex_shrink_0()
        .rounded_full()
        .border_2()
        .border_color(p.blue.solid)
        .flex()
        .items_center()
        .justify_center()
        .child(div().size(px(6. * fraction)).rounded_full().bg(p.blue.solid))
}

#[allow(clippy::too_many_arguments)]
fn action(
    task: &str,
    key: &'static str,
    label: &'static str,
    icon: IconName,
    primary: bool,
    cx: &mut Context<AppView>,
    run: impl Fn(&mut AppView, &str, &mut Window, &mut Context<AppView>) + 'static,
) -> PressButton {
    let id = task.to_owned();
    PressButton::new(SharedString::from(format!("{key}:{task}")))
        .xsmall()
        .icon(icon)
        .label(label)
        .map(|b| if primary { b.primary() } else { b.outline() })
        .on_click(cx.listener(move |this, _, window, cx| {
            cx.stop_propagation();
            run(this, &id, window, cx)
        }))
}

fn status_tone(status: TaskStatus, p: &Palette) -> Tone {
    match status {
        TaskStatus::InProgress => p.blue,
        TaskStatus::Blocked | TaskStatus::Review => p.amber,
        TaskStatus::Done => p.green,
        TaskStatus::Todo => p.blue,
        TaskStatus::Inbox => Tone { solid: p.faint, soft: p.surface2, text: p.muted },
    }
}

fn render_detail(app: &AppView, task: &Task, p: &Palette, theme: &Theme, cx: &mut Context<AppView>) -> impl IntoElement {
    let journal = app.board.events(Some(&task.id));
    let deps: Vec<(String, bool)> = task
        .meta
        .depends_on
        .iter()
        .map(|d| {
            let t = app.tasks.iter().find(|t| &t.id == d);
            let title = t.map(|t| t.meta.title.clone()).unwrap_or_else(|| d.clone());
            (title, t.is_some_and(|t| t.meta.status == TaskStatus::Done))
        })
        .collect();
    let live = app.task_session(&task.id, cx).map(|(_, v)| v.read(cx).provider);
    let provider = task_provider(task, live);
    let blocked = task.meta.status == TaskStatus::Blocked;
    let review = task.meta.status == TaskStatus::Review;
    let id = task.id.clone();
    let tone = status_tone(task.meta.status, p);
    let label = |text: &'static str| ui::eyebrow(text, p, theme);

    v_flex()
        .id("task-detail")
        .absolute()
        .top_0()
        .right_0()
        .bottom_0()
        .w(px(440.))
        .bg(p.surface)
        .border_l_1()
        .border_color(p.border)
        .shadow(ui::popover_shadow())
        // Clicks inside the drawer must not reach the backdrop.
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(
            v_flex()
                .id("task-detail-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .px(px(20.))
                .py(px(18.))
                .gap(px(16.))
                .child(
                    v_flex()
                        .gap(px(8.))
                        .child(
                            h_flex()
                                .child(ui::mono(task.id.to_uppercase(), theme).text_color(p.muted))
                                .child(div().flex_1())
                                .child(
                                    PressButton::new("close-detail")
                                        .xsmall()
                                        .ghost()
                                        .icon(IconName::Close)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.selected_task = None;
                                            cx.notify();
                                        })),
                                ),
                        )
                        .child(div().text_size(px(20.)).font_weight(FontWeight::BOLD).line_height(px(25.)).text_color(p.ink).child(task.meta.title.clone()))
                        .child(
                            h_flex()
                                .gap(px(6.))
                                .flex_wrap()
                                .child(ui::dot_badge(task.meta.status.label(), tone))
                                .when_some(task.project_name(), |this, name| this.child(ui::chip(name, p, theme)))
                                .when_some(provider, |this, pr| this.child(ui::provider_chip(pr, p, theme)))
                                .child(div().flex_1())
                                .when_some(task.meta.updated_at, |this, t| this.child(ui::mono(ago(t), theme).text_color(p.faint))),
                        ),
                )
                .when_some(task.meta.summary.clone(), |this, s| {
                    let tone = if blocked { p.amber } else { p.green };
                    this.child(
                        v_flex()
                            .p(px(12.))
                            .gap(px(8.))
                            .rounded(px(10.))
                            .border_1()
                            .border_color(ui::mix(tone.solid, p.border, 0.55))
                            .bg(tone.soft)
                            .child(ui::eyebrow(if blocked { "Вопрос агента" } else { "Итог" }, p, theme).text_color(tone.text))
                            .child(div().text_size(px(13.5)).text_color(p.ink).child(TextView::markdown(SharedString::from(format!("summary:{}", task.id)), s).selectable(true)))
                            .when(blocked, |this| {
                                let id = id.clone();
                                this.child(
                                    h_flex()
                                        .id("detail-reply")
                                        .px(px(12.))
                                        .py(px(6.))
                                        .gap_2()
                                        .rounded(px(7.))
                                        .border_1()
                                        .border_color(p.border_strong)
                                        .bg(p.surface)
                                        .cursor_pointer()
                                        .hover(|s| s.border_color(p.ink))
                                        .on_click(cx.listener(move |this, _, window, cx| this.open_task_session(&id, window, cx)))
                                        .child(div().flex_1().text_size(px(13.)).text_color(p.faint).child("Ответ уйдёт в сессию агента…"))
                                        .child(
                                            div()
                                                .px(px(8.))
                                                .py(px(2.))
                                                .rounded(px(5.))
                                                .bg(p.ink)
                                                .text_size(px(12.))
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_color(p.surface)
                                                .child("Ответить"),
                                        ),
                                )
                            }),
                    )
                })
                .when(!task.body.trim().is_empty(), |this| {
                    this.child(
                        v_flex()
                            .gap(px(6.))
                            .child(label("Описание"))
                            .child(div().text_size(px(13.5)).text_color(p.fg).child(TextView::markdown(SharedString::from(format!("body:{}", task.id)), task.body.clone()).selectable(true))),
                    )
                })
                .when_some(task.meta.cwd.clone(), |this, cwd| {
                    this.child(v_flex().gap(px(6.)).child(label("Папка")).child(ui::mono(cwd.display().to_string(), theme).text_color(p.muted)))
                })
                .when(!task.meta.attachments.is_empty(), |this| {
                    this.child(
                        v_flex()
                            .gap(px(6.))
                            .child(label("Вложения"))
                            .child(attach::strip(&format!("task-file:{}", task.id), &task.meta.attachments, theme, None)),
                    )
                })
                .when(!deps.is_empty(), |this| {
                    this.child(v_flex().gap(px(6.)).child(label("Зависит от")).children(deps.into_iter().map(|(title, done)| {
                        h_flex()
                            .gap(px(6.))
                            .text_size(px(13.))
                            .child(Icon::new(if done { IconName::CircleCheck } else { IconName::Loader }).xsmall().text_color(if done { p.green.solid } else { p.faint }))
                            .child(div().text_color(if done { p.muted } else { p.fg }).child(title))
                    })))
                })
                .child(
                    v_flex()
                        .gap(px(8.))
                        .child(label("Журнал"))
                        .when(journal.is_empty(), |this| this.child(div().text_size(px(12.5)).text_color(p.faint).child("Пока пусто")))
                        .children(journal.into_iter().rev().take(40).map(|e| {
                            h_flex()
                                .gap(px(10.))
                                .items_start()
                                .child(
                                    ui::mono(e.at.with_timezone(&chrono::Local).format("%d.%m %H:%M").to_string(), theme)
                                        .w(px(84.))
                                        .flex_shrink_0()
                                        .text_color(p.faint),
                                )
                                .child(div().mt(px(3.)).size(px(9.)).flex_shrink_0().rounded_full().border_1().border_color(p.border_strong).bg(p.surface))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_size(px(12.5))
                                        .line_height(px(17.))
                                        .text_color(p.fg)
                                        .child(
                                            h_flex()
                                                .gap(px(4.))
                                                .flex_wrap()
                                                .child(div().font_weight(FontWeight::BOLD).text_color(p.ink).child(e.actor.clone()))
                                                .child(event_label(&e.kind))
                                                .when(!e.text.is_empty(), |this| this.child(div().text_color(p.fg).child(format!("· {}", e.text)))),
                                        ),
                                )
                        })),
                )
                .child(
                    ui::mono(
                        format!("Сессий: {} · ~/.iv3/tasks/{}.md", task.meta.sessions.len(), task.id),
                        theme,
                    )
                    .text_color(p.faint),
                ),
        )
        .child(
            h_flex()
                .px(px(20.))
                .py(px(12.))
                .gap_2()
                .border_t_1()
                .border_color(p.border)
                .when(review, |this| {
                    let id = id.clone();
                    this.child(
                        PressButton::new("detail-accept")
                            .small()
                            .primary()
                            .icon(IconName::Check)
                            .label("Принять")
                            .on_click(cx.listener(move |this, _, window, cx| this.set_task_status(&id, TaskStatus::Done, window, cx))),
                    )
                })
                .child({
                    let id = id.clone();
                    PressButton::new("detail-session")
                        .small()
                        .outline()
                        .icon(IconName::SquareTerminal)
                        .label("Открыть сессию")
                        .on_click(cx.listener(move |this, _, window, cx| this.open_task_session(&id, window, cx)))
                }),
        )
}

fn event_label(kind: &str) -> &'static str {
    match kind {
        "create" => "создал",
        "claim" => "взял в работу",
        "log" => "отметил",
        "complete" => "сдал на ревью",
        "block" => "спрашивает",
        "release" => "вернул в очередь",
        "status" => "сменил статус",
        _ => "",
    }
}
