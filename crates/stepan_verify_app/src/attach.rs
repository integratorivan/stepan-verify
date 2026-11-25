//! Attaching pictures and files: file picker, Cmd+V, drag-and-drop and a
//! macOS region screenshot (`screencapture -i`). `Attachments` holds the
//! pending set and renders it as a strip of removable thumbnails; the
//! composer and the "new session" / "new task" dialogs each own one.

use std::{
    path::{Path, PathBuf},
    process::Command,
    rc::Rc,
};

use gpui_kit::component::{
    ActiveTheme, Icon, IconName, Sizable as _, Theme,
    spinner::Spinner,
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use crate::zoom::px;
use crate::ui::PressButton;

use crate::ui::{self, Palette};

gpui_kit::actions!(attach, [TakeScreenshot]);

/// Key context of a view that owns `Attachments` (composer, dialogs).
pub const CONTEXT: &str = "Attachments";

pub fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("cmd-shift-2", TakeScreenshot, Some(CONTEXT))]);
}

pub enum AttachmentsEvent {
    /// The set changed.
    Changed,
    /// Files an images-only tray does not keep: the owner mentions their
    /// paths in the text, which both agents can open themselves.
    NotImages(Vec<PathBuf>),
}

impl EventEmitter<AttachmentsEvent> for Attachments {}

pub struct Attachments {
    /// Element id prefix, unique per owner.
    id: SharedString,
    /// Message attachments take only images; task attachments take any file.
    images_only: bool,
    paths: Vec<PathBuf>,
    /// `screencapture -i` is waiting for the user to select a region.
    capturing: bool,
    error: Option<SharedString>,
}

impl Attachments {
    pub fn new(id: impl Into<SharedString>, images_only: bool) -> Self {
        Self { id: id.into(), images_only, paths: Vec::new(), capturing: false, error: None }
    }

    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    pub fn is_capturing(&self) -> bool {
        self.capturing
    }

    /// Hand the attachments over (on send) and start empty.
    pub fn take(&mut self, cx: &mut Context<Self>) -> Vec<PathBuf> {
        let paths = std::mem::take(&mut self.paths);
        if !paths.is_empty() {
            cx.emit(AttachmentsEvent::Changed);
            cx.notify();
        }
        paths
    }

    pub fn add(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) {
        let rejected = merge_paths(&mut self.paths, paths, self.images_only);
        self.error = None;
        if !rejected.is_empty() {
            cx.emit(AttachmentsEvent::NotImages(rejected));
        }
        cx.emit(AttachmentsEvent::Changed);
        cx.notify();
    }

    pub fn remove(&mut self, path: &Path, cx: &mut Context<Self>) {
        self.paths.retain(|p| p != path);
        cx.emit(AttachmentsEvent::Changed);
        cx.notify();
    }

