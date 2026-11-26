//! Interface zoom (⌘+ / ⌘− / ⌘0) and the app menu with ⌘Q.
//!
//! GPUI has no window-wide zoom, and the app lays out in pixels, so every
//! file imports this module's `px` instead of GPUI's: a length written as
//! `px(12.)` is 12 interface units, `12 × zoom` window pixels. What GPUI
//! hands back — mouse positions, element bounds, the viewport — is in window
//! pixels; `units` turns it back into interface units.

use std::sync::atomic::{AtomicU32, Ordering};

use gpui_kit::{App, KeyBinding, Menu, MenuItem, Pixels, SystemMenuType, component::Theme};

gpui_kit::actions!(stepan, [ZoomIn, ZoomOut, ZoomReset, Quit, Hide, HideOthers]);

const STEPS: [f32; 9] = [0.8, 0.9, 1.0, 1.1, 1.2, 1.35, 1.5, 1.75, 2.0];

static ZOOM: AtomicU32 = AtomicU32::new(0x3f80_0000); // 1.0

pub fn factor() -> f32 {
    f32::from_bits(ZOOM.load(Ordering::Relaxed))
}

/// `v` interface units as window pixels.
pub fn px(v: f32) -> Pixels {
    gpui_kit::px(v * factor())
}

/// Window pixels (from GPUI: mouse, bounds, viewport) as interface units.
pub fn units(p: Pixels) -> f32 {
    f32::from(p) / factor()
}

/// The GPUI theme's text size at 100%, so the components' rem-based sizes scale too.
struct Base {
    font: Pixels,
    mono: Pixels,
}

impl gpui_kit::Global for Base {}

pub fn init(cx: &mut App) {
    let theme = Theme::global(cx);
    cx.set_global(Base { font: theme.font_size, mono: theme.mono_font_size });
    apply(crate::settings::config(cx).appearance.zoom, cx);

    cx.on_action(|_: &ZoomIn, cx| step(1, cx));
    cx.on_action(|_: &ZoomOut, cx| step(-1, cx));
    cx.on_action(|_: &ZoomReset, cx| set(1.0, cx));
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.bind_keys([
        KeyBinding::new("cmd-=", ZoomIn, None),
        KeyBinding::new("cmd-+", ZoomIn, None),
        KeyBinding::new("cmd-shift-=", ZoomIn, None),
        KeyBinding::new("cmd--", ZoomOut, None),
        KeyBinding::new("cmd-0", ZoomReset, None),
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("cmd-alt-h", HideOthers, None),
    ]);
    cx.set_menus([
        Menu::new("Stepan").items([
            MenuItem::os_submenu("Службы", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Скрыть Stepan", Hide),
            MenuItem::action("Скрыть остальные", HideOthers),
            MenuItem::separator(),
            MenuItem::action("Завершить Stepan", Quit),
        ]),
        Menu::new("Вид").items([
            MenuItem::action("Увеличить", ZoomIn),
            MenuItem::action("Уменьшить", ZoomOut),
            MenuItem::action("Реальный размер", ZoomReset),
        ]),
    ]);
}

fn step(dir: i32, cx: &mut App) {
    stepan_verify_core::usage::hit("app.zoom");
    let now = factor();
    let next = if dir > 0 {
        STEPS.iter().copied().find(|s| *s > now + 0.001).unwrap_or(STEPS[STEPS.len() - 1])
    } else {
        STEPS.iter().rev().copied().find(|s| *s < now - 0.001).unwrap_or(STEPS[0])
    };
    set(next, cx);
}

fn set(zoom: f32, cx: &mut App) {
    apply(zoom, cx);
    let zoom = factor();
    crate::settings::update(cx, |c| c.appearance.zoom = zoom);
}

fn apply(zoom: f32, cx: &mut App) {
    let zoom = if zoom.is_finite() { zoom.clamp(STEPS[0], STEPS[STEPS.len() - 1]) } else { 1.0 };
    ZOOM.store(zoom.to_bits(), Ordering::Relaxed);
    let (font, mono) = {
        let base = cx.global::<Base>();
        (base.font * zoom, base.mono * zoom)
    };
    Theme::update(cx, |t| {
        t.font_size = font;
        t.mono_font_size = mono;
    });
    cx.refresh_windows();
}
