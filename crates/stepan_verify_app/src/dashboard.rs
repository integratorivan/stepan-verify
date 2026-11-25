//! Home screen: what runs now, what waits for you, what happened recently.
//! Layout and tokens follow the approved prototype (PostHog warmth, Evil
//! Martians type, reui agent-activity cards).

use std::collections::BTreeMap;

use chrono::{Datelike, Local, Timelike};
use gpui_kit::component::{
    ActiveTheme, Icon, IconName, Sizable, Theme,
    h_flex,
    input::{Input, InputState},
    skeleton::Skeleton,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use crate::zoom::px;
use crate::ui::PressButton;
use stepan_verify_core::{SessionSummary, TaskStatus};

use crate::{
    app::{AppView, ExternalLive, LiveSession, Page},
    format::ago,
    live_card::{self, StepState},
    session::Status,
    ui::{self, Palette, Tone},
};

/// A session file touched this recently is probably being worked on right now.
const ACTIVE_WINDOW_SECS: i64 = 120;
const MAX_WIDTH: f32 = 1180.;
const PROJECTS: usize = 12;
const ROWS_PER_PROJECT: usize = 5;

/// Sessions not worth listing: stepan_verify's own change-report runs and throwaway dirs.
fn listed(s: &SessionSummary) -> bool {
    let tmp = s.cwd.as_ref().is_some_and(|c| {
        c.starts_with("/private/tmp") || c.starts_with("/tmp") || c.starts_with("/private/var/folders") || c.starts_with("/var/folders")
    });
    !tmp && !is_internal(&s.title)
}

/// stepan_verify's own helper sessions (change reviewer, map batches, before/after media):
/// their first prompt starts with one of these.
pub fn is_internal(title: &str) -> bool {
    // `[iv3` — helper sessions from before the rename.
    const PREFIXES: [&str; 5] = ["[stepan_verify", "[iv3", "Ты — ревьюер изменений", "Ты ведёшь карту изменений", "Ты — фотограф изменений"];
    let t = title.trim_start();
    PREFIXES.iter().any(|p| t.starts_with(p))
}

pub fn render(app: &AppView, window: &mut Window, cx: &mut Context<AppView>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let theme = &theme;
    let p = Palette::of(theme);
    let p = &p;
    // «Недавние» shows either the live list or the archive.
    let archive_toggle = window.use_keyed_state("dash-archive-view", cx, |_, _| false);
    let archive_view = *archive_toggle.read(cx);
    let sessions: Vec<&SessionSummary> = app.sessions.iter().filter(|s| listed(s) && !app.archive.contains(s.provider, &s.id)).collect();
    let archived: Vec<&SessionSummary> = app.sessions.iter().filter(|s| app.archive.contains(s.provider, &s.id)).collect();
    let archived_count = archived.len();
    let now = chrono::Utc::now();
    let today = sessions.iter().filter(|s| s.updated_at > now - chrono::Duration::hours(24)).count();
    let yesterday = sessions
        .iter()
        .filter(|s| s.updated_at <= now - chrono::Duration::hours(24) && s.updated_at > now - chrono::Duration::hours(48))
        .count();
    // Sessions per day for the last 9 days, oldest first.
    let spark: Vec<usize> = (0..9)
        .rev()
        .map(|d| {
            let (from, to) = (now - chrono::Duration::days(d + 1), now - chrono::Duration::days(d));
            sessions.iter().filter(|s| s.updated_at > from && s.updated_at <= to).count()
        })
        .collect();
    let in_progress = app.tasks.iter().filter(|t| t.meta.status == TaskStatus::InProgress).count();
    let blocked = app.tasks.iter().filter(|t| t.meta.status == TaskStatus::Blocked).count();
    let review = app.tasks.iter().filter(|t| t.meta.status == TaskStatus::Review).count();
    let limit = crate::settings::config(cx).agents.autopilot_limit;

    let mut by_project: BTreeMap<String, Vec<&SessionSummary>> = BTreeMap::new();
    // Codex Desktop keeps each chat in its own dir: one group, not fifty projects.
    let codex_chats = std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join("Documents/Codex"));
    for s in if archive_view { &archived } else { &sessions } {
        let chat = matches!((&s.cwd, &codex_chats), (Some(c), Some(root)) if c.starts_with(root));
        let name = if chat { "Чаты Codex".to_owned() } else { s.project() };
        by_project.entry(name).or_default().push(s);
    }
    let mut projects: Vec<(String, Vec<&SessionSummary>)> = by_project.into_iter().collect();
    projects.sort_by(|a, b| b.1[0].updated_at.cmp(&a.1[0].updated_at));
    let project_count = projects.len();
    let busiest = projects
        .iter()
        .max_by_key(|(_, l)| l.iter().filter(|s| s.updated_at > now - chrono::Duration::days(7)).count())
        .map(|(n, _)| n.clone());

    let live = app.live_sessions(cx);
    let inside = live.len();
    let outside = app.external.len();
    let working = live.iter().filter(|s| s.activity.status == Status::Running).count() + app.external.iter().filter(|e| e.running).count();
    let yours = live.iter().filter(|s| s.activity.status == Status::Idle || s.activity.approvals > 0).count()
        + app.external.iter().filter(|e| !e.running).count();
    let loading = app.sessions_loading && app.sessions.is_empty();

    // What waits for you first: approvals, questions, then running, then idle.
    let mut live = live;
    live.sort_by_key(|s| urgency(s));
    let mut cards: Vec<AnyElement> = live.into_iter().map(|s| open_card(s, p, theme, window, cx).into_any_element()).collect();
    let mut external: Vec<&ExternalLive> = app.external.iter().collect();
    external.sort_by_key(|e| if e.running { 2 } else { 3 });
    cards.extend(external.into_iter().map(|e| external_card(e, p, theme, window, cx).into_any_element()));

    div().id("dashboard").size_full().overflow_y_scroll().child(
        v_flex()
            .w_full()
            .max_w(px(MAX_WIDTH))
            .mx_auto()
            .px(px(32.))
            .pt(px(28.))
            .pb(px(40.))
            // Header
            .child(
                h_flex()
                    .items_end()
                    .gap_3()
                    .mb(px(22.))
                    .child(
                        v_flex()
                            .flex_1()
                            .gap(px(6.))
                            .child(ui::eyebrow(date_line(), p, theme))
                            .child(ui::page_title("Дашборд", p))
                            .child(summary(working, yours, review + blocked, app.autopilot, p)),
                    )
                    .child(
                        PressButton::new("refresh")
                            .ghost()
                            .small()
                            .icon(IconName::RefreshCw)
                            .loading(app.sessions_loading)
                            .label(if app.sessions_loading { "Сканирую…" } else { "Обновить" })
                            .on_click(cx.listener(|this, _, window, cx| this.refresh_sessions(window, cx))),
                    )
            )
            // Numbers
            .child(
                ui::card(p).overflow_hidden().child(
                    h_flex()
                        .items_stretch()
                        .child(metric(
                            "Сессий за 24 ч",
                            (!loading).then_some(today),
                            Some(delta_line(today, yesterday)),
                            Some(spark),
                            None,
                            p,
                            theme,
                        ))
                        .child(metric("Задач в работе", Some(in_progress), Some(format!("лимит автопилота — {limit}")), None, None, p, theme).border_l_1())
                        .child(
                            metric(
                                "Ждут тебя",
                                Some(blocked + review),
                                Some(format!("{blocked} {} · {review} на ревью", questions_word(blocked))),
                                None,
                                (blocked + review > 0).then_some(p.amber),
                                p,
                                theme,
                            )
                            .border_l_1(),
                        )
                        .child(
                            metric(
                                "Проектов",
                                (!loading).then_some(project_count),
                                busiest.map(|b| format!("{b} активнее всех")),
                                None,
                                None,
                                p,
                                theme,
                            )
                            .border_l_1(),
                        ),
                ),
            )
            // Live sessions
            .child(
                h_flex()
                    .mt(px(30.))
                    .mb(px(12.))
                    .gap_3()
                    .child(div().flex_1().child(ui::section_title("Сейчас", (!cards.is_empty()).then(|| cards.len().to_string()), p, theme)))
                    .when(!cards.is_empty(), |this| {
                        this.child(ui::eyebrow(format!("сначала то, что ждёт тебя · {inside} в Stepan · {outside} вне Stepan"), p, theme).text_color(p.faint))
                    }),
            )
            .map(|this| {
                if cards.is_empty() {
                    return this.child(empty_live(p, theme, cx));
                }
                let mut rows = Vec::new();
                let mut it = cards.into_iter();
                while let Some(first) = it.next() {
                    let second = it.next().unwrap_or_else(|| div().into_any_element());
                    rows.push(
                        h_flex()
                            .items_start()
                            .gap(px(14.))
                            .child(div().flex_1().min_w_0().child(first))
                            .child(div().flex_1().min_w_0().child(second)),
                    );
                }
                this.child(v_flex().gap(px(14.)).children(rows))
            })
            // History by project
            .child(
                div()
                    .mt(px(30.))
                    .mb(px(12.))
                    .child(
                        h_flex()
                            .gap_3()
                            .child(div().flex_1().child(ui::section_title(
                                if archive_view { "Архив" } else { "Недавние" },
                                (!loading).then(|| if archive_view { archived_count } else { project_count }.to_string()),
                                p,
                                theme,
                            )))
                            .when(archive_view || archived_count > 0, |this| {
                                let toggle = archive_toggle.clone();
                                this.child(
                                    PressButton::new("dash-archive-toggle")
                                        .ghost()
                                        .small()
                                        .icon(Icon::empty().path(if archive_view { "icons/arrow-left.svg" } else { "icons/archive.svg" }))
                                        .label(if archive_view { "К недавним".to_owned() } else { format!("Архив · {archived_count}") })
                                        .on_click(move |_, _, cx| {
                                            toggle.update(cx, |v, cx| {
                                                *v = !*v;
                                                cx.notify();
                                            })
                                        }),
                                )
                            }),
                    ),
            )
            .map(|this| {
                if loading {
                    return this.child(
                        h_flex()
                            .items_start()
                            .gap(px(14.))
                            .child(div().flex_1().child(project_skeleton(0, 4, p, theme)))
                            .child(div().flex_1().child(project_skeleton(1, 3, p, theme))),
                    );
                }
                let mut left = v_flex().flex_1().min_w_0().gap(px(14.));
                let mut right = v_flex().flex_1().min_w_0().gap(px(14.));
                if archive_view && projects.is_empty() {
                    return this.child(div().text_size(px(13.)).text_color(p.muted).child("Архив пуст"));
                }
                for (i, (project, list)) in projects.into_iter().take(PROJECTS).enumerate() {
                    let panel = project_panel(project, list, archive_view, p, theme, cx);
                    if i % 2 == 0 {
                        left = left.child(panel);
                    } else {
                        right = right.child(panel);
                    }
                }
                this.child(h_flex().items_start().gap(px(14.)).child(left).child(right))
            }),
    )
}

