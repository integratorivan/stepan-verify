//! Скиллы · «Связи»: every skill on the machine, who can see it (Claude /
//! Codex / stepan_verify), how often each agent used it in the last two weeks and in
//! which sessions, plus the problems that keep agents from loading it.

use std::{collections::HashSet, path::PathBuf, sync::Arc, time::Instant};

use chrono::{Datelike, Duration as ChronoDuration, Utc};
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, Theme, h_flex,
    input::{Input, InputEvent, InputState},
    skeleton::Skeleton,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use stepan_verify_core::{
    Home, Provider, SessionSummary,
    skills::{Method, Problem, Report, Severity, Skill, Use, tilde},
};

use crate::{
    format::ago,
    ui::{self, Palette, PressButton, Tone},
};

const WINDOW_DAYS: i64 = 14;
/// Rescan when the page is shown and the data is older than this.
const STALE_SECS: u64 = 120;
const HISTORY_ROWS: usize = 12;

pub enum SkillsEvent {
    OpenSession(SessionSummary),
    OpenFile(PathBuf),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Called,
    All,
    Idle,
    Problems,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AgentFilter {
    All,
    Claude,
    Codex,
}

/// One table row, precomputed from the report.
struct Row {
    skill: Skill,
    claude14: usize,
    codex14: usize,
    /// Per day, oldest first: (claude, codex).
    days: Vec<(usize, usize)>,
    last: Option<Use>,
    sessions: usize,
    problems: usize,
}

impl Row {
    fn total14(&self) -> usize {
        self.claude14 + self.codex14
    }
}

pub struct SkillsView {
    home: Home,
    report: Option<Arc<Report>>,
    rows: Vec<Row>,
    loading: bool,
    scanned: Option<Instant>,
    tab: Tab,
    agent: AgentFilter,
    search: Entity<InputState>,
    expanded: HashSet<String>,
    selected: usize,
    focus: FocusHandle,
    _scan: Task<()>,
    _search_sub: Subscription,
}

impl EventEmitter<SkillsEvent> for SkillsView {}

impl SkillsView {
    pub fn new(home: Home, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Найти скилл"));
        let _search_sub = cx.subscribe(&search, |this: &mut Self, _, _: &InputEvent, cx| {
            this.selected = 0;
            cx.notify();
        });
        let mut this = Self {
            home,
            report: None,
            rows: Vec::new(),
            loading: false,
            scanned: None,
            tab: Tab::Called,
            agent: AgentFilter::All,
            search,
            expanded: HashSet::new(),
            selected: 0,
            focus: cx.focus_handle(),
            _scan: Task::ready(()),
            _search_sub,
        };
        this.refresh(cx);
        this
    }

    /// Skills on disk (for the sidebar counter); None until the first scan.
    pub fn count(&self) -> Option<usize> {
        self.report.as_ref().map(|r| r.skills.iter().filter(|s| !s.plugin).count())
    }

    pub fn refresh_if_stale(&mut self, cx: &mut Context<Self>) {
        if self.scanned.is_none_or(|t| t.elapsed().as_secs() > STALE_SECS) {
            self.refresh(cx);
        }
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        self.loading = true;
        let home = self.home.clone();
        self._scan = cx.spawn(async move |this, cx| {
            let report = cx.background_spawn(async move { stepan_verify_core::skills::scan(&home) }).await;
            let _ = this.update(cx, |this, cx| {
                this.rows = rows(&report);
                this.report = Some(Arc::new(report));
                this.loading = false;
                this.scanned = Some(Instant::now());
                cx.notify();
            });
        });
        cx.notify();
    }

    fn visible(&self, cx: &App) -> Vec<&Row> {
        let q = self.search.read(cx).value().to_lowercase();
        let mut out: Vec<&Row> = self
            .rows
            .iter()
            .filter(|r| match self.tab {
                Tab::Called => r.total14() > 0,
                Tab::All | Tab::Problems => true,
                Tab::Idle => r.total14() == 0 && !r.skill.plugin,
            })
            .filter(|r| match self.agent {
                AgentFilter::All => true,
                AgentFilter::Claude => r.skill.claude || r.claude14 > 0,
                AgentFilter::Codex => r.skill.codex || r.codex14 > 0,
            })
            .filter(|r| q.is_empty() || r.skill.name.to_lowercase().contains(&q) || r.skill.description.to_lowercase().contains(&q))
            .collect();
        if self.tab == Tab::Idle {
            out.sort_by(|a, b| a.skill.name.cmp(&b.skill.name));
        }
        out
    }

