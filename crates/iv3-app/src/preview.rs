//! Right-side file preview: HTML/PDF in a real WKWebView, markdown, images,
//! read-only highlighted text, folder listings.

use std::{
    io::Read as _,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use gpui_kit::component::{
    ActiveTheme, Icon, IconName, Sizable as _, WindowExt as _,
    input::{Editor, EditorState},
    text::TextView,
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use crate::ui::PressButton;
use gpui_webview::WebView;

use crate::ui::{self, Palette};

/// Text files are cut here; the editor stays responsive and the note says so.
const TEXT_LIMIT: u64 = 400 * 1024;
const FOLDER_LIMIT: usize = 2000;
/// How often the open file is checked for changes on disk.
const POLL: Duration = Duration::from_millis(1500);

/// The window that shows previews, so any view can open a file in-app.
pub struct Opener(pub WeakEntity<crate::app::AppView>);

impl Global for Opener {}

/// Show a file or folder in the right-hand preview panel. Web links go to the browser.
pub fn open_in_app(path: impl Into<PathBuf>, root: Option<PathBuf>, window: &mut Window, cx: &mut App) {
    let path = path.into();
    let text = path.to_string_lossy();
    if text.starts_with("http://") || text.starts_with("https://") {
        cx.open_url(&text);
        return;
    }
    match cx.try_global::<Opener>().and_then(|o| o.0.upgrade()) {
        Some(app) => app.update(cx, |app, cx| app.open_preview(path, root, window, cx)),
        None => run_open(&[path.as_os_str()]),
    }
}

pub enum PreviewEvent {
    Close,
}

impl EventEmitter<PreviewEvent> for PreviewView {}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Web,
    Markdown,
    Image,
    Text,
    Folder,
}

impl Kind {
    fn of(path: &Path) -> Self {
        if path.is_dir() {
            return Self::Folder;
        }
        let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        match ext.as_str() {
            // WKWebView plays video files with its own controls.
            "html" | "htm" | "xhtml" | "pdf" | "mp4" | "webm" | "mov" | "m4v" => Self::Web,
            "md" | "markdown" | "mdx" => Self::Markdown,
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tif" | "tiff" | "ico" | "avif" | "svg" => Self::Image,
            _ => Self::Text,
        }
    }

    fn icon(self) -> IconName {
        match self {
            Self::Web => IconName::Globe,
            Self::Markdown => IconName::FileText,
            Self::Folder => IconName::Folder,
            _ => IconName::File,
        }
    }
}

struct Entry {
    path: PathBuf,
    name: SharedString,
    dir: bool,
}

enum Content {
    Web(Entity<WebView>),
    /// WebView could not start: gpui-kit's own HTML renderer instead.
    HtmlFallback { text: SharedString, error: SharedString },
    Markdown(SharedString),
    Image,
    Text { editor: Entity<EditorState>, truncated: bool },
    Folder { entries: Vec<Entry>, truncated: bool },
    Binary,
    Error(SharedString),
}

/// The panel. One file at a time; folders navigate in place with a back button.
pub struct PreviewView {
    path: PathBuf,
    /// Session project, for showing paths relative to it.
    root: Option<PathBuf>,
    /// Folders we came from, for the back button.
    history: Vec<PathBuf>,
    kind: Kind,
    content: Content,
    stamp: Option<SystemTime>,
    /// «Код» instead of «Вид» for HTML and markdown.
    show_source: bool,
    /// Highlighted source for «Код», built on first use.
    source: Option<Entity<EditorState>>,
    _poll: Task<()>,
}

impl PreviewView {
    pub fn new(path: PathBuf, root: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let poll = cx.spawn_in(window, async move |this, cx| loop {
            cx.background_executor().timer(POLL).await;
            if this.update_in(cx, |this, window, cx| this.reload_if_changed(window, cx)).is_err() {
                break;
            }
        });
        let mut this = Self {
            kind: Kind::of(&path),
            path,
            root,
            history: Vec::new(),
            content: Content::Binary,
            stamp: None,
            show_source: false,
            source: None,
            _poll: poll,
        };
        this.load(window, cx);
        this
    }

    /// Show another file in the same panel (a new click in the transcript).
    pub fn open(&mut self, path: PathBuf, root: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        self.history.clear();
        self.root = root;
        self.show(path, window, cx);
    }

    fn show(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.kind = Kind::of(&path);
        self.path = path;
        self.load(window, cx);
        cx.notify();
    }

    fn navigate(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.history.push(self.path.clone());
        self.show(path, window, cx);
    }

    fn back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = self.history.pop() {
            self.show(path, window, cx);
        }
    }

    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.stamp = modified(&self.path);
        self.source = None;
        if !matches!(self.kind, Kind::Web | Kind::Markdown) {
            self.show_source = false;
        }
        if !self.path.exists() {
            self.content = Content::Error("Файл не найден".into());
            return;
        }
        self.content = match self.kind {
            Kind::Web => self.load_web(window, cx),
            Kind::Markdown => match read_text(&self.path) {
                Ok(Some((text, _))) => Content::Markdown(text.into()),
                Ok(None) => Content::Binary,
                Err(err) => Content::Error(err.into()),
            },
            Kind::Image => Content::Image,
            Kind::Folder => match list_dir(&self.path) {
                Ok((entries, truncated)) => Content::Folder { entries, truncated },
                Err(err) => Content::Error(err.into()),
            },
            Kind::Text => match read_text(&self.path) {
                Ok(Some((text, truncated))) => {
                    let language = self.path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
                    let editor = cx.new(|cx| EditorState::new(window, cx).language(language).default_value(text));
                    Content::Text { editor, truncated }
                }
                Ok(None) => Content::Binary,
                Err(err) => Content::Error(err.into()),
            },
        };
    }

    fn load_web(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Content {
        let builder = wry::WebViewBuilder::new().with_url(file_url(&self.path));
        match WebView::build(builder, window, cx) {
            Ok(webview) => Content::Web(cx.new(|_| webview)),
            Err(err) => {
                eprintln!("iv3: webview: {err}");
                match read_text(&self.path) {
                    Ok(Some((text, _))) => Content::HtmlFallback { text: text.into(), error: err.to_string().into() },
                    Ok(None) => Content::Binary,
                    Err(err) => Content::Error(err.into()),
                }
            }
        }
    }

    fn reload_if_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let stamp = modified(&self.path);
        if stamp == self.stamp {
            return;
        }
        match &self.content {
            Content::Web(webview) => {
                self.stamp = stamp;
                self.source = None;
                cx.notify();
                if let Err(err) = webview.read(cx).raw().reload() {
                    eprintln!("iv3: webview reload: {err}");
                }
            }
            _ => {
                self.load(window, cx);
                cx.notify();
            }
        }
    }

    /// Path relative to the session's project, else with `~` for home.
    fn display_path(&self) -> String {
        if let Some(root) = &self.root {
            if let Ok(rel) = self.path.strip_prefix(root) {
                return rel.display().to_string();
            }
            if let (Ok(a), Ok(b)) = (self.path.canonicalize(), root.canonicalize()) {
                if let Ok(rel) = a.strip_prefix(&b) {
                    return rel.display().to_string();
                }
            }
        }
        let full = self.path.display().to_string();
        match std::env::var("HOME") {
            Ok(home) if !home.is_empty() && full.starts_with(&home) => format!("~{}", &full[home.len()..]),
            _ => full,
        }
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let p = Palette::of(&theme);
        let shown = self.display_path();
        let (dir, name) = match shown.rfind('/') {
            Some(i) if i + 1 < shown.len() => (shown[..=i].to_owned(), shown[i + 1..].to_owned()),
            _ => (String::new(), shown),
        };
        let (open, reveal) = (self.path.clone(), self.path.clone());
        let segment = |id: &'static str, icon: IconName, label: &'static str, active: bool| {
            h_flex()
                .id(id)
                .gap_1()
                .items_center()
                .px(px(8.))
                .py(px(2.))
                .rounded(px(4.))
                .text_size(px(12.))
                .text_color(if active { p.ink } else { p.muted })
                .when(active, |d| d.bg(p.surface).font_weight(FontWeight::SEMIBOLD).shadow(ui::card_shadow()))
                .when(!active, |d| d.cursor_pointer().hover(|s| s.text_color(p.ink)))
                .child(Icon::new(icon).xsmall())
                .child(label)
        };
        h_flex()
            .flex_none()
            .h(px(44.))
            .px_3()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(p.border)
            .bg(p.surface)
            .when(!self.history.is_empty(), |this| {
                this.child(
                    PressButton::new("preview-back")
                        .ghost()
                        .xsmall()
                        .icon(IconName::ArrowLeft)
                        .tooltip("Назад")
                        .on_click(cx.listener(|this, _, window, cx| this.back(window, cx))),
                )
            })
            .child(Icon::new(self.kind.icon()).small().text_color(p.muted))
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .font_family(theme.mono_font_family.clone())
                    .text_size(px(12.))
                    .child(
                        div()
                            .flex_shrink(1.)
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis_start()
                            .text_color(p.faint)
                            .child(dir),
                    )
                    .child(div().flex_shrink_0().whitespace_nowrap().font_weight(FontWeight::MEDIUM).text_color(p.ink).child(name)),
            )
            .when(matches!(self.kind, Kind::Web | Kind::Markdown), |this| {
                this.child(
                    h_flex()
                        .flex_shrink_0()
                        .p(px(2.))
                        .gap(px(2.))
                        .rounded(px(6.))
                        .border_1()
                        .border_color(p.border)
                        .bg(p.sunken)
                        .child(segment("preview-view", IconName::Eye, "Вид", !self.show_source).on_click(cx.listener(|this, _, _, cx| {
                            this.show_source = false;
                            cx.notify();
                        })))
                        .child(segment("preview-code", IconName::SquareTerminal, "Код", self.show_source).on_click(cx.listener(|this, _, _, cx| {
                            this.show_source = true;
                            cx.notify();
                        }))),
                )
            })
            .child(
                PressButton::new("preview-open")
                    .ghost()
                    .xsmall()
                    .icon(IconName::ExternalLink)
                    .tooltip("Открыть в системе")
                    .on_click(move |_, _, _| run_open(&[open.as_os_str()])),
            )
            .child(
                PressButton::new("preview-reveal")
                    .ghost()
                    .xsmall()
                    .icon(IconName::FolderOpen)
                    .tooltip("Показать в Finder")
                    .on_click(move |_, _, _| run_open(&["-R".as_ref(), reveal.as_os_str()])),
            )
            .child(
                PressButton::new("preview-close")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Close)
                    .tooltip("Закрыть")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(PreviewEvent::Close))),
            )
    }

    /// «● следит за файлом · 4,1 КБ · изменён 1 мин назад».
    fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let p = Palette::of(&theme);
        let meta = std::fs::metadata(&self.path).ok();
        let size = match (&self.content, &meta) {
            (Content::Folder { entries, .. }, _) => Some(format!("{} элементов", entries.len())),
            (_, Some(m)) => Some(human_size(m.len())),
            _ => None,
        };
        let changed = self.stamp.map(|t| {
            let ago = crate::format::ago(chrono::DateTime::<chrono::Utc>::from(t));
            if ago == "сейчас" { "изменён только что".to_owned() } else { format!("изменён {ago} назад") }
        });
        h_flex()
            .flex_none()
            .h(px(28.))
            .px_3()
            .gap(px(6.))
            .items_center()
            .border_t_1()
            .border_color(p.border)
            .bg(p.surface)
            .font_family(theme.mono_font_family.clone())
            .text_size(px(11.))
            .text_color(p.faint)
            .child(ui::dot(p.green.solid).size(px(6.)))
            .child(div().text_color(p.green.text).child("следит за файлом"))
            .when_some(size, |this, s| this.child(format!("· {s}")))
            .when_some(changed, |this, c| this.child(format!("· {c}")))
    }

    fn render_body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        // «Код»: the file's source, highlighted. The web view must step aside
        // too, or its native layer stays on top.
        if self.show_source && matches!(self.kind, Kind::Web | Kind::Markdown) {
            if let Content::Web(webview) = &self.content {
                if webview.read(cx).visible() {
                    let _ = webview.read(cx).set_visible(false);
                }
            }
            if self.source.is_none() {
                if let Ok(Some((text, _))) = read_text(&self.path) {
                    let language = if self.kind == Kind::Web { "html" } else { "markdown" };
                    self.source = Some(cx.new(|cx| EditorState::new(window, cx).language(language).default_value(text)));
                }
            }
            if let Some(editor) = &self.source {
                return v_flex()
                    .flex_1()
                    .min_h_0()
                    .bg(Palette::of(cx.theme()).surface)
                    .child(Editor::new(editor).readonly(true).bordered(false).appearance(false).flex_1().min_h_0().size_full())
                    .into_any_element();
            }
        }
        let theme = cx.theme();
        let p = Palette::of(theme);
        let note = |text: String| {
            div()
                .flex_none()
                .px_3()
                .py_1()
                .border_b_1()
                .border_color(p.border)
                .bg(p.amber.soft)
                .font_family(theme.mono_font_family.clone())
                .text_size(px(11.))
                .text_color(p.amber.text)
                .child(text)
        };
        match &self.content {
            Content::Web(webview) => {
                // gpui-pre puts the native view above every GPUI layer in its bounds;
                // step aside while a dialog or sheet is open.
                let overlay = window.has_active_dialog(cx) || window.has_active_sheet(cx);
                let webview = webview.clone();
                if webview.read(cx).visible() == overlay {
                    let _ = webview.read(cx).set_visible(!overlay);
                }
                div().flex_1().min_h_0().size_full().child(webview).into_any_element()
            }
            Content::HtmlFallback { text, error } => v_flex()
                .flex_1()
                .min_h_0()
                .child(note(format!("Браузерный движок недоступен ({error}) — упрощённый вид")))
                .child(
                    div()
                        .id("preview-html")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .p_5()
                        .bg(p.surface)
                        .child(TextView::html(SharedString::from(format!("preview-html:{}", self.path.display())), text.clone()).selectable(true)),
                )
                .into_any_element(),
            Content::Markdown(text) => div()
                .id("preview-md")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .px_6()
                .py_5()
                .bg(p.surface)
                .text_size(px(14.))
                .text_color(p.fg)
                .child(TextView::markdown(SharedString::from(format!("preview-md:{}", self.path.display())), text.clone()).selectable(true))
                .into_any_element(),
            Content::Image => div()
                .flex_1()
                .min_h_0()
                .p_4()
                .bg(p.sunken)
                .child(
                    img(self.path.clone())
                        .size_full()
                        .object_fit(ObjectFit::Contain)
                        .with_fallback(|| div().size_full().flex().items_center().justify_center().text_sm().child("Не удалось показать картинку").into_any_element()),
                )
                .into_any_element(),
            Content::Text { editor, truncated } => v_flex()
                .flex_1()
                .min_h_0()
                .bg(p.surface)
                .when(*truncated, |this| this.child(note(format!("Показаны первые {} КБ файла", TEXT_LIMIT / 1024))))
                .child(Editor::new(editor).readonly(true).bordered(false).appearance(false).flex_1().min_h_0().size_full())
                .into_any_element(),
            Content::Folder { entries, truncated } => v_flex()
                .id("preview-folder")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .p_2()
                .bg(p.surface)
                .when(entries.is_empty(), |this| {
                    this.child(div().p_2().text_sm().text_color(p.muted).child("Папка пуста"))
                })
                .children(entries.iter().map(|entry| {
                    let target = entry.path.clone();
                    h_flex()
                        .id(SharedString::from(format!("entry:{}", entry.path.display())))
                        .flex_none()
                        .px_2()
                        .py(px(5.))
                        .gap_2()
                        .rounded(px(6.))
                        .text_size(px(13.))
                        .text_color(p.fg)
                        .cursor_pointer()
                        .hover(|s| s.bg(p.surface2))
                        .on_click(cx.listener(move |this, _, window, cx| this.navigate(target.clone(), window, cx)))
                        .child(
                            Icon::new(if entry.dir { IconName::Folder } else { IconName::File })
                                .small()
                                .text_color(if entry.dir { p.accent.solid } else { p.muted }),
                        )
                        .child(div().flex_1().min_w_0().truncate().child(entry.name.clone()))
                }))
                .when(*truncated, |this| {
                    this.child(div().p_2().font_family(theme.mono_font_family.clone()).text_size(px(11.)).text_color(p.faint).child(format!("показаны первые {FOLDER_LIMIT}")))
                })
                .into_any_element(),
            Content::Binary => self.render_message("Этот файл нельзя показать здесь", cx),
            Content::Error(err) => self.render_message(&err.clone(), cx),
        }
    }

    fn render_message(&self, text: &str, cx: &mut Context<Self>) -> AnyElement {
        let path = self.path.clone();
        v_flex()
            .flex_1()
            .items_center()
            .justify_center()
            .gap_3()
            .p_6()
            .child(div().text_size(px(13.)).text_color(Palette::of(cx.theme()).muted).child(text.to_owned()))
            .child(
                PressButton::new("preview-open-system")
                    .small()
                    .icon(IconName::ExternalLink)
                    .label("Открыть в системе")
                    .on_click(move |_, _, _| run_open(&[path.as_os_str()])),
            )
            .into_any_element()
    }
}