// ------------------------------------------------------------- header

fn date_line() -> String {
    const DAYS: [&str; 7] = ["понедельник", "вторник", "среда", "четверг", "пятница", "суббота", "воскресенье"];
    const MONTHS: [&str; 12] =
        ["января", "февраля", "марта", "апреля", "мая", "июня", "июля", "августа", "сентября", "октября", "ноября", "декабря"];
    let now = Local::now();
    format!(
        "{}, {} {} · {:02}:{:02}",
        DAYS[now.weekday().num_days_from_monday() as usize],
        now.day(),
        MONTHS[now.month0() as usize],
        now.hour(),
        now.minute()
    )
}

fn summary(working: usize, yours: usize, review: usize, autopilot: bool, p: &Palette) -> impl IntoElement {
    let item = |dot: Option<Hsla>, label: &str, value: String| {
        h_flex()
            .gap(px(6.))
            .when_some(dot, |d, c| d.child(ui::dot(c)))
            .child(label.to_owned())
            .child(div().font_weight(FontWeight::BOLD).text_color(p.fg).child(value))
    };
    h_flex()
        .flex_wrap()
        .gap(px(14.))
        .text_size(px(14.))
        .text_color(p.muted)
        // Zeros are noise: only what is happening shows up.
        .when(working > 0, |d| d.child(item(Some(p.blue.solid), "работает", agents_phrase(working))))
        .when(yours > 0, |d| d.child(item(Some(p.amber.solid), "ждут тебя", yours.to_string())))
        .when(review > 0, |d| d.child(item(None, "задач на ревью:", review.to_string())))
        .when(working + yours + review == 0, |d| d.child("все агенты свободны"))
        .child(item(None, "автопилот:", if autopilot { "включён".into() } else { "выключен".into() }))
}

