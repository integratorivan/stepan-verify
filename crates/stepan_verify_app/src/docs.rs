//! «Документы» — the shelf (prototype «Полка»): documents grouped by project or
//! by time on the left, the open document read in place in the middle, its
//! table of contents and provenance («Откуда / Дан агентам») on the right.
//! A document can be handed to an agent: an open session, a new Claude or
//! Codex session, a board task, or copied as a path.
//!
//! Comments: hover a block of a markdown document and comment on it, or
//! select text and press «Комментарий» (or c) to comment on the selection;
//! without a selection the comment is about the whole document (the only kind
//! an HTML document gets, its web view has no blocks to point at). Threads sit
//! under their block; «Отправить агенту» delivers the unsent ones to the
//! document's session. Earlier versions are kept by the scan: the header says
//! how often the document changed and shows the latest change as a diff.

use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use gpui_kit::component::{
    ActiveTheme, Icon, IconName, Sizable as _, Theme, WindowExt as _,
    h_flex,
    input::{Input, InputEvent, InputState, Textarea, TextareaState},
    skeleton::Skeleton,
    spinner::Spinner,
    text::{TextView, TextViewStyle},
    v_flex,
};
use gpui_kit::base::ElementExt as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use crate::zoom::px;
use gpui_webview::WebView;
use stepan_verify_core::{
    Home, Provider, SessionSummary,
    doc_comments::{self, Anchor, Comment, NewComment, Thread},
    doc_versions::{self, DiffLine, Version},
    docs::{self, Author, Doc, DocKind, DocType, Given, Section},
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
    /// Open (resume) this session and send it a message.
    SendToSummary { summary: SessionSummary, text: String },
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
    /// When the shown text was read: a newer file is reloaded.
    modified: chrono::DateTime<chrono::Utc>,
    /// Blocks a comment can point at, in reading order, with their section.
    /// Empty for anything but markdown.
    blocks: Vec<(usize, String)>,
    comments: Vec<Comment>,
    /// Stored versions, oldest first.
    versions: Vec<Version>,
    /// The latest change, computed when first shown.
    diff: Option<Vec<DiffLine>>,
}

impl OpenDoc {
    fn block_texts(&self) -> Vec<String> {
        self.blocks.iter().map(|(_, b)| b.clone()).collect()
    }

    /// The latest change: the previous stored version against the newest.
    fn compute_diff(&mut self) {
        if let [.., prev, last] = self.versions.as_slice() {
            let read = |v: &Version| String::from_utf8_lossy(&fs::read(&v.path).unwrap_or_default()).into_owned();
            self.diff = Some(doc_versions::line_diff(&read(prev), &read(last), 3));
        }
    }
}

/// What the comment box is writing.
#[derive(Clone, Debug, PartialEq)]
enum Compose {
    /// On block `ix`; `quote` narrows it to a selection.
    Block { ix: usize, quote: Option<String> },
    /// About the whole document.
    Doc,
    /// An answer in the thread with this root.
    Reply(String),
}

/// Where unsent comments go.
enum SendTo {
    Summary(SessionSummary),
    Live { ix: usize, title: SharedString, provider: Provider },
    New(Provider),
}

pub struct DocsView {
    home: Home,
    app: WeakEntity<AppView>,
    docs: Vec<Doc>,
    loaded: bool,
    fingerprint: u64,
    /// Document to open when the first scan lands.
    pending_open: Option<String>,
    by_time: bool,
    collapsed: HashSet<String>,
    search: Entity<InputState>,
    selected: Option<String>,
    open: Option<OpenDoc>,
    menu: bool,
    compose: Option<Compose>,
    compose_kind: doc_comments::Kind,
    composer: Entity<TextareaState>,
    /// «Отправить» / «Отправить и закрыть» asks which session (Some(resolve)).
    send_menu: Option<bool>,
    show_diff: bool,
    /// The window shows this page (the native web view must hide otherwise).
    visible: bool,
    focus: FocusHandle,
    scroll: ScrollHandle,
    _watch: Task<()>,
    _search: Subscription,
    _composer: Subscription,
}

