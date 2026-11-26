//! «Чем пользуюсь»: which features of Stepan you and your agents touched over
//! the last 30 days, and which nobody did. Counted locally (`core::usage`);
//! nothing leaves the machine.

use gpui_kit::component::{ActiveTheme as _, Theme, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use stepan_verify_core::{
    Home,
    usage::{self, DAYS, Row, Source},
};

use crate::{
    format::ago,
    ui::{self, Palette},
    zoom::px,
};

/// No use for this long reads as «не пользуюсь».
const IDLE_DAYS: i64 = 14;
/// Releases whose features still count as new.
const NEW_SINCE: [&str; 5] = ["0.2.8", "0.2.9", "0.2.10", "0.2.11", "0.2.12"];

#[derive(Clone, Copy, PartialEq)]
enum Filter {
    All,
    Idle,
}

pub struct UsageView {
    home: Home,
    rows: Vec<Row>,
    now: i64,
    /// When click counting began (`usage::live_since`).
    live_since: Option<i64>,
    loaded: bool,
    filter: Filter,
    _load: Task<()>,
}

impl UsageView {
    pub fn new(home: Home) -> Self {
        Self { home, rows: Vec::new(), now: 0, live_since: None, loaded: false, filter: Filter::All, _load: Task::ready(()) }
    }

    /// Recount from disk and the store: on every visit to the page.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let home = self.home.clone();
        let store = crate::workspace::store(cx);
        self._load = cx.spawn(async move |this, cx| {
            let (rows, now, live_since) = cx
                .background_spawn(async move {
                    let now = chrono::Utc::now().timestamp();
                    (usage::report(&usage::collect(&home, store.as_ref()), now), now, store.as_ref().and_then(usage::live_since))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.rows = rows;
                this.now = now;
                this.live_since = live_since;
                this.loaded = true;
                cx.notify();
            });
        });
    }

    fn idle(&self, r: &Row) -> bool {
        r.idle(self.now, IDLE_DAYS, self.live_since)
    }

    fn render_strip(&self, p: &Palette, theme: &Theme) -> impl IntoElement {
        let total = self.rows.len();
        let used = self.rows.iter().filter(|r| r.total30() > 0).count();
        let idle = self.rows.iter().filter(|r| self.idle(r)).count();
        let me: u32 = self.rows.iter().map(|r| r.me[0]).sum();
        let agents: u32 = self.rows.iter().map(|r| r.agent[0]).sum();
        let cell = |label: &'static str, value: String, note: &'static str, last: bool| {
            v_flex()
                .flex_1()
                .gap(px(4.))
                .px(px(18.))
                .py(px(14.))
                .when(!last, |d| d.border_r_1().border_color(p.border))
                .child(ui::eyebrow(label, p, theme))
                .child(div().text_size(px(22.)).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).child(value))
                .child(div().text_size(px(12.)).text_color(p.faint).child(note))
        };
        ui::card(p)
            .flex_row()
            .child(cell("в ходу", format!("{used} из {total}"), "фич трогали за 30 дней", false))
            .child(cell("не пользуюсь", idle.to_string(), "нет действий 14+ дней", false))
            .child(cell("твои действия", me.to_string(), "за 7 дней", false))
            .child(cell("агенты и Stepan сам", agents.to_string(), "за 7 дней", true))
    }

    fn render_group(&self, group: &'static str, p: &Palette, theme: &Theme) -> Option<AnyElement> {
        let all: Vec<&Row> = self.rows.iter().filter(|r| r.feature.group == group).collect();
        let shown: Vec<&Row> = all.iter().copied().filter(|r| self.filter == Filter::All || self.idle(r)).collect();
        if shown.is_empty() {
            return None;
        }
        let used = all.iter().filter(|r| r.total30() > 0).count();
        let head = |text: &'static str, w: f32| ui::mono(text, theme).w(px(w)).flex_none().text_right().text_color(p.faint);
        Some(
            v_flex()
                .gap(px(10.))
                .child(ui::section_title(group, Some(format!("{used}/{}", all.len())), p, theme))
                .child(
                    ui::card(p)
                        .overflow_hidden()
                        .child(
                            h_flex()
                                .px(px(16.))
                                .py(px(7.))
                                .gap(px(16.))
                                .bg(p.sunken)
                                .border_b_1()
                                .border_color(p.border)
                                .child(ui::mono("фича", theme).flex_1().text_color(p.faint))
                                .child(ui::mono("30 дней", theme).w(px(DAYS as f32 * 5.)).flex_none().text_color(p.faint))
                                .child(head("ты 7д · 30д", 92.))
                                .child(head("агенты 7д · 30д", 112.))
                                .child(head("последний раз", 104.)),
                        )
                        .children(shown.into_iter().enumerate().map(|(i, r)| self.render_row(i, r, p, theme))),
                )
                .into_any_element(),
        )
    }

    fn render_row(&self, i: usize, r: &Row, p: &Palette, theme: &Theme) -> impl IntoElement {
        let idle = self.idle(r);
        let fresh_and_untried = r.last.is_none() && NEW_SINCE.contains(&r.feature.since);
        let last = match r.last {
            Some(t) => chrono::DateTime::from_timestamp(t, 0).map_or_else(String::new, |t| format!("{} назад", ago(t))),
            // A click-counted feature had no chance before 0.2.10.
            None if r.feature.source == Source::Live => "пока не было".into(),
            None => "никогда".into(),
        };
        let pair = |n: [u32; 2], w: f32| {
            ui::mono(if n == [0, 0] { "—".to_owned() } else { format!("{} · {}", n[0], n[1]) }, theme)
                .w(px(w))
                .flex_none()
                .text_right()
                .text_color(if n[1] > 0 { p.fg } else { p.faint })
        };
        h_flex()
            .px(px(16.))
            .py(px(10.))
            .gap(px(16.))
            .items_center()
            .when(i > 0, |d| d.border_t_1().border_color(p.border))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.))
                    .child(
                        h_flex()
                            .gap(px(8.))
                            .items_center()
                            .child(div().text_size(px(13.5)).font_weight(FontWeight::MEDIUM).text_color(if idle { p.muted } else { p.ink }).child(r.feature.name))
                            .child(ui::mono(format!("с {}", r.feature.since), theme).text_color(p.faint))
                            .when(fresh_and_untried, |d| d.child(ui::badge("новое, не пробовал", p.amber))),
                    )
                    .child(div().text_size(px(12.)).text_color(p.muted).truncate().child(r.feature.hint)),
            )
            .child(spark(&r.days, p))
            .child(pair(r.me, 92.))
            .child(pair(r.agent, 112.))
            .child(ui::mono(last, theme).w(px(104.)).flex_none().text_right().text_color(match (idle, r.last) {
                (true, _) => p.amber.text,
                (false, None) => p.faint,
                (false, Some(_)) => p.muted,
            }))
    }
}

