mod app;
mod attach;
mod changes;
mod board;
mod dashboard;
mod distill;
mod docs;
mod flows;
mod format;
mod icons;
mod live_card;
mod loading;
mod media;
mod memory;
mod new_session;
mod new_task;
mod preview;
mod project_picker;
mod session;
mod settings;
mod settings_page;
mod skills;
mod ui;
mod update;

use gpui_kit::*;

fn main() {
    let home = stepan_verify_core::Home::locate();

    // `stepan_verify task …` (the board) and `stepan_verify memory …` are CLIs for agents and scripts; no window.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("memory") {
        match stepan_verify_core::memory::run(&home, &args[1..]) {
            Ok(out) => println!("{out}"),
            Err(err) => {
                eprintln!("stepan_verify: {err:#}");
                std::process::exit(1);
            }
        }
        return;
    }
    if args.first().map(String::as_str) == Some("task") {
        match stepan_verify_core::cli::run(&home, &args[1..]) {
            Ok(out) => println!("{out}"),
            Err(err) => {
                eprintln!("stepan_verify: {err:#}");
                std::process::exit(1);
            }
        }
        return;
    }
    if let Err(err) = home.ensure() {
        eprintln!("stepan_verify: cannot prepare {}: {err:#}", home.root.display());
    }

    let launch = app::Launch::from_args(std::env::args().skip(1));
    login_path();
    if std::env::var_os("STEPAN_VERIFY_DEBUG_PATH").is_some() {
        eprintln!("stepan_verify: PATH={}", std::env::var("PATH").unwrap_or_default());
    }

    application().with_assets(icons::AppAssets).run(move |cx| {
        if let Err(err) = cx.text_system().add_fonts(icons::FONTS.iter().map(|f| std::borrow::Cow::Borrowed(*f)).collect()) {
            eprintln!("stepan_verify: fonts: {err:#}");
        }
        set_dock_icon();
        init(cx);
        ui::init(cx);
        settings::init(home.clone(), cx);
        ui::apply_fonts(cx);
        update::init(cx);
        attach::init(cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(None, size(px(1280.), px(820.)), cx)),
            // Traffic lights sit inside the sidebar, as in the prototype.
            titlebar: Some(TitlebarOptions {
                title: Some("Stepan".into()),
                appears_transparent: true,
                traffic_light_position: Some(point(px(14.), px(14.))),
            }),
            ..Default::default()
        };
        open_window(options, cx, |window, cx| cx.new(|cx| app::AppView::new(home, launch, window, cx)))
            .expect("failed to open window");
        cx.activate(true);
    });
}

/// Opened from Finder or the Dock, the app gets launchd's bare PATH
/// (/usr/bin:/bin:…) and would not find `claude`, `codex` or `node` from
/// Homebrew, npm or mise. Take the PATH your login shell builds.
fn login_path() {
    use std::{io::Read as _, process::{Command, Stdio}, time::{Duration, Instant}};
    let current = std::env::var("PATH").unwrap_or_default();
    let home = std::env::var("HOME").unwrap_or_default();
    // Started from a terminal: the PATH is already the user's.
    if current.split(':').any(|p| p.starts_with("/opt/homebrew") || p.starts_with("/usr/local/bin") || (!home.is_empty() && p.starts_with(&home))) {
        return;
    }
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let Ok(mut child) = Command::new(&shell)
        .args(["-l", "-i", "-c", "printf '\\n__STEPAN_VERIFY_PATH__%s' \"$PATH\""])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return;
    };
    // A heavy or stuck shell config must not hold the window back.
    let deadline = Instant::now() + Duration::from_secs(3);
    while child.try_wait().ok().flatten().is_none() {
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return;
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    let mut text = String::new();
    let _ = child.stdout.take().map(|mut o| o.read_to_string(&mut text));
    let Some((_, path)) = text.rsplit_once("__STEPAN_VERIFY_PATH__") else { return };
    let mut parts: Vec<&str> = path.trim().split(':').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return;
    }
    for p in current.split(':') {
        if !p.is_empty() && !parts.contains(&p) {
            parts.push(p);
        }
    }
    // SAFETY: before any other thread starts.
    unsafe { std::env::set_var("PATH", parts.join(":")) };
}

/// The Dock / Cmd-Tab picture. A bundled stepan_verify.app already has AppIcon.icns;
/// a bare `cargo run` binary would otherwise show the generic exec icon.
#[cfg(target_os = "macos")]
fn set_dock_icon() {
    use objc2::{AnyThread as _, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSData;
    let Some(mtm) = MainThreadMarker::new() else { return };
    let data = NSData::with_bytes(icons::ICON_512);
    if let Some(image) = NSImage::initWithData(NSImage::alloc(), &data) {
        // SAFETY: on the main thread, with a valid image.
        unsafe { NSApplication::sharedApplication(mtm).setApplicationIconImage(Some(&image)) };
    }
}

#[cfg(not(target_os = "macos"))]
fn set_dock_icon() {}