    /// The «+» button: a file dialog.
    pub fn pick(&mut self, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Прикрепить".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            this.update(cx, |this, cx| this.add(&paths, cx)).ok();
        })
        .detach();
    }

    /// Cmd+V with a copied image (screenshot) or copied files. Returns true
    /// when handled, so plain text still pastes normally.
    pub fn paste(&mut self, item: &ClipboardItem, cx: &mut Context<Self>) -> bool {
        let mut paths = Vec::new();
        for entry in item.entries() {
            match entry {
                ClipboardEntry::Image(image) => paths.extend(save_pasted_image(image.format(), image.bytes())),
                ClipboardEntry::ExternalPaths(files) => paths.extend(files.paths().iter().cloned()),
                ClipboardEntry::String(_) => {}
            }
        }
        if paths.is_empty() {
            return false;
        }
        self.add(&paths, cx);
        true
    }

    /// «Снимок экрана»: the user selects a region (Esc cancels, then nothing
    /// is attached). Runs off the UI thread; `screencapture` blocks until done.
    pub fn screenshot(&mut self, cx: &mut Context<Self>) {
        if self.capturing {
            return;
        }
        self.capturing = true;
        self.error = None;
        cx.notify();
        let target = screenshot_target(&image_cache(), chrono::Local::now());
        let capture = cx.background_spawn(async move { capture_region(&target) });
        cx.spawn(async move |this, cx| {
            let result = capture.await;
            this.update(cx, |this, cx| {
                this.capturing = false;
                match result {
                    Ok(Some(path)) => this.add(&[path], cx),
                    Ok(None) => cx.notify(),
                    Err(err) => {
                        this.error = Some(format!("Снимок экрана не получился: {err}").into());
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }
}

impl Render for Attachments {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let weak = cx.entity().downgrade();
        let remove: RemoveHandler = Rc::new(move |path, _, cx| {
            if let Some(this) = weak.upgrade() {
                this.update(cx, |this, cx| this.remove(path, cx));
            }
        });
        v_flex()
            .gap_1()
            .when(!self.paths.is_empty() || self.capturing, |this| {
                this.child(
                    strip(&self.id, &self.paths, &theme, Some(remove)).when(self.capturing, |this| {
                        let p = Palette::of(&theme);
                        this.child(
                            v_flex()
                                .size(px(THUMB))
                                .items_center()
                                .justify_center()
                                .gap_1()
                                .rounded(px(8.))
                                .border_1()
                                .border_dashed()
                                .border_color(p.border_strong)
                                .bg(p.surface2)
                                .font_family(theme.mono_font_family.clone())
                                .text_size(px(10.5))
                                .text_color(p.muted)
                                .child(Spinner::new().xsmall().color(p.accent.solid))
                                .child("область…"),
                        )
                    }),
                )
            })
            .when_some(self.error.clone(), |this, err| {
                let p = Palette::of(&theme);
                this.child(div().font_family(theme.mono_font_family.clone()).text_size(px(11.)).text_color(p.red.text).child(err))
            })
    }
}

/// The «+» button of a view that owns `tray`.
pub fn attach_button(id: impl Into<ElementId>, tray: &Entity<Attachments>, tooltip: &'static str) -> PressButton {
    let tray = tray.clone();
    PressButton::new(id)
        .ghost()
        .icon(IconName::Plus)
        .tooltip(tooltip)
        .on_click(move |_, _, cx| tray.update(cx, |t, cx| t.pick(cx)))
}

/// «Снимок экрана»: select a screen region and attach it (Cmd+Shift+2).
pub fn screenshot_button(id: impl Into<ElementId>, tray: &Entity<Attachments>, cx: &App) -> PressButton {
    let capturing = tray.read(cx).is_capturing();
    let tray = tray.clone();
    PressButton::new(id)
        .ghost()
        .icon(IconName::Frame)
        .loading(capturing)
        .disabled(capturing)
        .tooltip("Снимок экрана (⌘⇧2) — выдели область, Esc — отмена")
        .on_click(move |_, _, cx| tray.update(cx, |t, cx| t.screenshot(cx)))
}

pub type RemoveHandler = Rc<dyn Fn(&Path, &mut Window, &mut App)>;

const THUMB: f32 = 56.;

/// Images as square thumbnails, other files as chips. A click opens the file
/// in its macOS app; with `remove`, each one gets a ✕.
pub fn strip(id: &str, paths: &[PathBuf], theme: &Theme, remove: Option<RemoveHandler>) -> Div {
    let remove_button = |key: String, path: &Path| {
        remove.clone().map(|remove| {
            let path = path.to_owned();
            PressButton::new(SharedString::from(format!("{id}-remove:{key}")))
                .xsmall()
                .icon(IconName::Close)
                .tooltip("Убрать")
                .on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    remove(&path, window, cx);
                })
        })
    };
    let p = Palette::of(theme);
    h_flex().flex_wrap().gap_2().children(paths.iter().map(|path| {
        let key = path.display().to_string();
        let open = path.clone();
        let base = div()
            .id(SharedString::from(format!("{id}:{key}")))
            .rounded(px(8.))
            .border_1()
            .border_color(p.border)
            .cursor_pointer()
            .hover(|s| s.border_color(p.accent.solid))
            .tooltip({
                let full = key.clone();
                move |window, cx| gpui_kit::component::tooltip::Tooltip::new(full.clone()).build(window, cx)
            })
            .on_click(move |_, window, cx| crate::preview::open_in_app(open.clone(), None, window, cx));
        if stepan_verify_agents::is_image_path(path) {
            base.relative()
                .size(px(THUMB))
                .bg(p.surface2)
                .overflow_hidden()
                .shadow(ui::card_shadow())
                .child(
                    img(path.clone())
                        .size_full()
                        .object_fit(ObjectFit::Cover)
                        .with_fallback(|| div().size_full().flex().items_center().justify_center().text_xs().child("нет файла").into_any_element()),
                )
                .when_some(remove_button(key, path), |this, button| {
                    this.child(div().absolute().top_0p5().right_0p5().child(button))
                })
                .into_any_element()
        } else {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| key.clone());
            let meta = file_meta(path);
            h_flex()
                .child(
                    base.flex()
                        .items_center()
                        .h(px(THUMB))
                        .max_w(px(260.))
                        .pl(px(10.))
                        .pr_1()
                        .gap_2()
                        .bg(p.surface)
                        .shadow(ui::card_shadow())
                        .child(Icon::new(IconName::FileText).small().text_color(p.muted))
                        .child(
                            v_flex()
                                .min_w_0()
                                .gap(px(1.))
                                .child(div().min_w_0().truncate().text_size(px(13.)).text_color(p.fg).child(name))
                                .when_some(meta, |this, m| {
                                    this.child(div().font_family(theme.mono_font_family.clone()).text_size(px(10.5)).text_color(p.faint).child(m))
                                }),
                        )
                        .when_some(remove_button(key, path), |this, button| this.child(button)),
                )
                .into_any_element()
        }
    }))
}

/// "2 КБ" or "папка" under a file chip's name.
fn file_meta(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.is_dir() {
        return Some("папка".into());
    }
    let kb = meta.len().div_ceil(1024);
    Some(if kb < 1024 { format!("{kb} КБ") } else { format!("{:.1} МБ", kb as f64 / 1024.).replace('.', ",") })
}

/// Add `new` to `paths` without duplicates. With `images_only`, non-images
/// are returned instead of added.
fn merge_paths(paths: &mut Vec<PathBuf>, new: &[PathBuf], images_only: bool) -> Vec<PathBuf> {
    let mut rejected = Vec::new();
    for path in new {
        if images_only && !stepan_verify_agents::is_image_path(path) {
            rejected.push(path.clone());
        } else if !paths.contains(path) {
            paths.push(path.clone());
        }
    }
    rejected
}

/// `text` with the paths appended, space-separated.
pub fn append_mentions(text: &str, paths: &[PathBuf]) -> String {
    let mentions: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
    let sep = if text.is_empty() || text.ends_with(char::is_whitespace) { "" } else { " " };
    format!("{text}{sep}{}", mentions.join(" "))
}

/// Where pasted images and screenshots go (`~/.stepan_verify/state/images`).
pub fn image_cache() -> PathBuf {
    stepan_verify_core::Home::locate().state().join("images")
}

/// Write a pasted image (e.g. a Cmd+Ctrl+Shift+4 screenshot) to the image cache.
fn save_pasted_image(format: ImageFormat, bytes: &[u8]) -> Option<PathBuf> {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    let dir = image_cache();
    let path = dir.join(format!("paste-{:016x}.{}", hasher.finish(), format.extension()));
    if !path.exists() {
        std::fs::create_dir_all(&dir).ok()?;
        std::fs::write(&path, bytes).ok()?;
    }
    Some(path)
}

fn screenshot_target(dir: &Path, now: chrono::DateTime<chrono::Local>) -> PathBuf {
    dir.join(format!("screenshot-{}.png", now.format("%Y%m%d-%H%M%S%3f")))
}

/// `-i` interactive region (Space toggles window mode), `-x` no sound.
fn screencapture_command(target: &Path) -> Command {
    let mut cmd = Command::new("/usr/sbin/screencapture");
    cmd.arg("-i").arg("-x").arg(target);
    cmd
}

/// Blocks until the user selects a region or presses Esc (`Ok(None)`).
fn capture_region(target: &Path) -> std::io::Result<Option<PathBuf>> {
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir)?;
    }
    screencapture_command(target).status()?;
    Ok(captured(target))
}