impl Render for PreviewView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .bg(Palette::of(cx.theme()).surface)
            .child(self.render_header(cx))
            .child(self.render_body(window, cx))
            .child(self.render_footer(cx))
    }
}

/// "812 Б", "4,1 КБ", "2,3 МБ".
fn human_size(bytes: u64) -> String {
    let (value, unit) = match bytes {
        0..1024 => return format!("{bytes} Б"),
        1024..1_048_576 => (bytes as f64 / 1024., "КБ"),
        _ => (bytes as f64 / 1_048_576., "МБ"),
    };
    format!("{value:.1} {unit}").replace('.', ",")
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// The start of a text file, or `None` when it looks binary. `bool`: cut at `TEXT_LIMIT`.
fn read_text(path: &Path) -> Result<Option<(String, bool)>, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("Не удалось прочитать: {e}"))?;
    let size = file.metadata().map(|m| m.len()).unwrap_or(0);
    let mut bytes = Vec::new();
    file.take(TEXT_LIMIT).read_to_end(&mut bytes).map_err(|e| format!("Не удалось прочитать: {e}"))?;
    if bytes[..bytes.len().min(8192)].contains(&0) {
        return Ok(None);
    }
    let truncated = size > TEXT_LIMIT;
    if truncated {
        // Don't end on half a UTF-8 character.
        if let Err(err) = std::str::from_utf8(&bytes) {
            if err.error_len().is_none() {
                bytes.truncate(err.valid_up_to());
            }
        }
    }
    Ok(Some((String::from_utf8_lossy(&bytes).into_owned(), truncated)))
}