    fn toggle(&mut self, name: &str) {
        if !self.expanded.remove(name) {
            self.expanded.insert(name.to_owned());
        }
    }

    fn on_key(&mut self, e: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let visible: Vec<String> = self.visible(cx).iter().map(|r| r.skill.name.clone()).collect();
        if visible.is_empty() {
            return;
        }
        match e.keystroke.key.as_str() {
            "j" | "down" => self.selected = (self.selected + 1).min(visible.len() - 1),
            "k" | "up" => self.selected = self.selected.saturating_sub(1),
            "enter" | "space" => {
                if let Some(name) = visible.get(self.selected) {
                    self.toggle(name);
                }
            }
            "escape" => self.expanded.clear(),
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }
}

/// Table rows, most used first.
fn rows(report: &Report) -> Vec<Row> {
    let now = Utc::now();
    let since = now - ChronoDuration::days(WINDOW_DAYS);
    let today = now.date_naive();
    let mut out: Vec<Row> = report
        .skills
        .iter()
        .map(|s| {
            let uses: Vec<&Use> = report.uses_of(&s.name).collect();
            let mut days = vec![(0usize, 0usize); WINDOW_DAYS as usize];
            let (mut c, mut x) = (0, 0);
            for u in uses.iter().filter(|u| u.at > since) {
                let back = (today - u.at.date_naive()).num_days();
                let slot = (WINDOW_DAYS - 1 - back).clamp(0, WINDOW_DAYS - 1) as usize;
                match u.agent {
                    Provider::Claude => {
                        c += 1;
                        days[slot].0 += 1;
                    }
                    Provider::Codex => {
                        x += 1;
                        days[slot].1 += 1;
                    }
                }
            }
            let sessions: HashSet<&str> = uses.iter().map(|u| u.session.as_str()).collect();
            Row {
                skill: s.clone(),
                claude14: c,
                codex14: x,
                days,
                last: uses.first().map(|u| (*u).clone()),
                sessions: sessions.len(),
                problems: report.problems.iter().filter(|p| p.skill == s.name).count(),
            }
        })
        .collect();
    out.sort_by(|a, b| b.total14().cmp(&a.total14()).then(b.last.as_ref().map(|u| u.at).cmp(&a.last.as_ref().map(|u| u.at))).then(a.skill.name.cmp(&b.skill.name)));
    out
}

const MONTHS: [&str; 12] = ["января", "февраля", "марта", "апреля", "мая", "июня", "июля", "августа", "сентября", "октября", "ноября", "декабря"];

fn range_line() -> String {
    let now = Utc::now();
    let from = now - ChronoDuration::days(WINDOW_DAYS);
    if from.month() == now.month() {
        format!("по журналу сессий · {}–{} {}", from.day(), now.day(), MONTHS[now.month0() as usize])
    } else {
        format!("по журналу сессий · {} {} – {} {}", from.day(), MONTHS[from.month0() as usize], now.day(), MONTHS[now.month0() as usize])
    }
}

fn calls_word(n: usize) -> &'static str {
    match (n % 10, n % 100) {
        (1, r) if r != 11 => "вызов",
        (2..=4, r) if !(12..=14).contains(&r) => "вызова",
        _ => "вызовов",
    }
}

fn sessions_word(n: usize) -> &'static str {
    match (n % 10, n % 100) {
        (1, r) if r != 11 => "сессия",
        (2..=4, r) if !(12..=14).contains(&r) => "сессии",
        _ => "сессий",
    }
}

// ------------------------------------------------------------------ render

