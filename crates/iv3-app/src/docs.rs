//! «Документы» — the shelf (prototype «Полка»): documents grouped by project or
//! by time on the left, the open document read in place in the middle, its
//! table of contents and provenance («Откуда / Дан агентам») on the right.
//! A document can be handed to an agent: an open session, a new Claude or
//! Codex session, a board task, or copied as a path.

use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use gpui_kit::component::{
    ActiveTheme, Icon, IconName, Sizable as _, Theme, WindowExt as _,
    h_flex,
    input::{Input, InputEvent, InputState},
    skeleton::Skeleton,
    text::{TextView, TextViewStyle},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use gpui_webview::WebView;
use iv3_core::{
    Home, Provider, SessionSummary,
    docs::{self, Author, Doc, DocKind, Given, Section},
};

use crate::{
    app::{AppView, LiveSession},
    format::ago,
    ui::{self, Palette, PressButton},
};

/// What the shelf asks the window to do.
pub enum DocsEvent {
    OpenSession(SessionSummary),
    OpenFile(PathBuf),
    /// Send a message into the open session at this index and show it.
    SendToSession { ix: usize, text: String },
    NewSession { provider: Provider, cwd: PathBuf, prompt: String },
    /// A document was attached to a task: reload the board.
    TaskChanged,
    ShowTasks,
}

impl EventEmitter<DocsEvent> for DocsView {}

enum Body {
    /// Markdown split at headings: one block per section, so the table of
    /// contents can jump and highlight.
    Sections { sections: Vec<Section>, toc: Vec<(usize, u8, String)> },
    Web(Entity<WebView>),
    HtmlFallback(SharedString),
    Text(SharedString),
    /// PDF, images, anything else: shown in the preview panel.
    File,
    Error(SharedString),
}

struct OpenDoc {
    rel: String,
    body: Body,
}

pub struct DocsView {
    home: Home,
    app: WeakEntity<AppView>,
    docs: Vec<Doc>,
    loaded: bool,
    fingerprint: u64,
    by_time: bool,
    collapsed: HashSet<String>,
    search: Entity<InputState>,
    selected: Option<String>,
    open: Option<OpenDoc>,
    menu: bool,
    /// The window shows this page (the native web view must hide otherwise).
    visible: bool,
    focus: FocusHandle,
    scroll: ScrollHandle,
    _watch: Task<()>,
    _search: Subscription,
}

impl DocsView {
    pub fn new(home: Home, app: WeakEntity<AppView>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Название, файл, проект"));
        let sub = cx.subscribe(&search, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        let mut this = Self {
            home,
            app,
            docs: Vec::new(),
            loaded: false,
            fingerprint: 0,
            by_time: false,
            collapsed: HashSet::new(),
            search,
            selected: None,
            open: None,
            menu: false,
            visible: false,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            _watch: Task::ready(()),
            _search: sub,
        };
        this.rescan(cx);
        this._watch = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(1500)).await;
                let Ok(home) = this.update(cx, |this, _| this.home.clone()) else { break };
                let print = cx.background_spawn(async move { docs::fingerprint(&home) }).await;
                let changed = this.update(cx, |this, _| this.fingerprint != print).unwrap_or(false);
                if changed && this.update(cx, |this, cx| this.rescan(cx)).is_err() {
                    break;
                }
            }
        });
        this
    }

    pub fn count(&self) -> usize {
        self.docs.len()
    }

    /// Called by the window on page changes.
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        self.visible = visible;
        if let Some(OpenDoc { body: Body::Web(web), .. }) = &self.open {
            if web.read(cx).visible() != visible {
                let _ = web.read(cx).set_visible(visible);
            }
        }
    }

    fn rescan(&mut self, cx: &mut Context<Self>) {
        let home = self.home.clone();
        cx.spawn(async move |this, cx| {
            let (docs, print) = cx.background_spawn(async move { (docs::scan(&home), docs::fingerprint(&home)) }).await;
            this.update(cx, |this, cx| {
                this.docs = docs;
                this.fingerprint = print;
                this.loaded = true;
                // Keep the open document fresh when its file changed.
                if let Some(rel) = this.open.as_ref().map(|o| o.rel.clone()) {
                    if this.docs.iter().all(|d| d.rel != rel) {
                        this.open = None;
                        this.selected = None;
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn doc(&self, rel: &str) -> Option<&Doc> {
        self.docs.iter().find(|d| d.rel == rel)
    }

    fn select(&mut self, rel: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(doc) = self.doc(&rel).cloned() else { return };
        self.menu = false;
        self.selected = Some(rel.clone());
        let body = match doc.kind {
            DocKind::Markdown => match fs::read_to_string(&doc.path) {
                Ok(text) => {
                    let (_, body) = docs::split_front(&text);
                    let sections = docs::sections(body);
                    let toc = docs::toc(&sections, &doc.title);
                    Body::Sections { sections, toc }
                }
                Err(err) => Body::Error(format!("Не удалось прочитать: {err}").into()),
            },
            DocKind::Html => {
                let builder = wry::WebViewBuilder::new().with_url(file_url(&doc.path));
                match WebView::build(builder, window, cx) {
                    Ok(web) => {
                        let web = cx.new(|_| web);
                        let _ = web.read(cx).set_visible(self.visible);
                        Body::Web(web)
                    }
                    Err(err) => {
                        eprintln!("iv3: docs webview: {err}");
                        Body::HtmlFallback(fs::read_to_string(&doc.path).unwrap_or_default().into())
                    }
                }
            }
            DocKind::Text => match fs::read_to_string(&doc.path) {
                Ok(text) => Body::Text(text.into()),
                Err(err) => Body::Error(format!("Не удалось прочитать: {err}").into()),
            },
            DocKind::Pdf | DocKind::Image | DocKind::Other => {
                cx.emit(DocsEvent::OpenFile(doc.path.clone()));
                Body::File
            }
        };
        self.open = Some(OpenDoc { rel, body });
        self.scroll.set_offset(point(px(0.), px(0.)));
        docs::mark_read(&self.home, &doc);
        if let Some(d) = self.docs.iter_mut().find(|d| d.rel == doc.rel) {
            d.read = true;
        }
        cx.notify();
    }

    /// Documents matching the search, in shelf order, grouped.
    fn groups(&self, cx: &App) -> Vec<(String, Vec<usize>)> {
        let query = self.search.read(cx).value().trim().to_lowercase();
        let groups = if self.by_time { docs::group_by_time(&self.docs, chrono::Local::now()) } else { docs::group_by_project(&self.docs) };
        if query.is_empty() {
            return groups;
        }
        groups
            .into_iter()
            .map(|(name, items)| {
                let items = items
                    .into_iter()
                    .filter(|&i| {
                        let d = &self.docs[i];
                        d.title.to_lowercase().contains(&query)
                            || d.rel.to_lowercase().contains(&query)
                            || d.project.as_deref().is_some_and(|p| p.to_lowercase().contains(&query))
                    })
                    .collect::<Vec<_>>();
                (name, items)
            })
            .filter(|(_, items)| !items.is_empty())
            .collect()
    }

    /// Rows a keyboard can walk: open groups only.
    fn walkable(&self, cx: &App) -> Vec<String> {
        let mut seen = HashSet::new();
        self.groups(cx)
            .into_iter()
            .filter(|(name, _)| !self.collapsed.contains(name))
            .flat_map(|(_, items)| items)
            .map(|i| self.docs[i].rel.clone())
            .filter(|rel| seen.insert(rel.clone()))
            .collect()
    }

    fn step(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let rows = self.walkable(cx);
        if rows.is_empty() {
            return;
        }
        let at = self.selected.as_ref().and_then(|s| rows.iter().position(|r| r == s));
        let next = match at {
            Some(i) => (i as isize + delta).clamp(0, rows.len() as isize - 1) as usize,
            None => 0,
        };
        self.select(rows[next].clone(), window, cx);
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        let mods = &event.keystroke.modifiers;
        if mods.platform || mods.control || mods.alt {
            return;
        }
        let typing = self.search.read(cx).focus_handle(cx).is_focused(window);
        if typing {
            match key {
                "escape" => {
                    self.search.update(cx, |s, cx| s.set_value("", window, cx));
                    window.focus(&self.focus, cx);
                }
                "enter" | "down" => {
                    window.focus(&self.focus, cx);
                    self.step(0, window, cx);
                }
                _ => return,
            }
            cx.stop_propagation();
            return;
        }
        match key {
            "/" => self.search.update(cx, |s, cx| s.focus(window, cx)),
            "j" | "down" => self.step(1, window, cx),
            "k" | "up" => self.step(-1, window, cx),
            "a" if self.open.is_some() => {
                self.menu = !self.menu;
                cx.notify();
            }
            "escape" if self.menu => {
                self.menu = false;
                cx.notify();
            }
            _ => return,
        }
        cx.stop_propagation();
    }

    // ------------------------------------------------------------- give

    fn session_by_id(&self, id: &str, cx: &App) -> Option<SessionSummary> {
        self.app.upgrade()?.read(cx).sessions.iter().find(|s| s.id == id).cloned()
    }

    /// Message that hands the document to an agent: the path and, for small
    /// text documents, the text itself.
    fn context_message(doc: &Doc) -> String {
        let mut msg = format!("Документ как контекст — «{}»: {}", doc.title, doc.path.display());
        if doc.kind.readable() && doc.size < 12 * 1024 {
            if let Ok(text) = fs::read_to_string(&doc.path) {
                let fence = if doc.kind == DocKind::Html { "html" } else { "markdown" };
                msg.push_str(&format!("\n\n```{fence}\n{}\n```", text.trim()));
            }
        }
        msg
    }

    fn log(&self, doc: &Doc, target: String, title: String, agent: Option<&str>) {
        docs::log_given(&self.home, &Given { at: chrono::Utc::now(), doc: doc.rel.clone(), target, title, agent: agent.map(str::to_owned) });
    }

    fn give_to_session(&mut self, ix: usize, title: SharedString, provider: Provider, cx: &mut Context<Self>) {
        let Some(doc) = self.open.as_ref().and_then(|o| self.doc(&o.rel)).cloned() else { return };
        let native = self.app.upgrade().and_then(|app| app.read(cx).open_sessions.get(ix).and_then(|v| v.read(cx).native_id.clone()));
        self.log(&doc, format!("session:{}", native.unwrap_or_default()), title.to_string(), Some(provider_name(provider)));
        self.menu = false;
        cx.emit(DocsEvent::SendToSession { ix, text: Self::context_message(&doc) });
        self.after_give(cx);
    }

    fn give_new(&mut self, provider: Provider, cx: &mut Context<Self>) {
        let Some(doc) = self.open.as_ref().and_then(|o| self.doc(&o.rel)).cloned() else { return };
        // Work where the document came from: its session's project, else the
        // newest session of its project, else the docs folder.
        let sessions = self.app.upgrade().map(|a| a.read(cx).sessions.clone()).unwrap_or_default();
        let cwd = doc
            .session
            .as_ref()
            .and_then(|id| sessions.iter().find(|s| &s.id == id))
            .or_else(|| doc.project.as_ref().and_then(|p| sessions.iter().find(|s| &s.project() == p)))
            .and_then(|s| s.cwd.clone())
            .filter(|c| c.is_dir())
            .unwrap_or_else(|| self.home.docs());
        let prompt = format!("{}\n\nПрочитай и коротко подтверди, что понял. Дальше жди задачу.", Self::context_message(&doc));
        let name = provider_name(provider);
        self.log(&doc, format!("new:{name}"), format!("новая сессия {name}"), Some(name));
        self.menu = false;
        cx.emit(DocsEvent::NewSession { provider, cwd, prompt });
        self.after_give(cx);
    }

    fn give_task(&mut self, task_id: String, task_title: String, cx: &mut Context<Self>) {
        let Some(doc) = self.open.as_ref().and_then(|o| self.doc(&o.rel)).cloned() else { return };
        let board = iv3_core::Board::new(self.home.clone());
        let result = board.get(&task_id).and_then(|mut task| {
            if !task.meta.attachments.contains(&doc.path) {
                task.meta.attachments.push(doc.path.clone());
                task.save()?;
            }
            board.log(&task_id, "ты", &format!("Прикреплён документ «{}»", doc.title))
        });
        if let Err(err) = result {
            eprintln!("iv3: attach doc to task: {err:#}");
            return;
        }
        self.log(&doc, format!("task:{task_id}"), task_title, None);
        self.menu = false;
        cx.emit(DocsEvent::TaskChanged);
        self.after_give(cx);
    }

    fn copy_path(&mut self, cx: &mut Context<Self>) {
        let Some(doc) = self.open.as_ref().and_then(|o| self.doc(&o.rel)).cloned() else { return };
        cx.write_to_clipboard(ClipboardItem::new_string(format!("@{}", doc.path.display())));
        self.log(&doc, "copy".into(), "скопирован путь".into(), None);
        self.menu = false;
        self.after_give(cx);
    }

    fn after_give(&mut self, cx: &mut Context<Self>) {
        if let Some(rel) = self.open.as_ref().map(|o| o.rel.clone()) {
            if let Some(d) = self.docs.iter_mut().find(|d| d.rel == rel) {
                d.given += 1;
            }
        }
        cx.notify();
    }
}

fn provider_name(p: Provider) -> &'static str {
    match p {
        Provider::Claude => "claude",
        Provider::Codex => "codex",
    }
}

// ------------------------------------------------------------------ render

impl Render for DocsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let p = Palette::of(&theme);
        // Rendered only while the Docs page is on screen.
        self.visible = true;
        // Open something on first visit: the shelf is for reading.
        if self.loaded && self.open.is_none() && self.selected.is_none() {
            if let Some(first) = self.walkable(cx).into_iter().next() {
                self.selected = Some(first.clone());
                cx.defer_in(window, move |this, window, cx| this.select(first, window, cx));
            }
        }
        // The native web view sits above GPUI: step aside for menus and dialogs.
        if let Some(OpenDoc { body: Body::Web(web), .. }) = &self.open {
            let show = self.visible && !self.menu && !window.has_active_dialog(cx) && !window.has_active_sheet(cx);
            if web.read(cx).visible() != show {
                let _ = web.read(cx).set_visible(show);
            }
        }
        h_flex()
            .id("docs")
            .key_context("Docs")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .size_full()
            .items_start()
            .child(self.render_shelf(&p, &theme, window, cx))
            .child(self.render_reader(&p, &theme, cx))
    }
}

impl DocsView {
    fn render_shelf(&mut self, p: &Palette, theme: &Theme, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let unread = self.docs.iter().filter(|d| !d.read).count();
        let groups = self.groups(cx);
        let mono = theme.mono_font_family.clone();
        let docs_dir = self.home.docs();
        v_flex()
            .w(px(340.))
            .flex_shrink_0()
            .h_full()
            .border_r_1()
            .border_color(p.border)
            .bg(p.sunken.opacity(0.5))
            .child(
                v_flex()
                    .px(px(16.))
                    .pt(px(30.))
                    .pb(px(12.))
                    .gap(px(10.))
                    .child(
                        h_flex()
                            .items_start()
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap(px(4.))
                                    .child(ui::page_title("Документы", p).text_size(px(22.)).line_height(px(28.)))
                                    .child(
                                        h_flex()
                                            .min_w_0()
                                            .gap(px(6.))
                                            .font_family(mono.clone())
                                            .text_size(px(11.5))
                                            .text_color(p.muted)
                                            .when(unread > 0, |d| {
                                                d.child(div().flex_shrink_0().text_color(p.accent.text).font_weight(FontWeight::SEMIBOLD).child(format!("{unread} {}", new_word(unread))))
                                            })
                                            .child(div().min_w_0().truncate().child(format!("{} в {}", self.docs.len(), tilde(&docs_dir)))),
                                    ),
                            )
                            .child(
                                PressButton::new("docs-folder")
                                    .xsmall()
                                    .icon(IconName::Folder)
                                    .tooltip("Открыть папку")
                                    .on_click(cx.listener(move |_, _, _, cx| cx.emit(DocsEvent::OpenFile(docs_dir.clone())))),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap(px(6.))
                            .child(div().flex_1().child(Input::new(&self.search).small().prefix(Icon::new(IconName::Search).xsmall().text_color(p.faint))))
                            .child(kbd("/", p, theme)),
                    )
                    .child(
                        h_flex()
                            .p(px(3.))
                            .gap(px(2.))
                            .rounded(px(8.))
                            .border_1()
                            .border_color(p.border)
                            .bg(p.sunken)
                            .w_auto()
                            .child(self.segment("docs-by-project", "По проектам", !self.by_time, false, p, cx))
                            .child(self.segment("docs-by-time", "По времени", self.by_time, true, p, cx)),
                    ),
            )
            .child(
                v_flex()
                    .id("docs-shelf")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(px(8.))
                    .pb(px(16.))
                    .map(|this| {
                        if !self.loaded {
                            return this.children((0..2).map(|g| shelf_skeleton(g, p)));
                        }
                        if self.docs.is_empty() {
                            return this.child(
                                div()
                                    .px(px(8.))
                                    .py(px(12.))
                                    .text_size(px(12.5))
                                    .text_color(p.muted)
                                    .child("Полка пустая — справа написано, как её наполнить."),
                            );
                        }
                        if groups.is_empty() {
                            return this.child(div().px(px(8.)).py(px(12.)).text_size(px(12.5)).text_color(p.muted).child("Ничего не нашлось"));
                        }
                        this.children(groups.into_iter().map(|(name, items)| self.render_group(name, items, p, theme, cx)))
                    }),
            )
    }

    fn segment(&self, id: &'static str, label: &'static str, active: bool, by_time: bool, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .id(id)
            .h(px(26.))
            .px(px(10.))
            .rounded(px(6.))
            .border_1()
            .border_color(transparent_black())
            .text_size(px(12.5))
            .cursor_pointer()
            .map(|d| {
                if active {
                    d.bg(p.surface).border_color(p.border).shadow(ui::edge_shadow(p.border_strong, 1.)).text_color(p.ink).font_weight(FontWeight::SEMIBOLD)
                } else {
                    d.text_color(p.muted).hover(|s| s.bg(p.ink.opacity(0.04)))
                }
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                this.by_time = by_time;
                cx.notify();
            }))
            .child(label)
    }

    fn render_group(&self, name: String, items: Vec<usize>, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let collapsed = self.collapsed.contains(&name);
        let toggle = name.clone();
        let mark: AnyElement = if name == "Закреплённые" {
            Icon::empty().path("icons/pin.svg").xsmall().text_color(p.accent.solid).into_any_element()
        } else if self.by_time {
            Icon::new(IconName::Calendar).xsmall().text_color(p.faint).into_any_element()
        } else {
            project_mark(&name, theme).into_any_element()
        };
        v_flex()
            .mt(px(6.))
            .child(
                h_flex()
                    .id(SharedString::from(format!("docs-group:{name}")))
                    .h(px(30.))
                    .px(px(8.))
                    .gap(px(8.))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .hover(|s| s.bg(p.ink.opacity(0.04)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.collapsed.remove(&toggle) {
                            this.collapsed.insert(toggle.clone());
                        }
                        cx.notify();
                    }))
                    .child(Icon::new(if collapsed { IconName::ChevronRight } else { IconName::ChevronDown }).xsmall().text_color(p.faint))
                    .child(mark)
                    .child(div().flex_1().min_w_0().truncate().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(p.fg).child(name))
                    .child(div().font_family(theme.mono_font_family.clone()).text_size(px(11.)).text_color(p.faint).child(items.len().to_string())),
            )
            .when(!collapsed, |this| this.children(items.into_iter().map(|i| self.render_row(&self.docs[i], p, theme, cx))))
    }

    fn render_row(&self, d: &Doc, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.selected.as_deref() == Some(d.rel.as_str());
        let rel = d.rel.clone();
        let mono = theme.mono_font_family.clone();
        h_flex()
            .id(SharedString::from(format!("doc:{}", d.rel)))
            .relative()
            .items_start()
            .ml(px(4.))
            .px(px(10.))
            .py(px(8.))
            .gap(px(10.))
            .rounded(px(8.))
            .border_1()
            .border_color(transparent_black())
            .cursor_pointer()
            .map(|this| {
                if active {
                    this.bg(p.surface).border_color(p.border).shadow(ui::edge_shadow(p.border_strong, 1.))
                } else {
                    this.hover(|s| s.bg(p.ink.opacity(0.04)))
                }
            })
            .on_click(cx.listener(move |this, _, window, cx| {
                window.focus(&this.focus, cx);
                this.select(rel.clone(), window, cx);
            }))
            .when(!d.read, |this| this.child(div().absolute().left(px(-8.)).top(px(16.)).size(px(6.)).rounded_full().bg(p.accent.solid)))
            .child(kind_box(d.kind, p))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(4.))
                    .child(
                        div()
                            .truncate()
                            .text_size(px(13.))
                            .text_color(p.ink)
                            .when(active || !d.read, |t| t.font_weight(FontWeight::SEMIBOLD))
                            .child(d.title.clone()),
                    )
                    .child(
                        h_flex()
                            .gap(px(6.))
                            .children(d.author.map(|a| author_chip(a, p, theme)))
                            .child(div().font_family(mono.clone()).text_size(px(11.)).text_color(p.faint).child(docs::short_time(d.modified, chrono::Local::now())))
                            .child(div().flex_1())
                            .when(d.given > 0, |row| {
                                row.child(div().font_family(mono.clone()).text_size(px(11.)).text_color(p.faint).child(format!("↗ {}", d.given)))
                            }),
                    ),
            )
    }

    fn render_reader(&mut self, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        if !self.loaded {
            return reader_skeleton(p).into_any_element();
        }
        if self.docs.is_empty() {
            return self.render_empty(p, theme, cx).into_any_element();
        }
        let Some(open) = &self.open else {
            return v_flex()
                .flex_1()
                .h_full()
                .items_center()
                .justify_center()
                .gap(px(6.))
                .child(Icon::new(IconName::BookOpen).text_color(p.faint))
                .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).child("Выбери документ на полке"))
                .child(div().text_size(px(12.5)).text_color(p.muted).child("j / k — по списку, / — поиск, a — дать агенту"))
                .into_any_element();
        };
        let Some(doc) = self.doc(&open.rel).cloned() else { return div().into_any_element() };

        let header = self.render_doc_header(&doc, p, theme, cx);
        let center: AnyElement = match &open.body {
            Body::Sections { sections, .. } => {
                let scroll = self.scroll.clone();
                v_flex()
                    .id("doc-scroll")
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .overflow_y_scroll()
                    .track_scroll(&scroll)
                    .on_scroll(cx.listener(|_, _, _, cx| cx.notify()))
                    .child(header)
                    .children((0..sections.len()).map(|i| {
                        let text = docs::section_body(sections, i).to_owned();
                        div()
                            .px(px(48.))
                            .max_w(px(820.))
                            .pb(px(6.))
                            .text_size(px(14.5))
                            .child(TextView::markdown(SharedString::from(format!("{}#{i}", doc.rel)), text).style(reader_style()).selectable(true))
                    }))
                    .child(div().h(px(80.)))
                    .into_any_element()
            }
            Body::Web(web) => v_flex()
                .flex_1()
                .min_w_0()
                .h_full()
                .child(header)
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .ml(px(48.))
                        .mr(px(16.))
                        .mb(px(16.))
                        .rounded(px(8.))
                        .border_1()
                        .border_color(p.border)
                        .overflow_hidden()
                        .child(web.clone()),
                )
                .into_any_element(),
            Body::HtmlFallback(text) => v_flex()
                .id("doc-scroll")
                .flex_1()
                .min_w_0()
                .h_full()
                .overflow_y_scroll()
                .child(header)
                .child(div().px(px(48.)).max_w(px(820.)).child(TextView::html(SharedString::from(doc.rel.clone()), text.clone()).selectable(true)))
                .into_any_element(),
            Body::Text(text) => v_flex()
                .id("doc-scroll")
                .flex_1()
                .min_w_0()
                .h_full()
                .overflow_y_scroll()
                .child(header)
                .child(
                    div()
                        .mx(px(48.))
                        .p(px(14.))
                        .rounded(px(8.))
                        .border_1()
                        .border_color(p.border)
                        .bg(p.surface)
                        .font_family(theme.mono_font_family.clone())
                        .text_size(px(12.5))
                        .text_color(p.fg)
                        .child(text.clone()),
                )
                .into_any_element(),
            Body::File | Body::Error(_) => {
                let note: SharedString = match &open.body {
                    Body::Error(e) => e.clone(),
                    _ => "Открыт в панели справа".into(),
                };
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(header)
                    .child(div().px(px(48.)).text_size(px(13.)).text_color(p.muted).child(note))
                    .into_any_element()
            }
        };
        h_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .items_start()
            .child(center)
            .child(self.render_aside(&doc, p, theme, cx))
            .into_any_element()
    }

    fn render_doc_header(&self, doc: &Doc, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let mono = theme.mono_font_family.clone();
        let source = doc.session.as_ref().and_then(|id| self.session_by_id(id, cx));
        let path = doc.path.clone();
        let pin_rel = doc.rel.clone();
        v_flex()
            .px(px(48.))
            .pt(px(30.))
            .pb(px(18.))
            .max_w(px(820.))
            .gap(px(10.))
            .child(
                h_flex()
                    .gap(px(6.))
                    .font_family(mono.clone())
                    .text_size(px(11.5))
                    .text_color(p.faint)
                    .when_some(doc.project.clone(), |d, project| {
                        d.child(project_mark(&project, theme)).child(div().text_color(p.muted).child(project)).child("/")
                    })
                    .child(format!("docs/{}", doc.rel)),
            )
            .child(div().text_size(px(26.)).line_height(px(32.)).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).child(doc.title.clone()))
            .child(
                h_flex()
                    .flex_wrap()
                    .gap(px(6.))
                    .text_size(px(12.5))
                    .text_color(p.muted)
                    .children(doc.author.map(|a| author_chip(a, p, theme)))
                    .when_some(source.clone(), |d, s| {
                        let open = s.clone();
                        d.child(
                            div()
                                .id("doc-source")
                                .cursor_pointer()
                                .underline()
                                .text_color(p.fg)
                                .on_click(cx.listener(move |_, _, _, cx| cx.emit(DocsEvent::OpenSession(open.clone()))))
                                .child(format!("сессия «{}»", short(&s.title, 48))),
                        )
                    })
                    .child(div().text_color(p.faint).child("·"))
                    .child(full_time(doc.modified))
                    .when(doc.kind.readable(), |d| d.child(div().text_color(p.faint).child("·")).child(format!("{} мин чтения", doc.read_minutes()))),
            )
            .when_some(doc.summary.clone().filter(|s| !s.is_empty()), |d, s| d.child(div().text_size(px(13.5)).text_color(p.muted).child(s)))
            .child(
                h_flex()
                    .relative()
                    .mt(px(4.))
                    .gap(px(8.))
                    .pb(px(14.))
                    .border_b_1()
                    .border_color(p.border)
                    .child(
                        PressButton::new("doc-give")
                            .primary()
                            .small()
                            .icon(IconName::ChevronDown)
                            .label("Дать агенту")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.menu = !this.menu;
                                cx.notify();
                            })),
                    )
                    .child(
                        PressButton::new("doc-source-file")
                            .small()
                            .icon(IconName::FileText)
                            .label("Исходник")
                            .tooltip("Открыть файл в панели справа")
                            .on_click(cx.listener(move |_, _, _, cx| cx.emit(DocsEvent::OpenFile(path.clone())))),
                    )
                    .child(div().flex_1())
                    .child(
                        PressButton::new("doc-pin")
                            .ghost()
                            .small()
                            .icon(Icon::empty().path("icons/pin.svg"))
                            .tooltip(if doc.pinned { "Открепить" } else { "Закрепить" })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let pinned = docs::toggle_pin(&this.home, &pin_rel);
                                if let Some(d) = this.docs.iter_mut().find(|d| d.rel == pin_rel) {
                                    d.pinned = pinned;
                                }
                                cx.notify();
                            })),
                    )
                    .when(self.menu, |d| d.child(deferred(self.render_menu(doc, p, theme, cx)).with_priority(2))),
            )
            .into_any_element()
    }

    fn render_menu(&self, doc: &Doc, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let mono = theme.mono_font_family.clone();
        let live: Vec<LiveSession> = self.app.upgrade().map(|a| a.read(cx).live_sessions(cx)).unwrap_or_default();
        // The document's own task first, then tasks being worked on.
        let tasks: Vec<(String, String)> = self
            .app
            .upgrade()
            .map(|a| {
                let all = &a.read(cx).tasks;
                let mut list: Vec<(String, String)> = Vec::new();
                if let Some(t) = doc.task.as_ref().and_then(|id| all.iter().find(|t| &t.id == id)) {
                    list.push((t.id.clone(), t.meta.title.clone()));
                }
                for t in all.iter().filter(|t| matches!(t.meta.status, iv3_core::TaskStatus::InProgress | iv3_core::TaskStatus::Todo | iv3_core::TaskStatus::Blocked)) {
                    if list.len() >= 4 {
                        break;
                    }
                    if !list.iter().any(|(id, _)| id == &t.id) {
                        list.push((t.id.clone(), t.meta.title.clone()));
                    }
                }
                list
            })
            .unwrap_or_default();
        let item = |id: SharedString| {
            h_flex()
                .id(id)
                .px(px(10.))
                .py(px(7.))
                .gap(px(10.))
                .rounded(px(6.))
                .cursor_pointer()
                .hover(|s| s.bg(p.surface2))
        };
        let section = |text: &'static str| div().px(px(10.)).pt(px(8.)).pb(px(4.)).child(ui::eyebrow(text, p, theme));
        v_flex()
            .id("doc-give-menu")
            .absolute()
            .top(px(40.))
            .left_0()
            .w(px(320.))
            .p(px(4.))
            .rounded(px(10.))
            .border_1()
            .border_color(p.border)
            .bg(p.surface)
            .shadow(ui::popover_shadow())
            .occlude()
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.menu = false;
                cx.notify();
            }))
            .when(!live.is_empty(), |m| {
                m.child(section("открытые сессии")).children(live.into_iter().map(|s| {
                    let (ix, title, provider) = (s.ix, s.title.clone(), s.provider);
                    item(SharedString::from(format!("give-live:{}", s.ix)))
                        .on_click(cx.listener(move |this, _, _, cx| this.give_to_session(ix, title.clone(), provider, cx)))
                        .child(ui::provider_chip(s.provider, p, theme))
                        .child(
                            v_flex()
                                .min_w_0()
                                .flex_1()
                                .child(div().truncate().text_size(px(13.)).text_color(p.ink).child(s.title.clone()))
                                .child(
                                    div()
                                        .truncate()
                                        .font_family(mono.clone())
                                        .text_size(px(10.5))
                                        .text_color(p.faint)
                                        .child(format!("{} · {}", s.project, s.card.progress.to_lowercase())),
                                ),
                        )
                }))
                .child(div().my(px(4.)).h(px(1.)).bg(p.border))
            })
            .child(
                item("give-new-claude".into())
                    .on_click(cx.listener(|this, _, _, cx| this.give_new(Provider::Claude, cx)))
                    .child(Icon::new(IconName::Plus).xsmall().text_color(p.muted))
                    .child(div().text_size(px(13.)).text_color(p.ink).child("Новая сессия Claude с документом")),
            )
            .child(
                item("give-new-codex".into())
                    .on_click(cx.listener(|this, _, _, cx| this.give_new(Provider::Codex, cx)))
                    .child(Icon::new(IconName::Plus).xsmall().text_color(p.muted))
                    .child(div().text_size(px(13.)).text_color(p.ink).child("Новая сессия Codex с документом")),
            )
            .when(!tasks.is_empty(), |m| {
                m.child(div().my(px(4.)).h(px(1.)).bg(p.border)).child(section("прикрепить к задаче")).children(tasks.into_iter().map(|(id, title)| {
                    let (tid, ttitle) = (id.clone(), title.clone());
                    item(SharedString::from(format!("give-task:{id}")))
                        .on_click(cx.listener(move |this, _, _, cx| this.give_task(tid.clone(), ttitle.clone(), cx)))
                        .child(Icon::empty().path("icons/list-checks.svg").xsmall().text_color(p.muted))
                        .child(div().min_w_0().flex_1().truncate().text_size(px(13.)).text_color(p.ink).child(title))
                        .child(div().font_family(mono.clone()).text_size(px(10.5)).text_color(p.faint).child(id))
                }))
            })
            .child(div().my(px(4.)).h(px(1.)).bg(p.border))
            .child(
                item("give-copy".into())
                    .on_click(cx.listener(|this, _, _, cx| this.copy_path(cx)))
                    .child(Icon::new(IconName::Copy).xsmall().text_color(p.muted))
                    .child(div().text_size(px(13.)).text_color(p.ink).child("Скопировать @путь")),
            )
    }

    fn render_aside(&self, doc: &Doc, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let mono = theme.mono_font_family.clone();
        let toc: Vec<(usize, u8, String)> = match self.open.as_ref().map(|o| &o.body) {
            Some(Body::Sections { toc, .. }) => toc.clone(),
            _ => Vec::new(),
        };
        // The section at the top of the reader (child 0 is the header).
        let current = self.scroll.top_item().saturating_sub(1);
        let active = toc.iter().rposition(|(i, _, _)| *i <= current).unwrap_or(0);
        let source = doc.session.as_ref().and_then(|id| self.session_by_id(id, cx));
        let task = doc
            .task
            .as_ref()
            .and_then(|id| self.app.upgrade().and_then(|a| a.read(cx).tasks.iter().find(|t| &t.id == id).map(|t| t.meta.title.clone())).or(Some(id.clone())));
        let given: Vec<Given> = {
            let mut all: Vec<Given> = docs::load_given(&self.home).into_iter().filter(|g| g.doc == doc.rel).collect();
            all.reverse();
            all.truncate(8);
            all
        };
        let label = |text: &'static str| ui::eyebrow(text, p, theme).mt(px(18.)).mb(px(8.));
        v_flex()
            .id("doc-aside")
            .w(px(240.))
            .flex_shrink_0()
            .h_full()
            .overflow_y_scroll()
            .pl(px(8.))
            .pr(px(20.))
            .pt(px(30.))
            .when(!toc.is_empty(), |a| {
                a.child(ui::eyebrow("содержание", p, theme).mb(px(8.))).children(toc.into_iter().enumerate().map(|(n, (section, level, text))| {
                    let on = n == active;
                    let scroll = self.scroll.clone();
                    div()
                        .id(SharedString::from(format!("toc:{section}")))
                        .pl(px(10. + 10. * (level.saturating_sub(2)) as f32))
                        .py(px(5.))
                        .border_l_2()
                        .border_color(if on { p.accent.solid } else { p.border })
                        .text_size(px(12.5))
                        .text_color(if on { p.ink } else { p.muted })
                        .when(on, |d| d.font_weight(FontWeight::SEMIBOLD))
                        .cursor_pointer()
                        .hover(|s| s.text_color(p.ink))
                        .on_click(cx.listener(move |_, _, _, cx| {
                            scroll.scroll_to_item(section + 1);
                            cx.notify();
                        }))
                        .child(text)
                }))
            })
            .when(source.is_some() || task.is_some(), |a| {
                a.child(label("откуда"))
                    .when_some(source, |a, s| {
                        let open = s.clone();
                        a.child(
                            h_flex()
                                .id("aside-source")
                                .items_start()
                                .gap(px(6.))
                                .mb(px(8.))
                                .cursor_pointer()
                                .on_click(cx.listener(move |_, _, _, cx| cx.emit(DocsEvent::OpenSession(open.clone()))))
                                .child(ui::provider_chip(s.provider, p, theme))
                                .child(div().min_w_0().text_size(px(12.5)).text_color(p.fg).underline().child(format!("сессия «{}»", short(&s.title, 60)))),
                        )
                    })
                    .when_some(task, |a, t| {
                        a.child(
                            h_flex()
                                .id("aside-task")
                                .gap(px(6.))
                                .cursor_pointer()
                                .on_click(cx.listener(|_, _, _, cx| cx.emit(DocsEvent::ShowTasks)))
                                .child(Icon::empty().path("icons/list-checks.svg").xsmall().text_color(p.muted))
                                .child(div().min_w_0().truncate().text_size(px(12.5)).text_color(p.fg).underline().child(t)),
                        )
                    })
            })
            .child(label("дан агентам"))
            .map(|a| {
                if given.is_empty() {
                    return a.child(div().text_size(px(12.)).text_color(p.faint).child("Пока никому. «Дать агенту» или клавиша a."));
                }
                a.children(given.into_iter().map(|g| {
                    let chip: AnyElement = match g.agent.as_deref() {
                        Some("claude") => ui::provider_chip(Provider::Claude, p, theme).into_any_element(),
                        Some("codex") => ui::provider_chip(Provider::Codex, p, theme).into_any_element(),
                        _ if g.target.starts_with("task:") => ui::chip("задача", p, theme).into_any_element(),
                        _ => ui::chip("путь", p, theme).into_any_element(),
                    };
                    h_flex()
                        .gap(px(6.))
                        .mb(px(6.))
                        .child(chip)
                        .child(div().flex_1().min_w_0().truncate().text_size(px(12.)).text_color(p.fg).child(g.title))
                        .child(div().font_family(mono.clone()).text_size(px(10.5)).text_color(p.faint).child(ago(g.at)))
                }))
            })
    }

    fn render_empty(&self, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let readme = self.home.docs().join("README.md");
        let mono = theme.mono_font_family.clone();
        let sample = "---\ntitle: Разбор /events/search\nproject: outpoll-web\nauthor: codex\nsession: <id сессии>\n---\n# …";
        v_flex().flex_1().h_full().items_center().justify_center().child(
            ui::card(p)
                .w(px(520.))
                .p(px(24.))
                .gap(px(12.))
                .child(div().text_size(px(17.)).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).child("Полка пока пустая"))
                .child(div().text_size(px(13.5)).text_color(p.muted).child(
                    "Агенты кладут отчёты, спеки и HTML-разборы в ~/.iv3/docs — каждая сессия видит эту папку как $IV3_DOCS. Подпапка — это проект; документ появится здесь сам, как только файл будет записан.",
                ))
                .child(
                    div()
                        .p(px(12.))
                        .rounded(px(8.))
                        .border_1()
                        .border_color(p.border)
                        .bg(p.surface2)
                        .font_family(mono)
                        .text_size(px(12.))
                        .text_color(p.fg)
                        .child(sample),
                )
                .child(div().text_size(px(12.5)).text_color(p.muted).child("Попроси агента: «запиши разбор в $IV3_DOCS/<проект>/<тема>.md с фронтматтером»."))
                .child(
                    h_flex()
                        .gap(px(8.))
                        .child(
                            PressButton::new("docs-empty-folder")
                                .small()
                                .icon(IconName::Folder)
                                .label("Открыть папку")
                                .on_click(cx.listener({
                                    let dir = self.home.docs();
                                    move |_, _, _, cx| cx.emit(DocsEvent::OpenFile(dir.clone()))
                                })),
                        )
                        .when(readme.exists(), |d| {
                            d.child(
                                PressButton::new("docs-empty-readme")
                                    .small()
                                    .ghost()
                                    .icon(IconName::BookOpen)
                                    .label("README")
                                    .on_click(cx.listener(move |_, _, _, cx| cx.emit(DocsEvent::OpenFile(readme.clone())))),
                            )
                        }),
                ),
        )
    }
}

