//! Память: what agents remember about you and each project — your corrections,
//! don'ts, preferences and project facts. Agents, the session distiller and you
//! write it; every Claude and Codex session gets it at start. The page shows
//! the entries by scope and kind, lets you fix or drop any of them, and the log.

use std::{collections::HashMap, time::Duration, time::SystemTime};

use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, Theme, h_flex,
    input::{Input, InputEvent, InputState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use crate::zoom::px;
use stepan_verify_core::{
    Home,
    memory::{self, Change, Entry, Kind, Memory, NewEntry, Op},
};

use crate::{
    format::ago,
    ui::{self, Palette, PressButton},
};

/// Shown in the log at most.
const LOG_ROWS: usize = 200;

#[derive(Clone, PartialEq, Eq)]
enum Sel {
    Shared,
    Project(String),
    Log,
}

pub struct MemoryView {
    home: Home,
    memory: Memory,
    stamp: Option<SystemTime>,
    sel: Sel,
    search: Entity<InputState>,
    adding: bool,
    draft: Entity<InputState>,
    draft_kind: Kind,
    editing: Option<String>,
    edit_input: Entity<InputState>,
    edit_kind: Kind,
    error: Option<String>,
    _watch: Task<()>,
    _subs: Vec<Subscription>,
}

impl MemoryView {
    /// `project`: open on that project's entries (`--page memory --project <name>`).
    pub fn new(home: Home, project: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Найти в памяти"));
        let draft = cx.new(|cx| InputState::new(window, cx).placeholder("Одна мысль, повелительно: «Не делать force push в main»"));
        let edit_input = cx.new(|cx| InputState::new(window, cx));
        let subs = vec![
            cx.subscribe(&search, |_, _, _: &InputEvent, cx| cx.notify()),
            cx.subscribe_in(&draft, window, |this, _, e: &InputEvent, window, cx| {
                if matches!(e, InputEvent::PressEnter { .. }) {
                    this.save_draft(window, cx);
                }
            }),
            cx.subscribe_in(&edit_input, window, |this, _, e: &InputEvent, window, cx| {
                if matches!(e, InputEvent::PressEnter { .. }) {
                    this.save_edit(window, cx);
                }
            }),
        ];
        let mut this = Self {
            home,
            memory: Memory::default(),
            stamp: None,
            sel: project.map_or(Sel::Shared, Sel::Project),
            search,
            adding: false,
            draft,
            draft_kind: Kind::Rule,
            editing: None,
            edit_input,
            edit_kind: Kind::Rule,
            error: None,
            _watch: Task::ready(()),
            _subs: subs,
        };
        this.reload(cx);
        // Agents write the log from their own processes: pick their entries up.
        this._watch = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(1500)).await;
                let changed = this.update(cx, |this, _| this.stamp != log_stamp(&this.home)).unwrap_or(false);
                if changed && this.update(cx, |this, cx| this.reload(cx)).is_err() {
                    break;
                }
            }
        });
        this
    }

    /// Live entries, for the sidebar counter.
    pub fn count(&self) -> usize {
        self.memory.entries.len()
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        self.stamp = log_stamp(&self.home);
        self.memory = Memory::load(&self.home);
        if let Sel::Project(p) = &self.sel {
            if !self.memory.projects().contains(p) {
                self.sel = Sel::Shared;
            }
        }
        cx.notify();
    }

    fn scope_project(&self) -> Option<String> {
        match &self.sel {
            Sel::Project(p) => Some(p.clone()),
            _ => None,
        }
    }

    fn save_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.draft.read(cx).value().trim().to_owned();
        if text.is_empty() {
            return;
        }
        let new = NewEntry { kind: Some(self.draft_kind), text, project: self.scope_project(), ..Default::default() };
        match memory::add(&self.home, new, "ты") {
            Ok(_) => {
                self.error = None;
                self.adding = false;
                self.draft.update(cx, |s, cx| s.set_value("", window, cx));
            }
            Err(err) => self.error = Some(format!("{err:#}")),
        }
        self.reload(cx);
    }

    fn start_edit(&mut self, e: &Entry, window: &mut Window, cx: &mut Context<Self>) {
        self.editing = Some(e.id.clone());
        self.edit_kind = e.kind;
        self.error = None;
        let text = e.text.clone();
        self.edit_input.update(cx, |s, cx| {
            s.set_value(text, window, cx);
            s.focus(window, cx);
        });
        cx.notify();
    }

    fn save_edit(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.editing.clone() else { return };
        let text = self.edit_input.read(cx).value().trim().to_owned();
        match memory::edit(&self.home, &id, Change { text: Some(text), kind: Some(self.edit_kind), project: None }, "ты") {
            Ok(_) => {
                self.editing = None;
                self.error = None;
            }
            Err(err) => self.error = Some(format!("{err:#}")),
        }
        self.reload(cx);
    }

    fn remove(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Err(err) = memory::remove(&self.home, id, "", "ты") {
            self.error = Some(format!("{err:#}"));
        }
        self.reload(cx);
    }

    fn make_shared(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Err(err) = memory::edit(&self.home, id, Change { project: Some(None), ..Default::default() }, "ты") {
            self.error = Some(format!("{err:#}"));
        }
        self.reload(cx);
    }

    // ------------------------------------------------------------------ parts

    fn render_nav(&self, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let shared = self.memory.entries.iter().filter(|e| e.project.is_none()).count();
        let row = |id: SharedString, label: SharedString, count: usize, sel: Sel, icon: Icon, cx: &mut Context<Self>| {
            let active = self.sel == sel;
            h_flex()
                .id(id)
                .h(px(32.))
                .px(px(10.))
                .gap(px(8.))
                .rounded(px(7.))
                .border_1()
                .border_color(transparent_black())
                .cursor_pointer()
                .text_size(px(13.5))
                .text_color(if active { p.ink } else { p.fg })
                .when(active, |d| d.bg(p.surface).border_color(p.border).shadow(ui::edge_shadow(p.border_strong, 1.)))
                .when(!active, |d| d.hover(|s| s.bg(p.ink.opacity(0.04))))
                .child(icon.xsmall().text_color(if active { p.ink } else { p.muted }))
                .child(div().flex_1().min_w_0().truncate().child(label))
                .child(ui::mono(count.to_string(), theme).text_color(p.faint))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.sel = sel.clone();
                    this.editing = None;
                    this.error = None;
                    cx.notify();
                }))
        };
        let projects = self.memory.projects();
        v_flex()
            .w(px(240.))
            .flex_none()
            .gap(px(2.))
            .child(row("mem-shared".into(), "Для всех проектов".into(), shared, Sel::Shared, Icon::empty().path("icons/globe.svg"), cx))
            .when(!projects.is_empty(), |this| this.child(div().pt(px(14.)).pb(px(4.)).px(px(10.)).child(ui::eyebrow("проекты", p, theme))))
            .children(projects.into_iter().map(|name| {
                let count = self.memory.entries.iter().filter(|e| e.project.as_deref() == Some(name.as_str())).count();
                row(format!("mem-p-{name}").into(), name.clone().into(), count, Sel::Project(name), Icon::new(IconName::Folder), cx)
            }))
            .child(div().my(px(12.)).h(px(1.)).bg(p.border))
            .child(row("mem-log".into(), "Журнал".into(), self.memory.log.len(), Sel::Log, Icon::empty().path("icons/scroll-text.svg"), cx))
    }

    fn kind_tabs(&self, id: &'static str, value: Kind, p: &Palette, cx: &mut Context<Self>, set: fn(&mut Self, Kind)) -> impl IntoElement {
        h_flex().gap(px(4.)).children(Kind::ALL.into_iter().map(|k| {
            let active = k == value;
            h_flex()
                .id(SharedString::from(format!("{id}-{}", k.key())))
                .h(px(26.))
                .px(px(9.))
                .rounded(px(6.))
                .border_1()
                .border_color(if active { p.border } else { transparent_black() })
                .cursor_pointer()
                .text_size(px(12.5))
                .text_color(if active { p.ink } else { p.muted })
                .when(active, |d| d.bg(p.surface).shadow(ui::edge_shadow(p.border_strong, 1.)))
                .when(!active, |d| d.hover(|s| s.bg(p.ink.opacity(0.04))))
                .child(k.title())
                .on_click(cx.listener(move |this, _, _, cx| {
                    set(this, k);
                    cx.notify();
                }))
        }))
    }

    fn render_add(&self, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let target = match &self.sel {
            Sel::Project(name) => format!("в проект {name}"),
            _ => "для всех проектов".to_owned(),
        };
        ui::card(p)
            .p(px(14.))
            .gap(px(10.))
            .child(Input::new(&self.draft))
            .child(
                h_flex()
                    .gap(px(10.))
                    .child(self.kind_tabs("draft-kind", self.draft_kind, p, cx, |this, k| this.draft_kind = k))
                    .child(div().flex_1().text_size(px(12.5)).text_color(p.muted).child(target))
                    .child(PressButton::new("mem-add-cancel").small().ghost().label("Отмена").on_click(cx.listener(|this, _, _, cx| {
                        this.adding = false;
                        this.error = None;
                        cx.notify();
                    })))
                    .child(PressButton::new("mem-add-save").small().primary().label("Запомнить").on_click(cx.listener(|this, _, window, cx| this.save_draft(window, cx)))),
            )
    }

    fn render_entry(&self, e: &Entry, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let editing = self.editing.as_deref() == Some(e.id.as_str());
        let id = e.id.clone();
        let (edit_entry, remove_id, share_id) = (e.clone(), id.clone(), id.clone());
        let meta = h_flex()
            .gap(px(8.))
            .items_center()
            .text_size(px(12.))
            .text_color(p.faint)
            .child(ui::chip(e.by.clone(), p, theme))
            .child(ago(e.updated))
            .when(e.updated != e.created, |d| d.child("· поправлено"))
            .child(ui::mono(e.id.clone(), theme).text_color(p.faint))
            .when(!e.why.is_empty(), |d| d.child(div().min_w_0().truncate().text_color(p.muted).child(format!("— {}", e.why))));
        let body = if editing {
            v_flex()
                .gap(px(8.))
                .child(Input::new(&self.edit_input))
                .child(
                    h_flex()
                        .gap(px(10.))
                        .child(self.kind_tabs("edit-kind", self.edit_kind, p, cx, |this, k| this.edit_kind = k))
                        .child(div().flex_1())
                        .child(PressButton::new("mem-edit-cancel").small().ghost().label("Отмена").on_click(cx.listener(|this, _, _, cx| {
                            this.editing = None;
                            cx.notify();
                        })))
                        .child(PressButton::new("mem-edit-save").small().primary().label("Сохранить").on_click(cx.listener(|this, _, window, cx| this.save_edit(window, cx)))),
                )
                .into_any_element()
        } else {
            h_flex()
                .gap(px(10.))
                .items_start()
                .child(div().flex_1().min_w_0().text_size(px(14.)).line_height(px(20.)).text_color(p.ink).child(e.text.clone()))
                .child(
                    h_flex()
                        .gap(px(2.))
                        .flex_none()
                        .child(
                            PressButton::new(SharedString::from(format!("mem-edit-{id}")))
                                .xsmall()
                                .ghost()
                                .icon(Icon::empty().path("icons/pencil.svg"))
                                .tooltip("Поправить")
                                .on_click(cx.listener(move |this, _, window, cx| this.start_edit(&edit_entry, window, cx))),
                        )
                        .when(e.project.is_some(), |d| {
                            d.child(
                                PressButton::new(SharedString::from(format!("mem-share-{id}")))
                                    .xsmall()
                                    .ghost()
                                    .icon(Icon::empty().path("icons/globe.svg"))
                                    .tooltip("Сделать общим для всех проектов")
                                    .on_click(cx.listener(move |this, _, _, cx| this.make_shared(&share_id, cx))),
                            )
                        })
                        .child(
                            PressButton::new(SharedString::from(format!("mem-rm-{id}")))
                                .xsmall()
                                .ghost()
                                .icon(Icon::empty().path("icons/trash.svg"))
                                .tooltip("Забыть")
                                .on_click(cx.listener(move |this, _, _, cx| this.remove(&remove_id, cx))),
                        ),
                )
                .into_any_element()
        };
        ui::card(p)
            .px(px(14.))
            .py(px(11.))
            .gap(px(6.))
            .child(body)
            .when(!e.quote.is_empty() && !editing, |d| d.child(div().text_size(px(12.5)).italic().text_color(p.muted).child(format!("«{}»", e.quote))))
            .child(meta)
    }

    fn render_entries(&self, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let project = self.scope_project();
        let query = self.search.read(cx).value().trim().to_string();
        let list: Vec<&Entry> = if query.is_empty() {
            self.memory.entries.iter().filter(|e| e.project == project).collect()
        } else {
            self.memory.search(&query, None).into_iter().filter(|e| e.project == project).collect()
        };
        if list.is_empty() {
            let text = if !query.is_empty() {
                "Ничего не нашёл."
            } else if project.is_some() {
                "По этому проекту пока ничего."
            } else {
                "Пока пусто. Когда вы поправляете агента или что-то запрещаете, он сам запишет это сюда, а «Итоги сессии» соберут то, что он пропустил. Можно и вручную — «Запомнить»."
            };
            return ui::card(p).p(px(18.)).text_size(px(13.5)).text_color(p.muted).child(text).into_any_element();
        }
        v_flex()
            .gap(px(22.))
            .children(Kind::ALL.into_iter().filter_map(|kind| {
                let part: Vec<&&Entry> = list.iter().filter(|e| e.kind == kind).collect();
                (!part.is_empty()).then(|| {
                    v_flex()
                        .gap(px(8.))
                        .child(ui::section_title(kind.title(), Some(part.len().to_string()), p, theme))
                        .children(part.into_iter().map(|e| self.render_entry(e, p, theme, cx)))
                })
            }))
            .into_any_element()
    }

    fn render_log(&self, p: &Palette, theme: &Theme) -> AnyElement {
        if self.memory.log.is_empty() {
            return ui::card(p).p(px(18.)).text_size(px(13.5)).text_color(p.muted).child("Журнал пуст.").into_any_element();
        }
        // Text of every entry as last known, so removals still read as words.
        let mut text: HashMap<&str, &str> = HashMap::new();
        for r in &self.memory.log {
            match &r.op {
                Op::Add { id, text: t, .. } => {
                    text.insert(id, t);
                }
                Op::Edit { id, text: Some(t), .. } => {
                    text.insert(id, t);
                }
                _ => {}
            }
        }
        ui::card(p)
            .children(self.memory.log.iter().rev().take(LOG_ROWS).enumerate().map(|(i, r)| {
                let (verb, tone) = match &r.op {
                    Op::Add { .. } => ("запомнил", p.green.text),
                    Op::Edit { .. } => ("поправил", p.amber.text),
                    Op::Remove { .. } => ("забыл", p.red.text),
                };
                let project = match &r.op {
                    Op::Add { project: Some(pr), .. } => Some(pr.clone()),
                    Op::Edit { project: Some(Some(pr)), .. } => Some(format!("→ {pr}")),
                    Op::Edit { project: Some(None), .. } => Some("→ для всех".into()),
                    _ => None,
                };
                let reason = match &r.op {
                    Op::Remove { reason, .. } if !reason.is_empty() => Some(format!("— {reason}")),
                    _ => None,
                };
                h_flex()
                    .px(px(14.))
                    .py(px(9.))
                    .gap(px(10.))
                    .items_center()
                    .when(i > 0, |d| d.border_t_1().border_color(p.border))
                    .child(ui::mono(ago(r.at), theme).w(px(72.)).flex_none().text_color(p.faint))
                    .child(ui::chip(r.by.clone(), p, theme))
                    .child(div().flex_none().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(tone).child(verb))
                    .child(div().flex_1().min_w_0().truncate().text_size(px(13.)).text_color(p.fg).child(text.get(r.op.id()).copied().unwrap_or(r.op.id()).to_owned()))
                    .when_some(project, |d, pr| d.child(ui::chip(pr, p, theme)))
                    .when_some(reason, |d, why| d.child(div().min_w_0().max_w(px(240.)).truncate().text_size(px(12.)).text_color(p.muted).child(why)))
            }))
            .into_any_element()
    }
}

