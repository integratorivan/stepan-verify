mod app;
mod attach;
mod before_after;
mod changes;
mod corner;
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
mod models;
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
mod usage_page;
mod workspace;
mod zoom;

use gpui_kit::*;

/// Narrower than this the sidebar plus a page stop fitting; the pages are checked down to it.
const MIN_WIDTH: f32 = 960.;
const MIN_HEIGHT: f32 = 600.;

fn main() {
    let home = stepan_verify_core::Home::locate();

    // `stepan_verify task …` (the board), `stepan_verify memory …`, `stepan_verify doc …` (comments), `stepan_verify usage`
    // and `stepan_verify flows check|fmt` are CLIs for agents and scripts; no window.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("usage") {
        match stepan_verify_core::usage::cli(&home, &args[1..]) {
            Ok(out) => println!("{out}"),
            Err(err) => {
                eprintln!("stepan_verify: {err:#}");
                std::process::exit(1);
            }
        }
        return;
    }
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
    if args.first().map(String::as_str) == Some("doc") {
        match stepan_verify_core::doc_comments::run(&home, &args[1..]) {
            Ok(out) => println!("{out}"),
            Err(err) => {
                eprintln!("stepan_verify: {err:#}");
                std::process::exit(1);
            }
        }
        return;
    }
    if args.first().map(String::as_str) == Some("flows") {
        if args.get(1).map(String::as_str) == Some("check") {
            stepan_verify_core::usage::hit_cli(&home, "flows.check");
        }
        match stepan_verify_core::flowdsl::cli(&args[1..]) {
            Ok((out, ok)) => {
                println!("{out}");
                std::process::exit(if ok { 0 } else { 1 });
            }
            Err(err) => {
                eprintln!("stepan_verify: {err:#}");
                std::process::exit(1);
            }
        }
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
    // `/show-me` and `/describe-pr` for every Claude / Codex session; show-me files open in our preview.
    let targets = stepan_verify_core::flowsync::skill_targets();
    for w in stepan_verify_core::showme::install_skill(&home, &targets).warnings {
        eprintln!("stepan_verify: skill {}: {w}", stepan_verify_core::showme::SKILL_NAME);
    }
    for w in stepan_verify_core::describepr::install_skill(&home, &targets).warnings {
        eprintln!("stepan_verify: skill {}: {w}", stepan_verify_core::describepr::SKILL_NAME);
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
        workspace::init(&home, cx);
        ui::apply_fonts(cx);
        zoom::init(cx);
        corner::init(cx);
        update::init(cx);
        attach::init(cx);
        // Screenshots and layout checks: STEPAN_VERIFY_WINDOW=1100x700.
        let (w, h) = std::env::var("STEPAN_VERIFY_WINDOW")
            .ok()
            .and_then(|v| v.split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?))))
            .unwrap_or((1280., 820.));
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(None, size(px(w), px(h)), cx)),
            window_min_size: Some(size(px(MIN_WIDTH), px(MIN_HEIGHT))),
            // Traffic lights sit inside the sidebar, as in the prototype.
            titlebar: Some(TitlebarOptions {
                title: Some("Stepan".into()),
                appears_transparent: true,
                traffic_light_position: Some(point(px(14.), px(14.))),
            }),
            ..Default::default()
        };
        let _window = open_window(options, cx, |window, cx| cx.new(|cx| app::AppView::new(home, launch, window, cx)))
            .expect("failed to open window");
        #[cfg(feature = "snapshot")]
        snapshot(_window.0, cx);
        cx.activate(true);
    });
}

/// Opened from Finder or the Dock, the app gets launchd's bare PATH
/// (/usr/bin:/bin:…) and would not find `claude`, `codex` or `node` from
/// Homebrew, npm or mise. Take the PATH your login shell builds.
fn login_path() {
    let current = std::env::var("PATH").unwrap_or_default();
    let home = std::env::var("HOME").unwrap_or_default();
    // Started from a terminal: the PATH is already the user's. launchd's own
    // PATH has /usr/local/bin too, so that one says nothing.
    if current.split(':').any(|p| p.starts_with("/opt/homebrew") || (!home.is_empty() && p.starts_with(&home))) {
        return;
    }
    let shell_path = shell_path().unwrap_or_default();
    let mut parts: Vec<String> = shell_path.split(':').filter(|p| !p.is_empty()).map(str::to_owned).collect();
    // A slow or broken shell config still leaves the usual install places.
    let fallback = ["/opt/homebrew/bin", "/opt/homebrew/sbin", "/usr/local/bin"]
        .map(str::to_owned)
        .into_iter()
        .chain([".local/bin", ".bun/bin", ".cargo/bin", ".npm-global/bin"].map(|d| format!("{home}/{d}")));
    for p in current.split(':').map(str::to_owned).chain(fallback) {
        if !p.is_empty() && !parts.contains(&p) && std::path::Path::new(&p).is_dir() {
            parts.push(p);
        }
    }
    // SAFETY: before any other thread starts.
    unsafe { std::env::set_var("PATH", parts.join(":")) };
}

/// Dev builds with `--features snapshot`: STEPAN_VERIFY_SNAPSHOT=<png> writes a
/// frame of the window there after STEPAN_VERIFY_SNAPSHOT_DELAY seconds (8) and
/// quits. For checking screens where macOS denies screen capture.
#[cfg(feature = "snapshot")]
fn snapshot(window: AnyWindowHandle, cx: &mut App) {
    let Some(path) = std::env::var_os("STEPAN_VERIFY_SNAPSHOT") else { return };
    let delay = std::env::var("STEPAN_VERIFY_SNAPSHOT_DELAY").ok().and_then(|s| s.parse().ok()).unwrap_or(8);
    cx.spawn(async move |cx| {
        cx.background_executor().timer(std::time::Duration::from_secs(delay)).await;
        let saved = window.update(cx, |_, window, cx| {
            window.draw(cx).clear(cx);
            window.render_to_image().map_err(|e| e.to_string()).and_then(|img| img.save(&path).map_err(|e| e.to_string()))
        });
        match saved {
            Ok(Ok(())) => eprintln!("stepan_verify: snapshot saved"),
            Ok(Err(err)) => eprintln!("stepan_verify: snapshot: {err}"),
            Err(err) => eprintln!("stepan_verify: snapshot: {err}"),
        }
        cx.update(|cx| cx.quit());
    })
    .detach();
}

/// The PATH the login shell builds, or None when it fails or takes too long.
fn shell_path() -> Option<String> {
    use std::{io::Read as _, process::{Command, Stdio}, time::{Duration, Instant}};
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let Ok(mut child) = Command::new(&shell)
        .args(["-l", "-i", "-c", "printf '\\n__STEPAN_VERIFY_PATH__%s' \"$PATH\""])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return None;
    };
    // A heavy or stuck shell config must not hold the window back.
    let deadline = Instant::now() + Duration::from_secs(4);
    while child.try_wait().ok().flatten().is_none() {
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    let mut text = String::new();
    let _ = child.stdout.take().map(|mut o| o.read_to_string(&mut text));
    let (_, path) = text.rsplit_once("__STEPAN_VERIFY_PATH__")?;
    Some(path.trim().to_owned())
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