// ------------------------------------------------------------------ bits

/// Calm headings for reading: H1 20, H2 17, H3 15 (the title above is 26).
fn reader_style() -> TextViewStyle {
    TextViewStyle::default().paragraph_gap(rems(0.7)).heading_font_size(|level, _| match level {
        1 => px(20.),
        2 => px(17.),
        3 => px(15.),
        _ => px(14.),
    })
}

fn kind_box(kind: DocKind, p: &Palette) -> impl IntoElement {
    let icon = match kind {
        DocKind::Html => IconName::Globe,
        DocKind::Markdown | DocKind::Text => IconName::FileText,
        _ => IconName::File,
    };
    let color = if kind == DocKind::Html { p.blue.solid } else { p.muted };
    div()
        .size(px(24.))
        .flex_shrink_0()
        .rounded(px(6.))
        .border_1()
        .border_color(p.border)
        .bg(p.surface)
        .flex()
        .items_center()
        .justify_center()
        .child(Icon::new(icon).xsmall().text_color(color))
}

fn author_chip(a: Author, p: &Palette, theme: &Theme) -> AnyElement {
    match a {
        Author::Claude => ui::provider_chip(Provider::Claude, p, theme).into_any_element(),
        Author::Codex => ui::provider_chip(Provider::Codex, p, theme).into_any_element(),
        Author::You => ui::chip("ты", p, theme).text_color(p.blue.text).border_color(ui::mix(p.blue.solid, p.border, 0.35)).bg(p.blue.soft).into_any_element(),
        Author::Auto => ui::chip("авто", p, theme).into_any_element(),
    }
}