fn log_stamp(home: &Home) -> Option<SystemTime> {
    std::fs::metadata(memory::dir(home).join("log.jsonl")).and_then(|m| m.modified()).ok()
}

impl Render for MemoryView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let p = Palette::of(&theme);
        let total = self.memory.entries.len();
        let header = h_flex()
            .items_end()
            .gap_3()
            .mb(px(20.))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(6.))
                    .child(ui::eyebrow(format!("журнал для агентов · {total} записей"), &p, &theme))
                    .child(ui::page_title("Память", &p))
                    .child(
                        div()
                            .max_w(px(720.))
                            .text_size(px(13.5))
                            .text_color(p.muted)
                            .child("Что агенты помнят о вас и ваших проектах: поправки, запреты, как устроен проект. Попадает в начало каждой сессии Claude и Codex. Пишут агенты, «Итоги сессии» и вы."),
                    ),
            )
            .when(self.sel != Sel::Log, |d| {
                d.child(PressButton::new("mem-new").small().primary().icon(IconName::Plus).label("Запомнить").on_click(cx.listener(|this, _, window, cx| {
                    this.adding = !this.adding;
                    this.error = None;
                    if this.adding {
                        this.draft.update(cx, |s, cx| s.focus(window, cx));
                    }
                    cx.notify();
                })))
            });

        let right = v_flex()
            .flex_1()
            .min_w_0()
            .gap(px(14.))
            .when(self.sel != Sel::Log, |d| d.child(Input::new(&self.search).small().prefix(Icon::new(IconName::Search).xsmall().text_color(p.faint))))
            .when(self.adding && self.sel != Sel::Log, |d| d.child(self.render_add(&p, cx)))
            .when_some(self.error.clone(), |d, err| d.child(div().text_size(px(12.5)).text_color(p.red.text).child(err)))
            .child(match self.sel {
                Sel::Log => self.render_log(&p, &theme),
                _ => self.render_entries(&p, &theme, cx),
            });

        div().id("memory").size_full().overflow_y_scroll().child(
            v_flex()
                .w_full()
                .max_w(px(1240.))
                .mx_auto()
                .px(px(32.))
                .pt(px(28.))
                .pb(px(48.))
                .child(header)
                .child(h_flex().gap(px(24.)).items_start().child(self.render_nav(&p, &theme, cx)).child(right)),
        )
    }
}