fn agents_phrase(n: usize) -> String {
    let w = match (n % 10, n % 100) {
        (1, r) if r != 11 => "агент",
        (2..=4, r) if !(12..=14).contains(&r) => "агента",
        _ => "агентов",
    };
    format!("{n} {w}")
}

fn questions_word(n: usize) -> &'static str {
    match (n % 10, n % 100) {
        (1, r) if r != 11 => "вопрос",
        (2..=4, r) if !(12..=14).contains(&r) => "вопроса",
        _ => "вопросов",
    }
}

fn delta_line(today: usize, yesterday: usize) -> String {
    let d = today as i64 - yesterday as i64;
    match d {
        0 => "как вчера".into(),
        d if d > 0 => format!("+{d} к вчера"),
        d => format!("{d} к вчера"),
    }
}

fn metric(
    label: &'static str,
    value: Option<usize>,
    sub: Option<String>,
    spark: Option<Vec<usize>>,
    hot: Option<Tone>,
    p: &Palette,
    theme: &Theme,
) -> Div {
    v_flex()
        .flex_1()
        .min_w_0()
        .relative()
        .px(px(16.))
        .py(px(11.))
        .gap(px(6.))
        .border_color(p.border)
        .when_some(hot, |d, t| d.child(div().absolute().top_0().left_0().right_0().h(px(3.)).bg(t.soft)))
        .child(ui::eyebrow(label, p, theme).when_some(hot, |d, t| d.text_color(t.text)))
        .child(
            h_flex()
                .items_end()
                .gap(px(8.))
                .child(match value {
                    Some(v) => div()
                        .text_size(px(22.))
                        .line_height(px(24.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(hot.map_or(p.ink, |t| t.text))
                        .child(v.to_string())
                        .into_any_element(),
                    None => Skeleton::new().w(px(36.)).h(px(22.)).rounded(px(5.)).into_any_element(),
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .pb(px(2.))
                        .truncate()
                        .text_size(px(12.))
                        .text_color(p.muted)
                        .when_some(sub, |d, s| d.child(s)),
                )
                .when_some(spark, |this, bars| this.child(sparkline(&bars, p))),
        )
}

fn sparkline(bars: &[usize], p: &Palette) -> impl IntoElement {
    let max = bars.iter().copied().max().unwrap_or(0).max(1) as f32;
    let last = bars.len().saturating_sub(1);
    h_flex().items_end().gap(px(2.)).h(px(22.)).children(bars.iter().enumerate().map(|(i, &v)| {
        div()
            .w(px(4.))
            .h(px(3. + 19. * v as f32 / max))
            .rounded(px(1.))
            .bg(if i == last { p.ink } else { p.border_strong })
    }))
}

// ------------------------------------------------------------- live cards

/// Lower sorts first: approvals, questions, running, idle.
fn urgency(s: &LiveSession) -> u8 {
    let asks = s.card.steps.iter().any(|st| st.state == StepState::NeedsAnswer);
    match (s.activity.approvals, s.activity.status) {
        (n, _) if n > 0 => 0,
        (_, Status::Idle) if asks => 1,
        (_, Status::Running | Status::Loading) => 2,
        _ => 3,
    }
}

struct CardHead {
    title: SharedString,
    badge: (&'static str, Tone),
    provider: stepan_verify_core::Provider,
    project: SharedString,
    outside: bool,
}

/// "✓ 3 шага готово · 2:52 ⌄": folds finished steps; a click unfolds them.
fn fold_row(key: &str, card: &live_card::Card, p: &Palette, theme: &Theme, window: &mut Window, cx: &mut Context<AppView>) -> Option<(AnyElement, bool)> {
    let done = card.done_total();
    if done == 0 {
        return None;
    }
    let open = window.use_keyed_state(SharedString::from(format!("fold:{key}")), cx, |_, _| false);
    let expanded = *open.read(cx);
    let row = h_flex()
        .id(SharedString::from(format!("fold-row:{key}")))
        .h(px(30.))
        .px_3()
        .gap(px(9.))
        .border_b_1()
        .border_color(p.border)
        .cursor_pointer()
        .hover(|s| s.bg(p.surface2))
        .on_click(move |_, _, cx| {
            cx.stop_propagation();
            open.update(cx, |v, cx| {
                *v = !*v;
                cx.notify();
            });
        })
        .child(Icon::new(IconName::CircleCheck).small().text_color(p.green.solid))
        .child(div().flex_1().text_size(px(12.5)).text_color(p.muted).child(format!("{done} {} готово", live_card::steps_word(done))))
        .when_some(card.done_secs, |d, secs| {
            d.child(div().font_family(theme.mono_font_family.clone()).text_size(px(11.5)).text_color(p.faint).child(live_card::mmss(secs)))
        })
        .child(Icon::new(if expanded { IconName::ChevronUp } else { IconName::ChevronDown }).xsmall().text_color(p.faint))
        .into_any_element();
    Some((row, !expanded))
}

/// Plan progress as dashes: done, current, pending.
fn plan_dashes(plan: Option<(usize, usize)>, p: &Palette) -> Option<impl IntoElement> {
    let (done, total) = plan?;
    Some(h_flex().gap(px(2.)).children((0..total.min(14)).map(move |i| {
        let color = if i < done {
            p.blue.solid
        } else if i == done {
            p.blue.solid.opacity(0.45)
        } else {
            p.border_strong.opacity(0.55)
        };
        div().w(px(8.)).h(px(3.)).rounded_full().bg(color)
    })))
}

/// Session card ("Пульт"): header on the frame, steps and the decision inside,
/// progress and actions on the footer.
fn run_card(
    key: String,
    head: CardHead,
    card: &live_card::Card,
    footer: String,
    actions: Vec<PressButton>,
    attention: Option<Tone>,
    fold: Option<(AnyElement, bool)>,
    gate: Option<AnyElement>,
    ask: Option<AnyElement>,
    p: &Palette,
    theme: &Theme,
) -> Stateful<Div> {
    let (label, tone) = head.badge;
    ui::frame_toned(SharedString::from(format!("live:{key}")), p, attention)
        .pt_0()
        .cursor_pointer()
        .hover(|this| this.border_color(p.border_strong))
        .child(
            v_flex()
                .pl(px(10.))
                .pr(px(8.))
                .pt(px(11.))
                .pb(px(10.))
                .gap(px(5.))
                .child(
                    h_flex()
                        .gap_2()
                        .min_w_0()
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_size(px(14.5))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(p.ink)
                                .child(head.title),
                        )
                        .child(ui::dot_badge(label, tone)),
                )
                .child(
                    h_flex()
                        .gap(px(6.))
                        .text_size(px(12.))
                        .text_color(p.muted)
                        .child(ui::provider_chip(head.provider, p, theme))
                        .child(head.project)
                        .when(head.outside, |this| {
                            this.child(
                                div()
                                    .px(px(6.))
                                    .rounded(px(4.))
                                    .border_1()
                                    .border_dashed()
                                    .border_color(p.border_strong)
                                    .font_family(theme.mono_font_family.clone())
                                    .text_size(px(10.5))
                                    .text_color(p.muted)
                                    .child("ВНЕ STEPAN"),
                            )
                        }),
                ),
        )
        .child(ui::inner(p).child(live_card::render_steps(&key, card, p, theme, fold, gate, ask)))
        .child(
            h_flex()
                .pl(px(10.))
                .pr(px(2.))
                .py(px(7.))
                .gap(px(10.))
                .children(plan_dashes(card.plan, p))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .font_family(theme.mono_font_family.clone())
                        .text_size(px(11.5))
                        .text_color(p.muted)
                        .child(footer),
                )
                .children(actions),
        )
}

fn open_card(s: LiveSession, p: &Palette, theme: &Theme, window: &mut Window, cx: &mut Context<AppView>) -> impl IntoElement {
    let a = &s.activity;
    let asks = s.card.steps.iter().any(|st| st.state == StepState::NeedsAnswer);
    let (badge, attention) = match (a.status, a.approvals) {
        (_, n) if n > 0 => (("Ждёт подтверждения", p.red), Some(p.red)),
        (Status::Running, _) => (("Работает", p.blue), None),
        (Status::Loading, _) => (("Загрузка", ui::Tone { solid: p.faint, soft: p.surface2, text: p.muted }), None),
        (Status::Exited, _) => (("Процесс завершён", p.red), None),
        (Status::Idle, _) if asks => (("Нужен ответ", p.amber), Some(p.amber)),
        (Status::Idle, _) => (("Ждёт тебя", p.amber), None),
    };
    let progress = lower_first(&s.card.progress);
    let footer = match s.elapsed {
        Some(secs) => format!("{progress} · {}", live_card::mmss(secs)),
        None => progress,
    };
    let ix = s.ix;
    let key = format!("open-{ix}");
    let running = a.status == Status::Running;

    let gate = (a.approvals > 0).then(|| {
        h_flex()
            .gap(px(6.))
            .child(
                PressButton::new(SharedString::from(format!("live-allow:{ix}")))
                    .primary()
                    .xsmall()
                    .icon(IconName::Check)
                    .label("Разрешить")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        if let Some(view) = this.open_sessions.get(ix).cloned() {
                            view.update(cx, |v, _| v.decide_first(true));
                        }
                    })),
            )
            .child(
                PressButton::new(SharedString::from(format!("live-deny:{ix}")))
                    .xsmall()
                    .label("Отклонить")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        if let Some(view) = this.open_sessions.get(ix).cloned() {
                            view.update(cx, |v, _| v.decide_first(false));
                        }
                    })),
            )
            .into_any_element()
    });

    // Answer the agent right on the card.
    let ask = asks.then(|| {
        let reply = window.use_keyed_state(SharedString::from(format!("reply:{ix}")), cx, |window, cx| {
            InputState::new(window, cx).placeholder("Ответить агенту…")
        });
        let send = reply.clone();
        h_flex()
            .gap(px(6.))
            .items_center()
            .child(div().flex_1().min_w_0().child(Input::new(&reply).small()))
            .child(
                PressButton::new(SharedString::from(format!("live-send:{ix}")))
                    .small()
                    .icon(IconName::ArrowUp)
                    .label("Отправить")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        let text = send.read(cx).value().trim().to_owned();
                        if text.is_empty() {
                            return;
                        }
                        if let Some(view) = this.open_sessions.get(ix).cloned() {
                            view.update(cx, |v, cx| v.send_text(text, cx));
                        }
                        send.update(cx, |s, cx| s.set_value("", window, cx));
                    })),
            )
            .into_any_element()
    });

    let mut actions = Vec::new();
    if running {
        actions.push(
            PressButton::new(SharedString::from(format!("live-pause:{ix}")))
                .xsmall()
                .icon(IconName::Pause)
                .label("Пауза")
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.interrupt_session(ix, cx);
                })),
        );
    }
    actions.push(
        PressButton::new(SharedString::from(format!("live-open:{ix}")))
            .xsmall()
            .label("Открыть")
            .icon(IconName::ArrowRight)
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.show(Page::Session(ix), cx);
            })),
    );
    let fold = fold_row(&key, &s.card, p, theme, window, cx);
    let head = CardHead { title: s.title.clone(), badge, provider: s.provider, project: s.project.clone(), outside: false };
    run_card(key, head, &s.card, footer, actions, attention, fold, gate, ask, p, theme)
        .on_click(cx.listener(move |this, _, _, cx| this.show(Page::Session(ix), cx)))
}