/// Stable color square with the project's initials.
fn project_mark(name: &str, theme: &Theme) -> impl IntoElement {
    const HUES: [f32; 8] = [16., 228., 145., 38., 280., 190., 340., 95.];
    let h = name.bytes().fold(0u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32));
    let parts: Vec<&str> = name.split(|c: char| !c.is_alphanumeric()).filter(|s| !s.is_empty()).collect();
    let initials: String = match parts.as_slice() {
        [] => "·".into(),
        [one] => one.chars().take(2).collect(),
        [a, b, ..] => a.chars().take(1).chain(b.chars().take(1)).collect(),
    };
    div()
        .size(px(16.))
        .flex_shrink_0()
        .rounded(px(4.))
        .flex()
        .items_center()
        .justify_center()
        .bg(hsla(HUES[(h % 8) as usize] / 360., 0.62, 0.48, 1.))
        .font_family(theme.mono_font_family.clone())
        .text_size(px(8.5))
        .font_weight(FontWeight::BOLD)
        .text_color(white())
        .child(initials.to_uppercase())
}

fn kbd(key: &'static str, p: &Palette, theme: &Theme) -> impl IntoElement {
    div()
        .px(px(5.))
        .rounded(px(4.))
        .border_1()
        .border_b_2()
        .border_color(p.border)
        .bg(p.surface)
        .font_family(theme.mono_font_family.clone())
        .text_size(px(10.5))
        .text_color(p.muted)
        .child(key)
}

