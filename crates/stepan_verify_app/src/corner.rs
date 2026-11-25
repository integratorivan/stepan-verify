//! A looping clip in the bottom-right corner of «Настройки» that plays a song
//! quietly while the pointer is over it.
//!
//! The media is the user's own and stays out of the app bundle:
//! `<home>/media/settings.webp` (animated WebP or GIF) and
//! `<home>/media/settings.mp3` (or .m4a). Without the clip there is no corner.

use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
};

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::{
    app::AppView,
    ui::{self, Palette},
    zoom::px,
};

/// afplay's volume, linear: 1 is full; 2% is barely there.
const VOLUME: &str = "0.02";
/// A round video note; a non-square clip is cropped to the circle.
const SIZE: f32 = 200.;

fn find(dir: &Path, names: &[&str]) -> Option<PathBuf> {
    names.iter().map(|n| dir.join(n)).find(|p| p.is_file())
}

/// The afplay process while the pointer is over the clip.
#[derive(Default)]
struct Player(Option<Child>);

impl Global for Player {}

impl Drop for Player {
    fn drop(&mut self) {
        stop_child(&mut self.0);
    }
}

fn stop_child(child: &mut Option<Child>) {
    if let Some(mut c) = child.take() {
        let _ = c.kill();
        let _ = c.wait();
    }
}

fn play(song: Option<PathBuf>, on: bool, cx: &mut App) {
    let player = cx.default_global::<Player>();
    stop_child(&mut player.0);
    let Some(song) = song.filter(|_| on) else { return };
    match Command::new("/usr/bin/afplay")
        .args(["-v", VOLUME])
        .arg(&song)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => player.0 = Some(child),
        Err(err) => eprintln!("stepan_verify: afplay {}: {err}", song.display()),
    }
}

/// Stop the song when the app quits: afplay would outlive it.
pub fn init(cx: &mut App) {
    cx.on_app_quit(|cx| {
        silence(cx);
        async {}
    })
    .detach();
}

/// Stop the song (leaving the page, quitting).
pub fn silence(cx: &mut App) {
    if let Some(player) = cx.try_global::<Player>() {
        if player.0.is_some() {
            stop_child(&mut cx.global_mut::<Player>().0);
        }
    }
}

pub fn render(app: &AppView, p: &Palette, cx: &mut Context<AppView>) -> Option<AnyElement> {
    let dir = app.home.root.join("media");
    let clip = find(&dir, &["settings.webp", "settings.gif"])?;
    let song = find(&dir, &["settings.mp3", "settings.m4a"]);
    Some(
        div()
            .id("settings-corner")
            .absolute()
            .right(px(20.))
            .bottom(px(20.))
            .size(px(SIZE))
            .rounded_full()
            .overflow_hidden()
            .border_1()
            .border_color(p.border)
            // Decoding the clip takes a moment: a quiet card meanwhile.
            .bg(p.sunken)
            .shadow(ui::card_shadow())
            .when(song.is_some(), |d| d.cursor_pointer())
            // GPUI only animates an image that has an id to keep its frame in.
            .child(img(clip).id("settings-corner-clip").size_full().rounded_full().object_fit(ObjectFit::Cover))
            .on_hover(cx.listener(move |_, on: &bool, _, cx| play(song.clone(), *on, cx)))
            .into_any_element(),
    )
}