impl Render for SkillsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let p = Palette::of(&theme);
        let report = self.report.clone();
        let visible_names: Vec<String> = self.visible(cx).iter().map(|r| r.skill.name.clone()).collect();
        self.selected = self.selected.min(visible_names.len().saturating_sub(1));

        let since = Utc::now() - ChronoDuration::days(WINDOW_DAYS);
        let (calls, top, idle, problems) = match &report {
            Some(r) => {
                let calls = r.uses.iter().filter(|u| u.at > since).count();
                let top: Vec<String> = self.rows.iter().filter(|r| r.total14() > 0).take(3).map(|r| r.skill.name.clone()).collect();
                let idle = self.rows.iter().filter(|r| r.total14() == 0 && !r.skill.plugin).count();
                (calls, top, idle, r.problems.len())
            }
            None => (0, Vec::new(), 0, 0),
        };
        let called = self.rows.iter().filter(|r| r.total14() > 0).count();
        let all = self.rows.len();

        let header = h_flex()
            .items_end()
            .gap_3()
            .mb(px(18.))
            .child(
                v_flex()
                    .flex_1()
                    .gap(px(6.))
                    .child(ui::eyebrow(range_line(), &p, &theme))
                    .child(ui::page_title("Скиллы", &p))
                    .child(
                        h_flex()
                            .flex_wrap()
                            .gap(px(14.))
                            .text_size(px(13.5))
                            .text_color(p.muted)
                            .when(report.is_none(), |d| d.child(Skeleton::new().w(px(320.)).h(px(14.)).rounded(px(4.))))
                            .when(report.is_some(), |d| {
                                d.child(h_flex().gap(px(5.)).child(calls_word(calls)).child(div().font_weight(FontWeight::BOLD).text_color(p.fg).child(calls.to_string())))
                                    .when(!top.is_empty(), |d| {
                                        d.child(
                                            h_flex()
                                                .gap(px(5.))
                                                .child("чаще всего:")
                                                .child(ui::mono(top.join(", "), &theme).text_size(px(12.5)).font_weight(FontWeight::SEMIBOLD).text_color(p.fg)),
                                        )
                                    })
                                    .child(h_flex().gap(px(5.)).child(div().font_weight(FontWeight::BOLD).text_color(p.fg).child(idle.to_string())).child("без дела"))
                            }),
                    ),
            )
            .child(
                PressButton::new("skills-refresh")
                    .small()
                    .icon(IconName::RefreshCw)
                    .loading(self.loading)
                    .label(if self.loading { "Читаю журналы…" } else { "Обновить" })
                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
            );

        let tab = |id: &'static str, label: &'static str, count: usize, value: Tab, hot: bool, cx: &mut Context<Self>| {
            let active = self.tab == value;
            h_flex()
                .id(id)
                .h(px(28.))
                .px(px(10.))
                .gap(px(6.))
                .rounded(px(6.))
                .border_1()
                .border_color(transparent_black())
                .cursor_pointer()
                .text_size(px(13.))
                .text_color(if active { p.ink } else { p.muted })
                .when(active, |d| d.bg(p.surface).border_color(p.border).shadow(ui::edge_shadow(p.border_strong, 1.)))
                .when(!active, |d| d.hover(|s| s.bg(p.ink.opacity(0.04))))
                .child(label)
                .child(ui::mono(count.to_string(), &theme).text_size(px(11.)).text_color(if hot && count > 0 { p.red.text } else { p.faint }))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.tab = value;
                    this.selected = 0;
                    cx.notify();
                }))
        };
        let agent_chip = |id: &'static str, label: &'static str, value: AgentFilter, cx: &mut Context<Self>| {
            let active = self.agent == value;
            div()
                .id(id)
                .px(px(9.))
                .h(px(26.))
                .flex()
                .items_center()
                .rounded_full()
                .border_1()
                .border_color(if active { p.ink } else { p.border })
                .bg(if active { p.ink } else { p.surface })
                .text_size(px(12.5))
                .text_color(if active { p.surface } else { p.muted })
                .cursor_pointer()
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.agent = value;
                    this.selected = 0;
                    cx.notify();
                }))
        };
        let toolbar = h_flex()
            .gap(px(10.))
            .mb(px(12.))
            .child(
                h_flex()
                    .p(px(3.))
                    .gap(px(2.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(p.border)
                    .bg(p.sunken)
                    .child(tab("tab-called", "Вызывались", called, Tab::Called, false, cx))
                    .child(tab("tab-all", "Все", all, Tab::All, false, cx))
                    .child(tab("tab-idle", "Без дела 2 нед", idle, Tab::Idle, false, cx))
                    .child(tab("tab-problems", "Проблемы", problems, Tab::Problems, true, cx)),
            )
            .child(
                h_flex()
                    .gap(px(5.))
                    .child(agent_chip("agent-all", "Все агенты", AgentFilter::All, cx))
                    .child(agent_chip("agent-claude", "Claude", AgentFilter::Claude, cx))
                    .child(agent_chip("agent-codex", "Codex", AgentFilter::Codex, cx)),
            )
            .child(div().flex_1())
            .child(div().w(px(240.)).child(Input::new(&self.search).small().prefix(Icon::new(IconName::Search).xsmall().text_color(p.faint))));

        let body: AnyElement = match (&report, self.tab) {
            (None, _) => skeleton(&p).into_any_element(),
            (Some(r), Tab::Problems) => self.render_problems(r, &p, &theme, cx).into_any_element(),
            (Some(r), _) => self.render_table(r, &visible_names, &p, &theme, cx).into_any_element(),
        };

        div()
            .id("skills")
            .size_full()
            .overflow_y_scroll()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(1240.))
                    .mx_auto()
                    .px(px(32.))
                    .pt(px(28.))
                    .pb(px(40.))
                    .child(header)
                    .child(toolbar)
                    .child(body),
            )
    }
}