fn shelf_skeleton(key: usize, p: &Palette) -> impl IntoElement {
    const WIDTHS: [f32; 4] = [0.78, 0.6, 0.7, 0.5];
    v_flex()
        .id(SharedString::from(format!("docs-skeleton-{key}")))
        .mt(px(10.))
        .gap(px(6.))
        .child(h_flex().px(px(8.)).gap(px(8.)).child(Skeleton::new().size(px(16.)).rounded(px(4.))).child(Skeleton::new().w(px(110.)).h_3().rounded(px(4.))))
        .children((0..3).map(|i| {
            h_flex()
                .px(px(14.))
                .py(px(8.))
                .gap(px(10.))
                .child(Skeleton::new().size(px(24.)).rounded(px(6.)))
                .child(
                    v_flex()
                        .flex_1()
                        .gap(px(6.))
                        .child(Skeleton::new().w(relative(WIDTHS[(key + i) % 4])).h_3().rounded(px(4.)))
                        .child(Skeleton::new().secondary().w(px(90.)).h(px(10.)).rounded(px(4.))),
                )
        }))
        .bg(p.sunken.opacity(0.))
}

fn reader_skeleton(_p: &Palette) -> impl IntoElement {
    v_flex()
        .flex_1()
        .px(px(48.))
        .pt(px(34.))
        .gap(px(12.))
        .child(Skeleton::new().w(px(220.)).h(px(10.)).rounded(px(4.)))
        .child(Skeleton::new().w(relative(0.6)).h(px(26.)).rounded(px(6.)))
        .child(Skeleton::new().secondary().w(px(300.)).h(px(12.)).rounded(px(4.)))
        .child(Skeleton::new().secondary().w_full().h(px(180.)).rounded(px(8.)))
        .child(Skeleton::new().secondary().w(relative(0.8)).h(px(12.)).rounded(px(4.)))
}