fn external_card(e: &ExternalLive, p: &Palette, theme: &Theme, window: &mut Window, cx: &mut Context<AppView>) -> impl IntoElement {
    let badge = if e.running { ("Работает", p.blue) } else { ("Ждёт тебя", p.amber) };
    let footer = format!("{} · {}", lower_first(&e.card.progress), ago(e.summary.updated_at));
    let summary = e.summary.clone();
    let open = summary.clone();
    let action = PressButton::new(SharedString::from(format!("ext-open:{}", e.summary.id)))
        .xsmall()
        .icon(IconName::ArrowRight)
        .label("Открыть")
        .on_click(cx.listener(move |this, _, window, cx| {
            cx.stop_propagation();
            this.open_session(&open, window, cx);
        }));
    let (provider, id) = (e.summary.provider, e.summary.id.clone());
    let put_away = PressButton::new(SharedString::from(format!("ext-archive:{}", e.summary.id)))
        .xsmall()
        .icon(Icon::empty().path("icons/archive.svg"))
        .tooltip("В архив: скрыть с дашборда")
        .on_click(cx.listener(move |this, _, window, cx| {
            cx.stop_propagation();
            this.set_archived(provider, &id, true, window, cx);
        }));
    let key = format!("ext-{}", e.summary.id);
    let fold = fold_row(&key, &e.card, p, theme, window, cx);
    let head = CardHead {
        title: e.summary.title.clone().into(),
        badge,
        provider: e.summary.provider,
        project: e.summary.project().into(),
        outside: true,
    };
    run_card(key, head, &e.card, footer, vec![put_away, action], None, fold, None, None, p, theme)
        .on_click(cx.listener(move |this, _, window, cx| this.open_session(&summary, window, cx)))
}