/// Folders first, then files, by name; `.DS_Store` skipped. `bool`: cut at `FOLDER_LIMIT`.
fn list_dir(path: &Path) -> Result<(Vec<Entry>, bool), String> {
    let mut entries: Vec<Entry> = std::fs::read_dir(path)
        .map_err(|e| format!("Не удалось открыть папку: {e}"))?
        .flatten()
        .filter(|e| e.file_name() != ".DS_Store")
        .map(|e| {
            let path = e.path();
            Entry { name: e.file_name().to_string_lossy().into_owned().into(), dir: path.is_dir(), path }
        })
        .collect();
    entries.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    let truncated = entries.len() > FOLDER_LIMIT;
    entries.truncate(FOLDER_LIMIT);
    Ok((entries, truncated))
}

/// `file://` URL with the path percent-encoded (NSURL rejects spaces and non-ASCII).
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

fn run_open(args: &[&std::ffi::OsStr]) {
    if let Err(err) = std::process::Command::new("open").args(args).spawn() {
        eprintln!("iv3: open: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[core::prelude::v1::test]
    fn file_url_encodes_spaces_and_cyrillic() {
        assert_eq!(file_url(Path::new("/tmp/a b/отчёт.html")), "file:///tmp/a%20b/%D0%BE%D1%82%D1%87%D1%91%D1%82.html");
    }

    #[core::prelude::v1::test]
    fn kinds_by_extension() {
        assert!(Kind::of(Path::new("/x/page.HTML")) == Kind::Web);
        assert!(Kind::of(Path::new("/x/readme.md")) == Kind::Markdown);
        assert!(Kind::of(Path::new("/x/shot.png")) == Kind::Image);
        assert!(Kind::of(Path::new("/x/app.tsx")) == Kind::Text);
    }

    #[core::prelude::v1::test]
    fn read_text_flags_binary_and_truncation() {
        let dir = std::env::temp_dir().join(format!("iv3-preview-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("a.bin");
        std::fs::write(&bin, [1u8, 0, 2]).unwrap();
        assert!(read_text(&bin).unwrap().is_none());
        let big = dir.join("big.txt");
        // Odd offset: the limit falls in the middle of a two-byte character.
        std::fs::write(&big, format!("a{}", "я".repeat(TEXT_LIMIT as usize))).unwrap();
        let (text, truncated) = read_text(&big).unwrap().unwrap();
        assert!(truncated);
        assert!(!text.contains('\u{FFFD}'));
        std::fs::remove_dir_all(&dir).ok();
    }
}