fn new_word(n: usize) -> &'static str {
    match (n % 10, n % 100) {
        (1, r) if r != 11 => "новый",
        (2..=4, r) if !(12..=14).contains(&r) => "новых",
        _ => "новых",
    }
}

/// "сегодня 11:15", "вчера 09:02", "12 нояб 14:00".
fn full_time(t: chrono::DateTime<chrono::Utc>) -> String {
    let now = chrono::Local::now();
    let local = t.with_timezone(&chrono::Local);
    let day = match docs::short_time(t, now).as_str() {
        s if s.contains(':') => "сегодня".to_owned(),
        "вчера" => "вчера".to_owned(),
        _ => local.format("%d.%m").to_string(),
    };
    format!("{day} {}", local.format("%H:%M"))
}

fn short(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_owned(),
    }
}

fn tilde(path: &Path) -> String {
    let s = path.display().to_string();
    match std::env::var("HOME") {
        Ok(h) if !h.is_empty() && s.starts_with(&h) => format!("~{}", &s[h.len()..]),
        _ => s,
    }
}

fn file_url(path: &Path) -> String {
    let mut url = String::from("file://");
    for &b in path.to_string_lossy().as_bytes() {
        if b.is_ascii_alphanumeric() || b"/-_.~".contains(&b) {
            url.push(b as char);
        } else {
            url.push_str(&format!("%{b:02X}"));
        }
    }
    url
}