fn lower_first(s: &str) -> String {
    let mut chars = s.chars();
    chars.next().map(|c| c.to_lowercase().chain(chars).collect()).unwrap_or_default()
}

fn empty_live(p: &Palette, _theme: &Theme, cx: &mut Context<AppView>) -> impl IntoElement {
    ui::frame("live-empty", p).child(
        ui::inner(p).child(
            h_flex()
                .px_4()
                .py_4()
                .gap_3()
                .child(Icon::new(IconName::CircleCheck).small().text_color(p.muted))
                .child(
                    v_flex()
                        .flex_1()
                        .gap(px(2.))
                        .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).child("Сейчас никто не работает"))
                        .child(div().text_size(px(12.5)).text_color(p.muted).child("Запусти агента или продолжи сессию из списка ниже")),
                )
                .child(
                    PressButton::new("empty-new-session")
                        .xsmall()
                        .outline()
                        .icon(IconName::Plus)
                        .label("Новая сессия")
                        .on_click(cx.listener(|this, _, window, cx| this.open_new_session_dialog(window, cx)))),
        ),
    )
}

// ------------------------------------------------------------- history

/// Stable color for a project's mark.
fn project_color(name: &str) -> Hsla {
    const HUES: [f32; 8] = [16., 228., 145., 38., 280., 190., 340., 95.];
    let h = name.bytes().fold(0u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32));
    hsla(HUES[(h % 8) as usize] / 360., 0.62, 0.48, 1.)
}