/// 30 bars, one per day, scaled to the row's busiest day.
fn spark(days: &[u32; DAYS], p: &Palette) -> impl IntoElement {
    let max = days.iter().copied().max().unwrap_or(0).max(1) as f32;
    h_flex()
        .w(px(DAYS as f32 * 5.))
        .h(px(20.))
        .flex_none()
        .items_end()
        .gap(px(1.))
        .children(days.iter().map(|&n| {
            let h = if n == 0 { 1. } else { 4. + 16. * n as f32 / max };
            div().w(px(4.)).h(px(h)).rounded(px(1.)).bg(if n == 0 { p.border } else { p.blue.solid })
        }))
}

impl Render for UsageView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let p = Palette::of(&theme);
        let idle = self.rows.iter().filter(|r| self.idle(r)).count();
        let header = h_flex().items_end().gap_3().child(
            v_flex()
                .flex_1()
                .gap(px(6.))
                .child(ui::eyebrow("локально · ничего не уходит с компьютера", &p, &theme))
                .child(ui::page_title("Чем пользуюсь", &p))
                .child(div().max_w(px(760.)).text_size(px(13.5)).text_color(p.muted).child(
                    "Какие фичи Stepan трогали ты и агенты. Задачи, документы, память, «Изменения» и скиллы посчитаны по файлам Stepan за всю историю. Страницы, вкладки и кнопки без следа на диске считаются с 0.2.10.",
                )),
        );
        let filter = ui::segmented(&p)
            .child(
                ui::segment("usage-all", "Все фичи", self.filter == Filter::All, &p)
                    .px(px(12.))
                    .h(px(28.))
                    .text_size(px(12.5))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.filter = Filter::All;
                        cx.notify();
                    })),
            )
            .child(
                ui::segment("usage-idle", "Не пользуюсь", self.filter == Filter::Idle, &p)
                    .px(px(12.))
                    .h(px(28.))
                    .text_size(px(12.5))
                    .child(ui::mono(idle.to_string(), &theme).text_color(p.faint))
                    .gap(px(6.))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.filter = Filter::Idle;
                        cx.notify();
                    })),
            );
        let body = if self.loaded {
            v_flex()
                .gap(px(24.))
                .child(self.render_strip(&p, &theme))
                .child(h_flex().child(filter))
                .children(usage::GROUPS.iter().filter_map(|g| self.render_group(g, &p, &theme)))
                .into_any_element()
        } else {
            div().text_size(px(13.5)).text_color(p.muted).child("Считаю…").into_any_element()
        };
        div().id("usage").size_full().overflow_y_scroll().child(
            v_flex()
                .w_full()
                .max_w(px(1240.))
                .mx_auto()
                .px(px(32.))
                .pt(px(28.))
                .pb(px(48.))
                .gap(px(20.))
                .child(header)
                .child(body),
        )
    }
}