/// The screenshot, if one was taken: Esc leaves no file behind.
fn captured(target: &Path) -> Option<PathBuf> {
    std::fs::metadata(target).is_ok_and(|m| m.len() > 0).then(|| target.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[core::prelude::v1::test]
    fn images_only_trays_hand_files_back() {
        let mut paths = vec![PathBuf::from("/a.png")];
        let rejected = merge_paths(&mut paths, &["/a.png".into(), "/b.JPG".into(), "/notes.txt".into()], true);
        assert_eq!(paths, [PathBuf::from("/a.png"), PathBuf::from("/b.JPG")]);
        assert_eq!(rejected, [PathBuf::from("/notes.txt")]);

        let rejected = merge_paths(&mut paths, &["/notes.txt".into()], false);
        assert!(rejected.is_empty());
        assert_eq!(paths.len(), 3);

        assert_eq!(append_mentions("глянь", &["/a.txt".into(), "/b.rs".into()]), "глянь /a.txt /b.rs");
        assert_eq!(append_mentions("", &["/a.txt".into()]), "/a.txt");
    }

    #[core::prelude::v1::test]
    fn screenshot_command_and_cancel() {
        let dir = std::env::temp_dir().join(format!("stepan_verify-shot-{}", std::process::id()));
        let target = screenshot_target(&dir, chrono::Local::now());
        assert!(target.starts_with(&dir));
        assert_eq!(target.extension().unwrap(), "png");

        let cmd = screencapture_command(&target);
        let args: Vec<_> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(cmd.get_program(), "/usr/sbin/screencapture");
        assert_eq!(args, ["-i", "-x", target.to_string_lossy().as_ref()]);

        // Esc: no file, nothing attached.
        assert_eq!(captured(&target), None);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&target, b"").unwrap();
        assert_eq!(captured(&target), None);
        std::fs::write(&target, b"\x89PNG").unwrap();
        assert_eq!(captured(&target), Some(target.clone()));
        let _ = std::fs::remove_dir_all(dir);
    }
}