fn initials(name: &str) -> String {
    let parts: Vec<&str> = name.split(|c: char| !c.is_alphanumeric()).filter(|s| !s.is_empty()).collect();
    let s: String = match parts.as_slice() {
        [] => "?".into(),
        [one] => one.chars().take(2).collect(),
        [a, b, ..] => a.chars().take(1).chain(b.chars().take(1)).collect(),
    };
    s.to_uppercase()
}

fn short_path(path: &std::path::Path) -> String {
    let s = path.display().to_string();
    match std::env::var("HOME") {
        Ok(home) if s.starts_with(&home) => format!("~{}", &s[home.len()..]),
        _ => s,
    }
}

fn project_panel(project: String, list: Vec<&SessionSummary>, archived: bool, p: &Palette, theme: &Theme, cx: &mut Context<AppView>) -> impl IntoElement {
    let total = list.len();
    let hidden = total.saturating_sub(ROWS_PER_PROJECT);
    let path = if project == "Чаты Codex" {
        "~/Documents/Codex".to_owned()
    } else {
        list.first().and_then(|s| s.cwd.as_deref()).map(|c| short_path(&stepan_verify_core::worktree::project_root(c))).unwrap_or_default()
    };
    let mono = theme.mono_font_family.clone();
    ui::card(p)
        .id(SharedString::from(format!("project:{project}")))
        .overflow_hidden()
        .child(
            h_flex()
                .px(px(14.))
                .py(px(11.))
                .gap(px(10.))
                .border_b_1()
                .border_color(p.border)
                .child(
                    div()
                        .size(px(26.))
                        .flex_shrink_0()
                        .rounded(px(6.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(project_color(&project))
                        .font_family(mono.clone())
                        .text_size(px(11.))
                        .font_weight(FontWeight::BOLD)
                        .text_color(gpui_kit::white())
                        .child(initials(&project)),
                )
                .child(
                    v_flex()
                        .min_w_0()
                        .flex_1()
                        .child(div().truncate().text_size(px(14.5)).font_weight(FontWeight::BOLD).text_color(p.ink).child(project))
                        .child(div().truncate().font_family(mono.clone()).text_size(px(11.)).text_color(p.faint).child(path)),
                )
                .child(div().flex_shrink_0().font_family(mono.clone()).text_size(px(11.5)).text_color(p.muted).child(sessions_word(total))),
        )
        .children(list.into_iter().take(ROWS_PER_PROJECT).enumerate().map(|(i, s)| session_row(i, s, archived, p, theme, cx)))
        .when(hidden > 0, |this| {
            this.child(
                div()
                    .px(px(14.))
                    .py(px(8.))
                    .border_t_1()
                    .border_dashed()
                    .border_color(p.border)
                    .font_family(mono)
                    .text_size(px(11.5))
                    .text_color(p.faint)
                    .child(format!("ещё {hidden}")),
            )
        })
}

fn session_row(i: usize, s: &SessionSummary, archived: bool, p: &Palette, theme: &Theme, cx: &mut Context<AppView>) -> impl IntoElement {
    let open = s.clone();
    let (provider, id) = (s.provider, s.id.clone());
    let active = (chrono::Utc::now() - s.updated_at).num_seconds() < ACTIVE_WINDOW_SECS;
    // Ran in a worktree of the project (a task's copy): say which branch.
    let branch = s.cwd.as_deref().and_then(stepan_verify_core::worktree::linked).map(|l| {
        let b = l.branch.unwrap_or_else(|| "worktree".into());
        match b.char_indices().nth(24) {
            Some((i, _)) => format!("{}…", &b[..i]),
            None => b,
        }
    });
    h_flex()
        .id(SharedString::from(format!("{}:{}", s.provider.label(), s.id)))
        .group(SharedString::from("session-row"))
        .h(px(36.))
        .px(px(14.))
        .gap(px(10.))
        .items_center()
        .when(i > 0, |d| d.border_t_1().border_color(ui::mix(p.border, p.surface, 0.6)))
        .cursor_pointer()
        .hover(|this| this.bg(p.surface2))
        .on_click(cx.listener(move |this, _, window, cx| this.open_session(&open, window, cx)))
        .child(if active {
            div().size(px(8.)).flex_shrink_0().rounded_full().bg(p.green.solid).border_1().border_color(p.green.solid).shadow(vec![BoxShadow {
                color: p.green.soft,
                offset: point(px(0.), px(0.)),
                blur_radius: px(0.),
                spread_radius: px(3.),
                inset: false,
            }])
        } else {
            div().size(px(8.)).flex_shrink_0().rounded_full().border(px(1.5)).border_color(p.border_strong)
        })
        .child(div().w(px(56.)).flex_shrink_0().child(ui::provider_chip(s.provider, p, theme).whitespace_nowrap()))
        .child(div().flex_1().min_w_0().truncate().text_size(px(13.)).text_color(p.fg).child(s.title.clone()))
        .when_some(branch, |this, b| this.child(ui::branch_chip(b, p, theme)))
        .child(div().flex_shrink_0().font_family(theme.mono_font_family.clone()).text_size(px(11.5)).text_color(p.faint).child(ago(s.updated_at)))
        // Archive (or restore): shows on hover so the list stays calm.
        .child(
            div()
                .id(SharedString::from(format!("archive:{}:{}", s.provider.label(), s.id)))
                .flex_shrink_0()
                .size(px(22.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(5.))
                .when(!archived, |d| d.opacity(0.).group_hover("session-row", |st| st.opacity(1.)))
                .hover(|st| st.bg(p.ink.opacity(0.08)))
                .cursor_pointer()
                .tooltip(move |window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(if archived { "Вернуть из архива" } else { "В архив" }).build(window, cx)
                })
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.set_archived(provider, &id, !archived, window, cx);
                }))
                .child(
                    Icon::empty()
                        .path(if archived { "icons/archive-restore.svg" } else { "icons/archive.svg" })
                        .xsmall()
                        .text_color(p.muted),
                ),
        )
}