const COL_CHECK: f32 = 64.;
const COL_USES: f32 = 150.;
const COL_LAST: f32 = 300.;
const COL_RESULT: f32 = 92.;

impl SkillsView {
    fn render_table(&self, report: &Arc<Report>, names: &[String], p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let head = |text: &'static str, w: Option<f32>| {
            ui::eyebrow(text, p, theme).text_color(p.faint).map(|d| match w {
                Some(w) => d.w(px(w)).flex_shrink_0(),
                None => d.flex_1().min_w_0(),
            })
        };
        let rows: Vec<AnyElement> = names
            .iter()
            .enumerate()
            .filter_map(|(i, name)| self.rows.iter().find(|r| &r.skill.name == name).map(|r| (i, r)))
            .map(|(i, row)| self.render_row(i, row, report, p, theme, cx).into_any_element())
            .collect();
        let empty = rows.is_empty();
        ui::card(p)
            .overflow_hidden()
            .child(
                h_flex()
                    .px(px(14.))
                    .h(px(36.))
                    .gap(px(10.))
                    .border_b_1()
                    .border_color(p.border)
                    .bg(p.surface2)
                    .child(div().w(px(16.)))
                    .child(head("скилл", None))
                    .child(head("claude", Some(COL_CHECK)))
                    .child(head("codex", Some(COL_CHECK)))
                    .child(head("stepan_verify", Some(48.)))
                    .child(head("вызовы · 14 дн", Some(COL_USES)))
                    .child(head("последний вызов", Some(COL_LAST)))
                    .child(head("сессий · 30 дн", Some(COL_RESULT))),
            )
            .children(rows)
            .when(empty, |d| {
                d.child(div().px(px(14.)).py(px(28.)).text_size(px(13.)).text_color(p.muted).child(match self.tab {
                    Tab::Called => "За две недели агенты не вызывали скиллы под этот фильтр",
                    Tab::Idle => "Все скиллы под этот фильтр использовались",
                    _ => "Ничего не найдено",
                }))
            })
    }

    fn render_row(&self, i: usize, row: &Row, report: &Arc<Report>, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let s = &row.skill;
        let name = s.name.clone();
        let open = self.expanded.contains(&s.name);
        let selected = i == self.selected;
        let mono = theme.mono_font_family.clone();
        let place = if s.plugin {
            "плагин · без папки на диске".to_owned()
        } else {
            s.locations.first().map(|l| l.root.label()).unwrap_or_default()
        };
        let last_session = row.last.as_ref().and_then(|u| report.sessions.get(&u.session).cloned());
        let toggle_name = name.clone();

        let line = h_flex()
            .id(SharedString::from(format!("skill-row:{name}")))
            .relative()
            .px(px(14.))
            .min_h(px(50.))
            .gap(px(10.))
            .when(i > 0, |d| d.border_t_1().border_color(ui::mix(p.border, p.surface, 0.6)))
            .when(selected, |d| d.child(div().absolute().left_0().top_0().bottom_0().w(px(2.)).bg(p.accent.solid)))
            .when(open || selected, |d| d.bg(p.surface2))
            .cursor_pointer()
            .hover(|d| d.bg(p.surface2))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.toggle(&toggle_name);
                this.selected = i;
                window.focus(&this.focus, cx);
                cx.notify();
            }))
            .child(Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight }).xsmall().text_color(p.faint))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.))
                    .child(
                        h_flex()
                            .gap(px(6.))
                            .child(div().truncate().font_family(mono.clone()).text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).child(name.clone()))
                            .when(s.manual_only, |d| d.child(ui::chip("вручную", p, theme)))
                            .when(row.problems > 0, |d| d.child(Icon::new(IconName::TriangleAlert).xsmall().text_color(p.red.solid))),
                    )
                    .child(div().truncate().font_family(mono.clone()).text_size(px(11.)).text_color(p.faint).child(place)),
            )
            .child(div().w(px(COL_CHECK)).flex_shrink_0().child(sees(s.claude, Seer::Claude, p)))
            .child(div().w(px(COL_CHECK)).flex_shrink_0().child(sees(s.codex, Seer::Codex, p)))
            .child(div().w(px(48.)).flex_shrink_0().child(sees(s.stepan_verify, Seer::StepanVerify, p)))
            .child(
                h_flex()
                    .w(px(COL_USES))
                    .flex_shrink_0()
                    .gap(px(8.))
                    .child(sparkline(&row.days, p))
                    .child(div().font_family(mono.clone()).text_size(px(12.5)).font_weight(FontWeight::SEMIBOLD).text_color(if row.total14() > 0 { p.ink } else { p.faint }).child(row.total14().to_string())),
            )
            .child(
                h_flex()
                    .w(px(COL_LAST))
                    .flex_shrink_0()
                    .min_w_0()
                    .gap(px(7.))
                    .map(|d| match &row.last {
                        Some(u) => d
                            .child(ui::provider_chip(u.agent, p, theme))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(12.5))
                                    .text_color(p.fg)
                                    .child(last_session.as_ref().map(|s| s.title.clone()).unwrap_or_else(|| format!("сессия {}", short_id(&u.session)))),
                            )
                            .child(div().flex_shrink_0().font_family(mono.clone()).text_size(px(11.)).text_color(p.faint).child(ago(u.at))),
                        None => d.child(div().text_size(px(12.5)).text_color(p.faint).child("—")),
                    }),
            )
            .child(
                div()
                    .w(px(COL_RESULT))
                    .flex_shrink_0()
                    .font_family(mono.clone())
                    .text_size(px(11.5))
                    .text_color(p.muted)
                    .child(if row.sessions > 0 { format!("{} {}", row.sessions, sessions_word(row.sessions)) } else { "—".into() }),
            );

        v_flex().child(line).when(open, |d| d.child(self.render_details(row, report, p, theme, cx)))
    }

    fn render_details(&self, row: &Row, report: &Arc<Report>, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let s = &row.skill;
        let mono = theme.mono_font_family.clone();
        let uses: Vec<&Use> = report.uses_of(&s.name).take(HISTORY_ROWS).collect();
        let more = report.uses_of(&s.name).count().saturating_sub(HISTORY_ROWS);
        let problems: Vec<&Problem> = report.problems.iter().filter(|pr| pr.skill == s.name).collect();

        let history = v_flex()
            .flex_1()
            .min_w_0()
            .gap(px(2.))
            .child(ui::eyebrow("история вызовов", p, theme).mb(px(6.)))
            .when(uses.is_empty(), |d| d.child(div().text_size(px(12.5)).text_color(p.faint).child("В журналах последних 30 дней вызовов нет")))
            .children(uses.into_iter().enumerate().map(|(i, u)| {
                let summary = report.sessions.get(&u.session).cloned();
                let title = summary.as_ref().map(|s| s.title.clone()).unwrap_or_else(|| format!("сессия {}", short_id(&u.session)));
                let project = summary.as_ref().map(|s| s.project());
                h_flex()
                    .id(SharedString::from(format!("use:{}:{i}", s.name)))
                    .h(px(30.))
                    .px(px(6.))
                    .gap(px(8.))
                    .rounded(px(5.))
                    .when(i > 0, |d| d.border_t_1().border_color(ui::mix(p.border, p.surface, 0.5)))
                    .when(summary.is_some(), |d| d.cursor_pointer().hover(|d| d.bg(p.surface)))
                    .child(div().w(px(84.)).flex_shrink_0().font_family(mono.clone()).text_size(px(11.)).text_color(p.faint).child(ago(u.at)))
                    .child(ui::provider_chip(u.agent, p, theme))
                    .child(div().flex_1().min_w_0().truncate().text_size(px(12.5)).text_color(p.fg).child(title))
                    .when_some(project, |d, pr| d.child(ui::chip(pr, p, theme)))
                    .child(ui::badge(
                        match u.method {
                            Method::Auto => "авто",
                            Method::Command => "команда",
                        },
                        match u.method {
                            Method::Auto => p.blue,
                            Method::Command => p.amber,
                        },
                    ))
                    .when_some(summary, |d, summary| {
                        d.on_click(cx.listener(move |_, _, _, cx| cx.emit(SkillsEvent::OpenSession(summary.clone()))))
                    })
            }))
            .when(more > 0, |d| d.child(div().pt(px(6.)).font_family(mono.clone()).text_size(px(11.)).text_color(p.faint).child(format!("ещё {more} {}", calls_word(more)))));

        let skill_md = s.skill_md();
        let folder = s.folder();
        let side = v_flex()
            .w(px(380.))
            .flex_shrink_0()
            .gap(px(8.))
            .child(ui::eyebrow("где лежит", p, theme))
            .when(s.locations.is_empty(), |d| d.child(div().text_size(px(12.5)).text_color(p.muted).child("Скилл плагина: вызывается Claude, своей папки в skills нет")))
            .children(s.locations.iter().map(|l| {
                let target = match (&l.link, &l.real) {
                    (true, Some(real)) => format!(" → {}", tilde(real)),
                    (true, None) => " → (битая ссылка)".into(),
                    _ => String::new(),
                };
                v_flex()
                    .child(div().font_family(mono.clone()).text_size(px(11.5)).text_color(p.fg).child(format!("{}{}", tilde(&l.path), target)))
                    .child(div().text_size(px(11.)).text_color(p.faint).child(l.root.label()))
            }))
            .when(!s.description.is_empty(), |d| {
                d.child(ui::eyebrow("описание", p, theme).mt(px(6.)))
                    .child(div().text_size(px(12.5)).line_height(px(18.)).text_color(p.muted).child(s.description.clone()))
            })
            .children(problems.into_iter().map(|pr| {
                div().mt(px(4.)).text_size(px(12.)).text_color(p.red.text).child(format!("⚠ {} — {}", pr.title, pr.detail))
            }))
            .child(
                h_flex()
                    .mt(px(6.))
                    .gap(px(6.))
                    .when_some(skill_md, |d, path| {
                        d.child(
                            PressButton::new(SharedString::from(format!("skill-md:{}", s.name)))
                                .xsmall()
                                .icon(IconName::FileText)
                                .label("SKILL.md")
                                .on_click(cx.listener(move |_, _, _, cx| cx.emit(SkillsEvent::OpenFile(path.clone())))),
                        )
                    })
                    .when_some(folder, |d, path| {
                        d.child(
                            PressButton::new(SharedString::from(format!("skill-dir:{}", s.name)))
                                .xsmall()
                                .icon(IconName::Folder)
                                .label("Папка")
                                .on_click(cx.listener(move |_, _, _, cx| cx.emit(SkillsEvent::OpenFile(path.clone())))),
                        )
                    }),
            );

        h_flex()
            .items_start()
            .gap(px(28.))
            .px(px(40.))
            .pt(px(10.))
            .pb(px(16.))
            .bg(p.surface2)
            .border_t_1()
            .border_dashed()
            .border_color(p.border)
            .child(history)
            .child(side)
    }

    fn render_problems(&self, report: &Arc<Report>, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let q = self.search.read(cx).value().to_lowercase();
        let list: Vec<&Problem> = report
            .problems
            .iter()
            .filter(|pr| q.is_empty() || pr.skill.to_lowercase().contains(&q) || pr.title.to_lowercase().contains(&q))
            .collect();
        let mono = theme.mono_font_family.clone();
        v_flex()
            .gap(px(10.))
            .when(list.is_empty(), |d| {
                d.child(ui::card(p).px(px(16.)).py(px(20.)).child(div().text_size(px(13.)).text_color(p.muted).child("Проблем не найдено: все скиллы на месте и видны агентам")))
            })
            .children(list.into_iter().enumerate().map(|(i, pr)| {
                let (label, tone): (&str, Tone) = match pr.severity {
                    Severity::High => ("высокая", p.red),
                    Severity::Medium => ("средняя", p.amber),
                    Severity::Low => ("низкая", Tone { solid: p.faint, soft: p.surface2, text: p.muted }),
                };
                let path = pr.path.clone();
                ui::card(p)
                    .id(SharedString::from(format!("problem:{i}")))
                    .px(px(16.))
                    .py(px(12.))
                    .gap(px(6.))
                    .child(
                        h_flex()
                            .gap(px(8.))
                            .child(ui::badge(label, tone))
                            .child(div().font_family(mono.clone()).text_size(px(12.5)).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).child(pr.skill.clone()))
                            .child(div().flex_1().min_w_0().truncate().text_size(px(13.5)).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).child(pr.title.clone()))
                            .when_some(path, |d, path| {
                                d.child(
                                    PressButton::new(SharedString::from(format!("problem-open:{i}")))
                                        .xsmall()
                                        .icon(IconName::ExternalLink)
                                        .label("Открыть")
                                        .on_click(cx.listener(move |_, _, _, cx| cx.emit(SkillsEvent::OpenFile(path.clone())))),
                                )
                            }),
                    )
                    .child(div().text_size(px(12.5)).text_color(p.muted).child(pr.detail.clone()))
                    .child(
                        h_flex()
                            .gap(px(6.))
                            .items_start()
                            .child(div().flex_shrink_0().text_size(px(12.)).text_color(p.faint).child("Как исправить:"))
                            .child(div().min_w_0().font_family(mono.clone()).text_size(px(11.5)).text_color(p.fg).child(pr.fix.clone())),
                    )
            }))
    }
}