impl DocsView {
    pub fn new(home: Home, app: WeakEntity<AppView>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Название, файл, проект"));
        let sub = cx.subscribe(&search, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx).auto_grow(2, 8).submit_on_enter(true).placeholder("Комментарий… (Enter — добавить, Shift+Enter — новая строка)")
        });
        let composer_sub = cx.subscribe_in(&composer, window, |this: &mut Self, _, event: &InputEvent, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.submit_comment(window, cx);
            }
        });
        let mut this = Self {
            home,
            app,
            docs: Vec::new(),
            loaded: false,
            fingerprint: 0,
            pending_open: None,
            by_time: false,
            collapsed: HashSet::new(),
            search,
            selected: None,
            open: None,
            menu: false,
            compose: None,
            compose_kind: doc_comments::Kind::Comment,
            composer,
            send_menu: None,
            show_diff: false,
            visible: false,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            _watch: Task::ready(()),
            _search: sub,
            _composer: composer_sub,
        };
        this.rescan(cx);
        crate::distill::observe(cx);
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

    /// Select `rel` once the shelf has loaded (`--doc`).
    pub fn open_on_load(&mut self, rel: String) {
        self.pending_open = Some(rel);
    }

    /// The document open on the shelf (or about to open, while it loads).
    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref().or(self.pending_open.as_deref())
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
                // Keep the open document fresh when its file or comments changed.
                if let Some(rel) = this.open.as_ref().map(|o| o.rel.clone()) {
                    match this.doc(&rel).cloned() {
                        None => {
                            this.open = None;
                            this.selected = None;
                        }
                        Some(doc) => this.refresh_open(&doc),
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

    /// A markdown document as sections, and the blocks comments point at.
    fn markdown_body(doc: &Doc) -> (Body, Vec<(usize, String)>) {
        match fs::read_to_string(&doc.path) {
            Ok(text) => {
                let (_, body) = docs::split_front(&text);
                let sections = docs::sections(body);
                let toc = docs::toc(&sections, &doc.title);
                let blocks = (0..sections.len()).flat_map(|i| doc_comments::blocks(docs::section_body(&sections, i)).into_iter().map(move |b| (i, b))).collect();
                (Body::Sections { sections, toc }, blocks)
            }
            Err(err) => (Body::Error(format!("Не удалось прочитать: {err}").into()), Vec::new()),
        }
    }

    /// After a rescan: newer markdown text (the scroll stays), comments, versions.
    fn refresh_open(&mut self, doc: &Doc) {
        let home = self.home.clone();
        let Some(open) = self.open.as_mut() else { return };
        if doc.modified != open.modified {
            open.modified = doc.modified;
            if doc.kind == DocKind::Markdown {
                (open.body, open.blocks) = Self::markdown_body(doc);
            }
        }
        open.comments = doc_comments::load(&home, &doc.rel);
        let versions = doc_versions::list(&home, &doc.rel);
        if versions != open.versions {
            open.versions = versions;
            open.diff = None;
            if self.show_diff {
                open.compute_diff();
            }
        }
    }

    /// Comments changed here: reload them and the shelf's counter.
    fn reload_comments(&mut self, cx: &mut Context<Self>) {
        let Some(open) = self.open.as_mut() else { return };
        open.comments = doc_comments::load(&self.home, &open.rel);
        let count = open.comments.iter().filter(|c| c.reply_to.is_none() && !c.resolved).count();
        let rel = open.rel.clone();
        if let Some(d) = self.docs.iter_mut().find(|d| d.rel == rel) {
            d.comments = count;
        }
        cx.notify();
    }

    fn select(&mut self, rel: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(doc) = self.doc(&rel).cloned() else { return };
        self.menu = false;
        self.send_menu = None;
        self.show_diff = false;
        if self.selected.as_deref() != Some(rel.as_str()) {
            self.compose = None;
        }
        self.selected = Some(rel.clone());
        let mut blocks = Vec::new();
        let body = match doc.kind {
            DocKind::Markdown => {
                let (body, b) = Self::markdown_body(&doc);
                blocks = b;
                body
            }
            DocKind::Html => {
                let builder = wry::WebViewBuilder::new().with_url(file_url(&doc.path));
                match WebView::build(builder, window, cx) {
                    Ok(web) => {
                        let web = cx.new(|_| web);
                        let _ = web.read(cx).set_visible(self.visible);
                        Body::Web(web)
                    }
                    Err(err) => {
                        eprintln!("stepan_verify: docs webview: {err}");
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
        let (comments, versions) = (doc_comments::load(&self.home, &rel), doc_versions::list(&self.home, &rel));
        self.open = Some(OpenDoc { rel, body, modified: doc.modified, blocks, comments, versions, diff: None });
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
        if self.composer.read(cx).focus_handle(cx).is_focused(window) {
            if key == "escape" {
                self.cancel_compose(window, cx);
                cx.stop_propagation();
            }
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
            "c" if self.open.is_some() => self.comment_selection(window, cx),
            "escape" if self.menu || self.send_menu.is_some() => {
                self.menu = false;
                self.send_menu = None;
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
        let cwd = self.doc_cwd(&doc, cx);
        let prompt = format!("{}\n\nПрочитай и коротко подтверди, что понял. Дальше жди задачу.", Self::context_message(&doc));
        let name = provider_name(provider);
        self.log(&doc, format!("new:{name}"), format!("новая сессия {name}"), Some(name));
        self.menu = false;
        cx.emit(DocsEvent::NewSession { provider, cwd, prompt });
        self.after_give(cx);
    }

    fn give_task(&mut self, task_id: String, task_title: String, cx: &mut Context<Self>) {
        let Some(doc) = self.open.as_ref().and_then(|o| self.doc(&o.rel)).cloned() else { return };
        let board = stepan_verify_core::Board::new(self.home.clone());
        let result = board.get(&task_id).and_then(|mut task| {
            if !task.meta.attachments.contains(&doc.path) {
                task.meta.attachments.push(doc.path.clone());
                task.save()?;
            }
            board.log(&task_id, "ты", &format!("Прикреплён документ «{}»", doc.title))
        });
        if let Err(err) = result {
            eprintln!("stepan_verify: attach doc to task: {err:#}");
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

// ------------------------------------------------------------------ comments

impl DocsView {
    fn start_compose(&mut self, compose: Compose, window: &mut Window, cx: &mut Context<Self>) {
        if self.compose.as_ref() != Some(&compose) {
            self.composer.update(cx, |c, cx| c.set_value("", window, cx));
            self.compose_kind = doc_comments::Kind::Comment;
        }
        self.compose = Some(compose);
        self.composer.update(cx, |c, cx| c.focus(window, cx));
        cx.notify();
    }

    fn cancel_compose(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.compose = None;
        self.composer.update(cx, |c, cx| c.set_value("", window, cx));
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// «Комментарий» / c: on the selected text when there is one in this
    /// document, otherwise about the whole document.
    fn comment_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selection = gpui_kit::base::TextSelection::selected_text(window, cx);
        let selection = selection.trim();
        let blocks = self.open.as_ref().map(OpenDoc::block_texts).unwrap_or_default();
        let compose = match doc_comments::locate(&blocks, selection).filter(|_| !selection.is_empty()) {
            Some(ix) => Compose::Block { ix, quote: Some(selection.to_owned()) },
            None => Compose::Doc,
        };
        self.start_compose(compose, window, cx);
    }

    fn submit_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(open), Some(compose)) = (self.open.as_ref(), self.compose.clone()) else { return };
        let text = self.composer.read(cx).value().trim().to_owned();
        let you = Author::You.label().to_owned();
        let result = match compose {
            Compose::Reply(root) => doc_comments::reply(&self.home, &open.rel, &root, &text, &you),
            Compose::Doc => doc_comments::add(&self.home, &open.rel, NewComment { text, kind: self.compose_kind, author: you, ..NewComment::default() }),
            Compose::Block { ix, quote } => {
                let blocks = open.block_texts();
                doc_comments::add(&self.home, &open.rel, NewComment { text, kind: self.compose_kind, author: you, ..NewComment::on_block(&blocks, ix, quote.as_deref()) })
            }
        };
        // Empty text: the box stays open.
        if result.is_err() {
            return;
        }
        self.cancel_compose(window, cx);
        self.reload_comments(cx);
    }

    fn set_resolved(&mut self, id: &str, resolved: bool, cx: &mut Context<Self>) {
        let Some(rel) = self.open.as_ref().map(|o| o.rel.clone()) else { return };
        if let Err(err) = doc_comments::set_resolved(&self.home, &rel, &[id.to_owned()], resolved, Author::You.label()) {
            eprintln!("stepan_verify: comments: {err:#}");
        }
        self.reload_comments(cx);
    }

    fn delete_comment(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(rel) = self.open.as_ref().map(|o| o.rel.clone()) else { return };
        if let Err(err) = doc_comments::delete(&self.home, &rel, id, Author::You.label()) {
            eprintln!("stepan_verify: comments: {err:#}");
        }
        self.reload_comments(cx);
    }

    /// Open threads holding your messages no agent has seen yet.
    fn unsent(open: &OpenDoc) -> Vec<Thread> {
        doc_comments::threads(&open.comments).into_iter().filter(|t| !t.root.resolved && !t.unsent().is_empty()).collect()
    }

    /// Where «Отправить» goes without asking: the document's own session,
    /// else a new session when no session of its project is open (None: pick).
    fn send_target(&self, doc: &Doc, cx: &App) -> Option<SendTo> {
        if let Some(s) = doc.session.as_ref().and_then(|id| self.session_by_id(id, cx)) {
            return Some(SendTo::Summary(s));
        }
        if self.project_sessions(doc, cx).is_empty() {
            return Some(SendTo::New(crate::settings::config(cx).agents.default_provider.into()));
        }
        None
    }

    fn project_sessions(&self, doc: &Doc, cx: &App) -> Vec<LiveSession> {
        let live: Vec<LiveSession> = self.app.upgrade().map(|a| a.read(cx).live_sessions(cx)).unwrap_or_default();
        live.into_iter().filter(|s| doc.project.as_deref() == Some(s.project.as_ref())).collect()
    }

    /// Where a new session for this document works: its session's project,
    /// else the newest session of its project, else the docs folder.
    fn doc_cwd(&self, doc: &Doc, cx: &App) -> PathBuf {
        let sessions = self.app.upgrade().map(|a| a.read(cx).sessions.clone()).unwrap_or_default();
        doc.session
            .as_ref()
            .and_then(|id| sessions.iter().find(|s| &s.id == id))
            .or_else(|| doc.project.as_ref().and_then(|p| sessions.iter().find(|s| &s.project() == p)))
            .and_then(|s| s.cwd.clone())
            .filter(|c| c.is_dir())
            .unwrap_or_else(|| self.home.docs())
    }

    /// Deliver the unsent threads; `resolve` closes them as they go.
    fn send_comments(&mut self, to: SendTo, resolve: bool, cx: &mut Context<Self>) {
        self.send_menu = None;
        let Some(doc) = self.open.as_ref().and_then(|o| self.doc(&o.rel)).cloned() else { return };
        let Some(open) = self.open.as_ref() else { return };
        let threads = Self::unsent(open);
        if threads.is_empty() {
            return;
        }
        let refs: Vec<&Thread> = threads.iter().collect();
        let text = doc_comments::message(&doc.title, &doc.path, &refs, &open.block_texts(), resolve);
        // `session`: what the comments remember; `target`: the «Дан агентам» entry.
        let (session, target, title, agent, event) = match to {
            SendTo::Summary(s) => (s.id.clone(), format!("session:{}", s.id), s.title.clone(), provider_name(s.provider), DocsEvent::SendToSummary { summary: s, text }),
            SendTo::Live { ix, title, provider } => {
                let native = self.app.upgrade().and_then(|app| app.read(cx).open_sessions.get(ix).and_then(|v| v.read(cx).native_id.clone()));
                let session = native.unwrap_or_else(|| format!("new:{}", provider_name(provider)));
                (session.clone(), format!("session:{session}"), title.to_string(), provider_name(provider), DocsEvent::SendToSession { ix, text })
            }
            SendTo::New(provider) => {
                let name = provider_name(provider);
                (format!("new:{name}"), format!("new:{name}"), format!("новая сессия {name}"), name, DocsEvent::NewSession { provider, cwd: self.doc_cwd(&doc, cx), prompt: text })
            }
        };
        let ids: Vec<String> = threads.iter().flat_map(|t| t.unsent().into_iter().map(|c| c.id.clone())).collect();
        let mut result = doc_comments::mark_sent(&self.home, &doc.rel, &ids, &session);
        if resolve {
            let roots: Vec<String> = threads.iter().map(|t| t.root.id.clone()).collect();
            result = result.and(doc_comments::set_resolved(&self.home, &doc.rel, &roots, true, Author::You.label()).map(|_| ()));
        }
        if let Err(err) = result {
            eprintln!("stepan_verify: comments: {err:#}");
        }
        self.log(&doc, target, format!("комментарии → {title}"), Some(agent));
        cx.emit(event);
        self.after_give(cx);
        self.reload_comments(cx);
    }

    fn toggle_diff(&mut self, cx: &mut Context<Self>) {
        self.show_diff = !self.show_diff;
        if self.show_diff {
            stepan_verify_core::usage::hit("docs.versions");
        }
        if let Some(open) = self.open.as_mut().filter(|o| o.diff.is_none()) {
            open.compute_diff();
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
        // `--doc` wins over the first-visit default.
        if self.loaded {
            if let Some(rel) = self.pending_open.take() {
                self.selected = Some(rel.clone());
                cx.defer_in(window, move |this, window, cx| this.select(rel, window, cx));
            }
        }
        // Open something on first visit: the shelf is for reading.
        if self.loaded && self.open.is_none() && self.selected.is_none() {
            if let Some(first) = self.walkable(cx).into_iter().next() {
                self.selected = Some(first.clone());
                cx.defer_in(window, move |this, window, cx| this.select(first, window, cx));
            }
        }
        // The native web view sits above GPUI: step aside for menus and dialogs.
        if let Some(OpenDoc { body: Body::Web(web), .. }) = &self.open {
            let show = self.visible && !self.menu && self.send_menu.is_none() && !window.has_active_dialog(cx) && !window.has_active_sheet(cx);
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
                                    )
                                    .when_some(writing_line(cx), |d, line| {
                                        d.child(
                                            h_flex()
                                                .min_w_0()
                                                .gap(px(6.))
                                                .text_size(px(11.5))
                                                .text_color(p.muted)
                                                .child(Spinner::new().xsmall().color(p.faint))
                                                .child(div().min_w_0().truncate().child(line)),
                                        )
                                    }),
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
            .child(kind_box(d, p))
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
                            .when(d.doc_type != DocType::Note, |row| row.child(type_label(d, p, theme)))
                            .when(d.open_question().is_some(), |row| row.child(question_chip(p, theme)))
                            .child(div().font_family(mono.clone()).text_size(px(11.)).text_color(p.faint).child(docs::short_time(d.modified, chrono::Local::now())))
                            .child(div().flex_1())
                            .when(d.comments > 0, |row| {
                                row.child(
                                    h_flex()
                                        .gap(px(3.))
                                        .font_family(mono.clone())
                                        .text_size(px(11.))
                                        .text_color(p.accent.text)
                                        .child(Icon::empty().path("icons/message-square.svg").xsmall())
                                        .child(d.comments.to_string()),
                                )
                            })
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

        // Every thread and where it sits in the current text.
        let texts = open.block_texts();
        let placed: Vec<(Thread, Anchor)> = doc_comments::threads(&open.comments)
            .into_iter()
            .map(|t| {
                let anchor = doc_comments::anchor(&texts, &t.root);
                (t, anchor)
            })
            .collect();
        let header = self.render_doc_header(&doc, open, &placed, p, theme, cx);
        let center: AnyElement = match &open.body {
            Body::Sections { sections, .. } => {
                let scroll = self.scroll.clone();
                // One child per section (the table of contents scrolls to them), one row per block inside.
                let mut section_els: Vec<AnyElement> = Vec::with_capacity(sections.len());
                for i in 0..sections.len() {
                    let mut rows: Vec<AnyElement> = Vec::new();
                    for (ix, (section, text)) in open.blocks.iter().enumerate() {
                        if *section != i {
                            continue;
                        }
                        let tight = doc_comments::list_item(text) && open.blocks.get(ix + 1).is_some_and(|(_, next)| doc_comments::list_item(next));
                        let here: Vec<&(Thread, Anchor)> = placed.iter().filter(|(_, a)| matches!(a, Anchor::Block { ix: at, .. } if *at == ix)).collect();
                        rows.push(self.render_block(ix, text, tight, &here, &doc.rel, p, theme, cx));
                    }
                    section_els.push(v_flex().text_size(px(14.5)).children(rows).into_any_element());
                }
                v_flex()
                    .id("doc-scroll")
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .overflow_y_scroll()
                    .track_scroll(&scroll)
                    .on_scroll(cx.listener(|_, _, _, cx| cx.notify()))
                    .child(header)
                    .children(section_els)
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

    fn render_doc_header(&self, doc: &Doc, open: &OpenDoc, placed: &[(Thread, Anchor)], p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let mono = theme.mono_font_family.clone();
        let source = doc.session.as_ref().and_then(|id| self.session_by_id(id, cx));
        let path = doc.path.clone();
        let pin_rel = doc.rel.clone();
        // The first stored version is the state before any change.
        let changes = open.versions.len().saturating_sub(1);
        let last_change = open.versions.last().map(|v| v.at);
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
                    .when(doc.doc_type != DocType::Note, |d| d.child(type_label(doc, p, theme)))
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
                    .when(doc.kind.readable(), |d| d.child(div().text_color(p.faint).child("·")).child(format!("{} мин чтения", doc.read_minutes())))
                    .when_some(last_change.filter(|_| changes > 0), |d, at| {
                        d.child(div().text_color(p.faint).child("·")).child(
                            div()
                                .id("doc-versions")
                                .cursor_pointer()
                                .underline()
                                .text_color(if self.show_diff { p.ink } else { p.fg })
                                .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new("Показать последнюю правку").build(window, cx))
                                .on_click(cx.listener(|this, _, _, cx| this.toggle_diff(cx)))
                                .child(format!("изменён {changes} {} · последняя правка {}", times_word(changes), full_time(at))),
                        )
                    }),
            )
            .when_some(doc.summary.clone().filter(|s| !s.is_empty()), |d, s| d.child(div().text_size(px(13.5)).text_color(p.muted).child(s)))
            .when_some(self.render_question(doc, source.clone(), p, cx), |d, q| d.child(q))
            .when(doc.doc_type == DocType::Decision && doc.kind == DocKind::Markdown, |d| d.child(self.render_decision_bar(doc, p, theme, cx)))
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
                        PressButton::new("doc-comment")
                            .small()
                            .icon(Icon::empty().path("icons/message-square-plus.svg"))
                            .label("Комментарий")
                            .tooltip(if doc.kind == DocKind::Markdown { "К выделенному тексту, без выделения — ко всему документу · c" } else { "Ко всему документу · c" })
                            .on_click(cx.listener(|this, _, window, cx| this.comment_selection(window, cx))),
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
                    .when(self.menu, |d| d.child(deferred(self.render_menu(doc, p, theme, cx)).with_priority(2)))
                    // «Комментарий» reads the selection when clicked: pressing it must not clear it.
                    .keep_text_selection("doc-actions"),
            )
            .when_some(self.render_diff(open, p, theme, cx), |d, diff| d.child(diff))
            .when_some(self.render_send_bar(doc, open, p, theme, cx), |d, bar| d.child(bar))
            .when_some(self.render_doc_comments(placed, p, theme, cx), |d, list| d.child(list))
            .into_any_element()
    }

    /// Rewrite one frontmatter field of the open document; it stays read.
    fn set_field(&mut self, rel: &str, key: &'static str, value: &'static str, cx: &mut Context<Self>) {
        let Some(doc) = self.doc(rel).cloned() else { return };
        if let Err(err) = docs::set_front_field(&doc.path, key, value) {
            eprintln!("stepan_verify: docs: {}: {err}", doc.rel);
            return;
        }
        if let Ok(modified) = fs::metadata(&doc.path).and_then(|m| m.modified()) {
            docs::mark_read(&self.home, &Doc { modified: modified.into(), ..doc });
        }
        self.rescan(cx);
    }

    /// «Агент спрашивает»: the document's open question, answered in its session.
    fn render_question(&self, doc: &Doc, source: Option<SessionSummary>, p: &Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let question = doc.open_question()?.to_owned();
        let rel = doc.rel.clone();
        let editable = doc.kind == DocKind::Markdown;
        Some(
            h_flex()
                .mt(px(4.))
                .p(px(14.))
                .gap(px(12.))
                .items_start()
                .rounded(px(8.))
                .border_1()
                .border_color(ui::mix(p.amber.solid, p.border, 0.45))
                .bg(p.amber.soft)
                .child(Icon::new(IconName::Info).small().text_color(p.amber.text).mt(px(2.)))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap(px(4.))
                        .child(div().text_size(px(12.5)).font_weight(FontWeight::SEMIBOLD).text_color(p.amber.text).child("Агент спрашивает"))
                        .child(div().text_size(px(14.)).line_height(px(21.)).text_color(p.ink).child(question)),
                )
                .child(
                    v_flex()
                        .flex_shrink_0()
                        .gap(px(6.))
                        .items_end()
                        .when_some(source, |d, s| {
                            d.child(
                                PressButton::new("doc-answer")
                                    .small()
                                    .label("Ответить в сессии")
                                    .on_click(cx.listener(move |_, _, _, cx| cx.emit(DocsEvent::OpenSession(s.clone())))),
                            )
                        })
                        .when(editable, |d| {
                            d.child(
                                PressButton::new("doc-answered")
                                    .small()
                                    .ghost()
                                    .icon(IconName::Check)
                                    .label("Отвечено")
                                    .tooltip("Пометить вопрос закрытым (status: answered)")
                                    .on_click(cx.listener(move |this, _, _, cx| this.set_field(&rel, "status", "answered", cx))),
                            )
                        }),
                )
                .into_any_element(),
        )
    }

    /// A proposed decision can be accepted or rejected right here.
    fn render_decision_bar(&self, doc: &Doc, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let status = doc.status.as_deref().map(|s| s.trim().to_lowercase()).unwrap_or_default();
        if !matches!(status.as_str(), "proposed" | "предложено" | "") {
            return div().into_any_element();
        }
        let (accept, reject) = (doc.rel.clone(), doc.rel.clone());
        h_flex()
            .mt(px(4.))
            .px(px(12.))
            .py(px(8.))
            .gap(px(10.))
            .rounded(px(8.))
            .border_1()
            .border_color(p.border)
            .bg(p.surface)
            .child(ui::mono("решение предложено", theme).text_color(p.muted))
            .child(div().flex_1())
            .child(
                PressButton::new("doc-accept")
                    .small()
                    .icon(IconName::Check)
                    .label("Принять")
                    .on_click(cx.listener(move |this, _, _, cx| this.set_field(&accept, "status", "accepted", cx))),
            )
            .child(
                PressButton::new("doc-reject")
                    .small()
                    .ghost()
                    .label("Отклонить")
                    .on_click(cx.listener(move |this, _, _, cx| this.set_field(&reject, "status", "rejected", cx))),
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
                for t in all.iter().filter(|t| matches!(t.meta.status, stepan_verify_core::TaskStatus::InProgress | stepan_verify_core::TaskStatus::Todo | stepan_verify_core::TaskStatus::Blocked)) {
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
                    "Агенты кладут отчёты, спеки и HTML-разборы в ~/.stepan_verify/docs — каждая сессия видит эту папку как $STEPAN_VERIFY_DOCS. Подпапка — это проект; документ появится здесь сам, как только файл будет записан.",
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
                .child(div().text_size(px(12.5)).text_color(p.muted).child("Попроси агента: «запиши разбор в $STEPAN_VERIFY_DOCS/<проект>/<тема>.md с фронтматтером»."))
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

// ------------------------------------------------------------------ render: comments

impl DocsView {
    /// One block of a markdown document: its text, a comment button in the
    /// left margin on hover, the threads on it and the comment box.
    #[allow(clippy::too_many_arguments)]
    fn render_block(&self, ix: usize, text: &str, tight: bool, threads: &[&(Thread, Anchor)], rel: &str, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let composing = match &self.compose {
            Some(Compose::Block { ix: at, quote }) if *at == ix => Some(quote.clone()),
            _ => None,
        };
        let heading = text.starts_with('#');
        v_flex()
            .id(SharedString::from(format!("doc-block:{ix}")))
            .group("doc-block")
            .relative()
            .px(px(48.))
            .max_w(px(820.))
            .pb(px(if tight { 2. } else { 10. }))
            .when(heading, |d| d.pt(px(8.)))
            .child(TextView::markdown(SharedString::from(format!("{rel}#b{ix}")), text.to_owned()).style(reader_style()).selectable(true))
            .child(
                div()
                    .absolute()
                    .left(px(16.))
                    .top(px(if heading { 8. } else { 0. }))
                    .when(composing.is_none(), |d| d.opacity(0.).group_hover("doc-block", |s| s.opacity(1.)))
                    .child(
                        PressButton::new(SharedString::from(format!("doc-block-comment:{ix}")))
                            .ghost()
                            .xsmall()
                            .icon(Icon::empty().path("icons/message-square-plus.svg"))
                            .tooltip("Комментарий к этому блоку")
                            .on_click(cx.listener(move |this, _, window, cx| this.start_compose(Compose::Block { ix, quote: None }, window, cx))),
                    ),
            )
            .children(threads.iter().map(|(t, a)| self.render_thread(t, *a, Some(text), p, theme, cx)))
            .when_some(composing, |d, quote| d.child(self.render_composer(false, quote.as_deref(), p, theme, cx)))
            .into_any_element()
    }

    /// A thread: the quote when it differs from its block, the messages,
    /// then reply / close / delete. A closed thread folds into one line.
    fn render_thread(&self, t: &Thread, anchor: Anchor, block: Option<&str>, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let root = &t.root;
        let id = root.id.clone();
        let eid = |what: &str| SharedString::from(format!("comment-{what}:{id}"));
        if root.resolved {
            let reopen = id.clone();
            let summary = if root.text.is_empty() { root.kind.label().to_owned() } else { root.text.clone() };
            return h_flex()
                .id(eid("thread"))
                .mb(px(8.))
                .gap(px(8.))
                .text_size(px(12.))
                .text_color(p.faint)
                .child(Icon::new(IconName::Check).xsmall())
                .child(div().flex_1().min_w_0().truncate().child(format!("закрыт · {summary}")))
                .child(
                    PressButton::new(eid("reopen"))
                        .ghost()
                        .xsmall()
                        .label("Открыть снова")
                        .on_click(cx.listener(move |this, _, _, cx| this.set_resolved(&reopen, false, cx))),
                )
                .into_any_element();
        }
        let squash = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
        let quote = (!root.quote.trim().is_empty() && block.is_none_or(|b| squash(b) != squash(&root.quote))).then(|| root.quote.clone());
        let note = match anchor {
            Anchor::Block { moved: true, .. } => Some("текст под комментарием изменился"),
            _ => None,
        };
        let replying = self.compose.as_ref() == Some(&Compose::Reply(id.clone()));
        let (reply, resolve, delete) = (id.clone(), id.clone(), id.clone());
        v_flex()
            .id(eid("thread"))
            .mt(px(2.))
            .mb(px(10.))
            .p(px(10.))
            .gap(px(8.))
            .rounded(px(8.))
            .border_1()
            .border_color(p.border)
            .bg(p.surface)
            .when_some(note, |d, n| d.child(div().text_size(px(11.5)).text_color(p.amber.text).child(n)))
            .when_some(quote, |d, q| {
                let stale = anchor == Anchor::Stale;
                d.child(
                    div()
                        .pl(px(8.))
                        .border_l_2()
                        .border_color(p.border_strong)
                        .text_size(px(12.5))
                        .text_color(p.muted)
                        .when(stale, |d| d.line_through())
                        .child(short(&q, 400)),
                )
            })
            .children(t.messages().map(|m| render_message(m, m.id == root.id, p, theme)))
            .when(replying, |d| d.child(self.render_composer(true, None, p, theme, cx)))
            .child(
                h_flex()
                    .gap(px(2.))
                    .when(!replying, |d| {
                        d.child(
                            PressButton::new(eid("reply"))
                                .ghost()
                                .xsmall()
                                .icon(Icon::empty().path("icons/reply.svg"))
                                .label("Ответить")
                                .on_click(cx.listener(move |this, _, window, cx| this.start_compose(Compose::Reply(reply.clone()), window, cx))),
                        )
                    })
                    .child(
                        PressButton::new(eid("resolve"))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Check)
                            .label("Закрыть")
                            .on_click(cx.listener(move |this, _, _, cx| this.set_resolved(&resolve, true, cx))),
                    )
                    .child(div().flex_1())
                    .child(
                        PressButton::new(eid("delete"))
                            .ghost()
                            .xsmall()
                            .icon(Icon::empty().path("icons/trash.svg"))
                            .tooltip("Удалить тред")
                            .on_click(cx.listener(move |this, _, _, cx| this.delete_comment(&delete, cx))),
                    ),
            )
            .into_any_element()
    }

    /// The comment box: kind (not for a reply), text, add / cancel.
    fn render_composer(&self, reply: bool, quote: Option<&str>, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        use doc_comments::Kind;
        let mut kinds = h_flex().p(px(2.)).gap(px(2.)).rounded(px(8.)).border_1().border_color(p.border).bg(p.sunken);
        for k in [Kind::Comment, Kind::Approval, Kind::Deletion] {
            let active = self.compose_kind == k;
            kinds = kinds.child(
                h_flex()
                    .id(SharedString::from(format!("comment-kind:{}", k.key())))
                    .h(px(24.))
                    .px(px(9.))
                    .rounded(px(6.))
                    .border_1()
                    .border_color(transparent_black())
                    .text_size(px(12.))
                    .cursor_pointer()
                    .map(|d| {
                        if active {
                            d.bg(p.surface).border_color(p.border).shadow(ui::edge_shadow(p.border_strong, 1.)).text_color(p.ink).font_weight(FontWeight::SEMIBOLD)
                        } else {
                            d.text_color(p.muted).hover(|s| s.bg(p.ink.opacity(0.04)))
                        }
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.compose_kind = k;
                        cx.notify();
                    }))
                    .child(k.label()),
            );
        }
        v_flex()
            .mt(px(2.))
            .mb(px(10.))
            .p(px(10.))
            .gap(px(8.))
            .rounded(px(8.))
            .border_1()
            .border_color(p.border_strong)
            .bg(p.surface)
            .when_some(quote, |d, q| {
                d.child(div().pl(px(8.)).border_l_2().border_color(p.accent.solid).text_size(px(12.5)).text_color(p.muted).child(short(q, 400)))
            })
            .when(!reply, |d| d.child(h_flex().child(kinds)))
            .child(Textarea::new(&self.composer))
            .child(
                h_flex()
                    .gap(px(8.))
                    .child(
                        PressButton::new("comment-submit")
                            .primary()
                            .small()
                            .label(if reply { "Ответить" } else { "Добавить" })
                            .on_click(cx.listener(|this, _, window, cx| this.submit_comment(window, cx))),
                    )
                    .child(PressButton::new("comment-cancel").ghost().small().label("Отмена").on_click(cx.listener(|this, _, window, cx| this.cancel_compose(window, cx))))
                    .child(div().flex_1())
                    .child(ui::mono("Esc — отмена", theme).text_size(px(11.)).text_color(p.faint)),
            )
            .into_any_element()
    }

    /// Comments about the whole document, and stale ones whose quote is gone.
    fn render_doc_comments(&self, placed: &[(Thread, Anchor)], p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let doc_level: Vec<&(Thread, Anchor)> = placed.iter().filter(|(_, a)| *a == Anchor::Doc).collect();
        let stale: Vec<&(Thread, Anchor)> = placed.iter().filter(|(_, a)| *a == Anchor::Stale).collect();
        let composing = self.compose == Some(Compose::Doc);
        if doc_level.is_empty() && stale.is_empty() && !composing {
            return None;
        }
        Some(
            v_flex()
                .mt(px(4.))
                .when(!doc_level.is_empty() || composing, |d| d.child(ui::eyebrow("комментарии к документу", p, theme).mb(px(6.))))
                .children(doc_level.iter().map(|(t, a)| self.render_thread(t, *a, None, p, theme, cx)))
                .when(composing, |d| d.child(self.render_composer(false, None, p, theme, cx)))
                .when(!stale.is_empty(), |d| {
                    d.child(ui::eyebrow("устарели — цитаты больше нет в тексте", p, theme).mt(px(4.)).mb(px(6.)))
                        .children(stale.iter().map(|(t, a)| self.render_thread(t, *a, None, p, theme, cx)))
                })
                .into_any_element(),
        )
    }

    /// «N комментариев не отправлены — в сессию «…»» with Отправить / Отправить и закрыть.
    fn render_send_bar(&self, doc: &Doc, open: &OpenDoc, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let n = Self::unsent(open).len();
        if n == 0 {
            return None;
        }
        let target = match self.send_target(doc, cx) {
            Some(SendTo::Summary(s)) => format!("в сессию «{}»", short(&s.title, 40)),
            Some(SendTo::New(provider)) => format!("в новую сессию {}", provider.label()),
            _ => "в одну из открытых сессий проекта".to_owned(),
        };
        let (send_doc, close_doc) = (doc.clone(), doc.clone());
        Some(
            v_flex()
                .relative()
                .mt(px(4.))
                .px(px(12.))
                .py(px(10.))
                .gap(px(8.))
                .rounded(px(8.))
                .border_1()
                .border_color(p.border)
                .bg(p.surface)
                .child(
                    h_flex()
                        .gap(px(8.))
                        .child(Icon::empty().path("icons/message-square.svg").small().text_color(p.muted))
                        .child(div().flex_1().min_w_0().truncate().text_size(px(12.5)).text_color(p.fg).child(format!("{n} {} — {target}", unsent_word(n)))),
                )
                .child(
                    h_flex()
                        .gap(px(8.))
                        .child(
                    PressButton::new("doc-send")
                        .primary()
                        .small()
                        .icon(Icon::empty().path("icons/send.svg"))
                        .label("Отправить агенту")
                        .on_click(cx.listener(move |this, _, _, cx| this.send_or_ask(&send_doc, false, cx))),
                )
                .child(
                    PressButton::new("doc-send-close")
                        .small()
                        .ghost()
                        .label("Отправить и закрыть")
                        .tooltip("Агент получит их уже закрытыми")
                        .on_click(cx.listener(move |this, _, _, cx| this.send_or_ask(&close_doc, true, cx))),
                        ),
                )
                .when_some(self.send_menu, |d, resolve| d.child(deferred(self.render_send_menu(doc, resolve, p, theme, cx)).with_priority(2)))
                .into_any_element(),
        )
    }

    /// Send where the comments obviously go, or ask which session.
    fn send_or_ask(&mut self, doc: &Doc, resolve: bool, cx: &mut Context<Self>) {
        match self.send_target(doc, cx) {
            Some(to) => self.send_comments(to, resolve, cx),
            None => {
                self.send_menu = if self.send_menu == Some(resolve) { None } else { Some(resolve) };
                cx.notify();
            }
        }
    }

    /// Which session gets the comments when the document's own is unknown.
    fn render_send_menu(&self, doc: &Doc, resolve: bool, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let mono = theme.mono_font_family.clone();
        let item = |id: SharedString| h_flex().id(id).px(px(10.)).py(px(7.)).gap(px(10.)).rounded(px(6.)).cursor_pointer().hover(|s| s.bg(p.surface2));
        v_flex()
            .id("doc-send-menu")
            .absolute()
            .top(relative(1.)).mt(px(4.))
            .right_0()
            .w(px(320.))
            .p(px(4.))
            .rounded(px(10.))
            .border_1()
            .border_color(p.border)
            .bg(p.surface)
            .shadow(ui::popover_shadow())
            .occlude()
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.send_menu = None;
                cx.notify();
            }))
            .child(div().px(px(10.)).pt(px(8.)).pb(px(4.)).child(ui::eyebrow("открытые сессии проекта", p, theme)))
            .children(self.project_sessions(doc, cx).into_iter().map(|s| {
                let (ix, title, provider) = (s.ix, s.title.clone(), s.provider);
                item(SharedString::from(format!("send-live:{}", s.ix)))
                    .on_click(cx.listener(move |this, _, _, cx| this.send_comments(SendTo::Live { ix, title: title.clone(), provider }, resolve, cx)))
                    .child(ui::provider_chip(s.provider, p, theme))
                    .child(
                        v_flex()
                            .min_w_0()
                            .flex_1()
                            .child(div().truncate().text_size(px(13.)).text_color(p.ink).child(s.title.clone()))
                            .child(div().truncate().font_family(mono.clone()).text_size(px(10.5)).text_color(p.faint).child(s.card.progress.to_lowercase())),
                    )
            }))
            .child(div().my(px(4.)).h(px(1.)).bg(p.border))
            .children([Provider::Claude, Provider::Codex].into_iter().map(|provider| {
                item(SharedString::from(format!("send-new:{}", provider_name(provider))))
                    .on_click(cx.listener(move |this, _, _, cx| this.send_comments(SendTo::New(provider), resolve, cx)))
                    .child(Icon::new(IconName::Plus).xsmall().text_color(p.muted))
                    .child(div().text_size(px(13.)).text_color(p.ink).child(format!("Новая сессия {}", provider.label())))
            }))
    }

    /// The latest change: previous version → current, line by line.
    fn render_diff(&self, open: &OpenDoc, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.show_diff {
            return None;
        }
        let lines = open.diff.as_ref()?;
        let at = open.versions.last()?.at;
        let added = lines.iter().filter(|l| matches!(l, DiffLine::Added(_))).count();
        let removed = lines.iter().filter(|l| matches!(l, DiffLine::Removed(_))).count();
        const SHOWN: usize = 600;
        Some(
            v_flex()
                .id("doc-diff")
                .mt(px(4.))
                .rounded(px(8.))
                .border_1()
                .border_color(p.border)
                .bg(p.surface)
                .overflow_hidden()
                .child(
                    h_flex()
                        .px(px(12.))
                        .py(px(6.))
                        .gap(px(8.))
                        .border_b_1()
                        .border_color(p.border)
                        .text_size(px(12.5))
                        .text_color(p.fg)
                        .child(div().flex_1().min_w_0().truncate().child(format!("Последняя правка · {}", full_time(at))))
                        .child(ui::mono(format!("+{added}"), theme).text_size(px(11.5)).text_color(p.green.text))
                        .child(ui::mono(format!("−{removed}"), theme).text_size(px(11.5)).text_color(p.red.text))
                        .child(PressButton::new("doc-diff-close").ghost().xsmall().icon(IconName::Close).tooltip("Скрыть").on_click(cx.listener(|this, _, _, cx| this.toggle_diff(cx)))),
                )
                .child(
                    v_flex()
                        .py(px(4.))
                        .font_family(theme.mono_font_family.clone())
                        .text_size(px(12.))
                        .line_height(px(18.))
                        .when(lines.is_empty(), |d| d.child(div().px(px(12.)).text_color(p.faint).child("Текст не менялся")))
                        .children(lines.iter().take(SHOWN).map(|l| match l {
                            DiffLine::Added(t) => div().px(px(12.)).bg(p.green.soft).text_color(p.green.text).child(format!("+ {t}")),
                            DiffLine::Removed(t) => div().px(px(12.)).bg(p.red.soft).text_color(p.red.text).child(format!("− {t}")),
                            DiffLine::Same(t) => div().px(px(12.)).text_color(p.muted).child(format!("  {t}")),
                            DiffLine::Skipped(n) => div().px(px(12.)).py(px(2.)).text_color(p.faint).child(format!("⋯ {n} {} без изменений", lines_word(*n))),
                        }))
                        .when(lines.len() > SHOWN, |d| d.child(div().px(px(12.)).text_color(p.faint).child(format!("…и ещё {} строк", lines.len() - SHOWN)))),
                )
                .into_any_element(),
        )
    }
}

fn render_message(m: &Comment, root: bool, p: &Palette, theme: &Theme) -> impl IntoElement {
    use doc_comments::Kind;
    let who: AnyElement = match Author::parse(&m.author) {
        Some(a) => author_chip(a, p, theme),
        None => ui::chip(m.author.clone(), p, theme).into_any_element(),
    };
    let kind: Option<AnyElement> = match m.kind {
        Kind::Approval if root => Some(ui::chip("одобряю", p, theme).text_color(p.green.text).border_color(ui::mix(p.green.solid, p.border, 0.4)).bg(p.green.soft).into_any_element()),
        Kind::Deletion if root => Some(ui::chip("убрать это", p, theme).text_color(p.red.text).border_color(ui::mix(p.red.solid, p.border, 0.4)).bg(p.red.soft).into_any_element()),
        _ => None,
    };
    v_flex()
        .gap(px(3.))
        .child(
            h_flex()
                .gap(px(6.))
                .child(who)
                .children(kind)
                .child(ui::mono(ago(m.created_at), theme).text_size(px(11.)).text_color(p.faint))
                .when(m.sent_to.is_some(), |d| d.child(ui::mono("↗ отправлен агенту", theme).text_size(px(11.)).text_color(p.faint))),
        )
        .when(!m.text.is_empty(), |d| d.child(div().text_size(px(13.5)).line_height(px(20.)).text_color(p.ink).child(m.text.clone())))
}

/// "1 комментарий не отправлен", "3 комментария не отправлены".
fn unsent_word(n: usize) -> &'static str {
    match (n % 10, n % 100) {
        (1, r) if r != 11 => "комментарий не отправлен",
        (2..=4, r) if !(12..=14).contains(&r) => "комментария не отправлены",
        _ => "комментариев не отправлены",
    }
}

/// "1 раз", "3 раза", "5 раз".
fn times_word(n: usize) -> &'static str {
    match (n % 10, n % 100) {
        (2..=4, r) if !(12..=14).contains(&r) => "раза",
        _ => "раз",
    }
}

/// "1 строка", "3 строки", "5 строк".
fn lines_word(n: usize) -> &'static str {
    match (n % 10, n % 100) {
        (1, r) if r != 11 => "строка",
        (2..=4, r) if !(12..=14).contains(&r) => "строки",
        _ => "строк",
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

fn kind_box(d: &Doc, p: &Palette) -> impl IntoElement {
    let (icon, color) = match (d.doc_type, d.kind) {
        (DocType::Facts, _) => (IconName::BookOpen, p.accent.solid),
        (DocType::Decision, _) => (IconName::CircleCheck, if d.live_decision() { p.green.solid } else { p.faint }),
        (DocType::Research, _) => (IconName::Search, p.muted),
        (DocType::Handoff, _) => (IconName::ArrowRight, p.muted),
        (DocType::Briefing, _) => (IconName::Calendar, p.muted),
        (_, DocKind::Html) => (IconName::Globe, p.blue.solid),
        (_, DocKind::Markdown | DocKind::Text) => (IconName::FileText, p.muted),
        _ => (IconName::File, p.muted),
    };
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

/// "записываю итоги «…»" while the distiller runs.
fn writing_line(cx: &App) -> Option<String> {
    match crate::distill::running(cx).as_slice() {
        [] => None,
        [one] => Some(format!("записываю итоги «{}»…", short(one, 36))),
        many => Some(format!("записываю итоги {} сессий…", many.len())),
    }
}

/// "решение · принято", "фактура", "разбор" — mono, quiet.
fn type_label(d: &Doc, p: &Palette, theme: &Theme) -> impl IntoElement {
    let status = d.status.as_deref().and_then(docs::status_label).filter(|_| d.doc_type == DocType::Decision);
    let text = match status {
        Some(s) => format!("{} · {s}", d.doc_type.label()),
        None => d.doc_type.label().to_owned(),
    };
    let retired = d.doc_type == DocType::Decision && !d.live_decision();
    ui::mono(text, theme).text_size(px(11.)).text_color(if retired { p.faint } else { p.muted }).when(retired, |t| t.line_through())
}

fn question_chip(p: &Palette, theme: &Theme) -> impl IntoElement {
    ui::chip("вопрос", p, theme).text_color(p.amber.text).border_color(ui::mix(p.amber.solid, p.border, 0.4)).bg(p.amber.soft)
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