fn sessions_word(n: usize) -> String {
    let w = match (n % 10, n % 100) {
        (1, r) if r != 11 => "сессия",
        (2..=4, r) if !(12..=14).contains(&r) => "сессии",
        _ => "сессий",
    };
    format!("{n} {w}")
}

fn project_skeleton(key: usize, rows: usize, p: &Palette, theme: &Theme) -> impl IntoElement {
    const WIDTHS: [f32; 5] = [0.62, 0.45, 0.71, 0.38, 0.55];
    let _ = theme;
    ui::card(p)
        .id(SharedString::from(format!("project-skeleton-{key}")))
        .overflow_hidden()
        .child(
            h_flex()
                .px(px(14.))
                .py(px(11.))
                .gap(px(10.))
                .border_b_1()
                .border_color(p.border)
                .child(Skeleton::new().size(px(26.)).rounded(px(6.)))
                .child(v_flex().gap_1().child(Skeleton::new().w(px(110.)).h_3().rounded(px(4.))).child(Skeleton::new().secondary().w(px(160.)).h(px(9.)).rounded(px(4.)))),
        )
        .children((0..rows).map(|i| {
            h_flex()
                .h(px(36.))
                .px(px(14.))
                .gap(px(10.))
                .child(Skeleton::new().secondary().size(px(8.)).rounded_full())
                .child(Skeleton::new().secondary().w(px(46.)).h_4().rounded(px(4.)))
                .child(div().flex_1().child(Skeleton::new().w(relative(WIDTHS[(key + i) % WIDTHS.len()])).h_3().rounded(px(4.))))
                .child(Skeleton::new().secondary().w(px(36.)).h_3().rounded(px(4.)))
        }))
}