#[derive(Clone, Copy)]
enum Seer {
    Claude,
    Codex,
    StepanVerify,
}

/// Checkbox-like mark: the agent sees the skill, or a dash.
fn sees(yes: bool, who: Seer, p: &Palette) -> AnyElement {
    if !yes {
        return div().pl(px(6.)).text_size(px(13.)).text_color(p.faint).child("—").into_any_element();
    }
    let (bg, border, fg) = match who {
        Seer::Claude => (p.claude.0, p.claude.2, p.claude.1),
        Seer::Codex => (p.codex.0, p.codex.2, p.codex.1),
        Seer::StepanVerify => (p.accent.soft, p.accent.solid, p.accent.text),
    };
    div()
        .size(px(20.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(5.))
        .border_1()
        .border_color(border)
        .bg(bg)
        .child(Icon::new(IconName::Check).xsmall().text_color(fg))
        .into_any_element()
}

/// 14 daily bars: Codex at the bottom (ink), Claude on top (orange).
fn sparkline(days: &[(usize, usize)], p: &Palette) -> impl IntoElement {
    let max = days.iter().map(|(c, x)| c + x).max().unwrap_or(0).max(1) as f32;
    h_flex().items_end().gap(px(1.5)).h(px(18.)).children(days.iter().map(|&(c, x)| {
        let total = c + x;
        let h = if total == 0 { 2. } else { 4. + 14. * total as f32 / max };
        let claude_h = if total == 0 { 0. } else { h * c as f32 / total as f32 };
        v_flex()
            .w(px(3.))
            .h(px(h))
            .rounded(px(1.))
            .overflow_hidden()
            .bg(if total == 0 { p.border } else { p.codex.0 })
            .child(div().w_full().h(px(claude_h)).bg(p.accent.solid))
    }))
}

fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

fn skeleton(p: &Palette) -> impl IntoElement {
    const WIDTHS: [f32; 6] = [0.5, 0.36, 0.62, 0.44, 0.3, 0.55];
    ui::card(p).overflow_hidden().children((0..8).map(|i| {
        h_flex()
            .px(px(14.))
            .h(px(50.))
            .gap(px(10.))
            .when(i > 0, |d| d.border_t_1().border_color(p.border))
            .child(
                v_flex()
                    .flex_1()
                    .gap(px(6.))
                    .child(Skeleton::new().w(relative(WIDTHS[i % 6])).h(px(12.)).rounded(px(4.)))
                    .child(Skeleton::new().secondary().w(px(140.)).h(px(9.)).rounded(px(4.))),
            )
            .child(Skeleton::new().secondary().size(px(20.)).rounded(px(5.)))
            .child(Skeleton::new().secondary().size(px(20.)).rounded(px(5.)))
            .child(Skeleton::new().w(px(110.)).h(px(14.)).rounded(px(3.)))
            .child(Skeleton::new().secondary().w(px(240.)).h(px(12.)).rounded(px(4.)))
    }))
}
