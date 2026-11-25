mod app;
mod attach;
mod changes;
mod board;
mod dashboard;
mod docs;
mod format;
mod icons;
mod live_card;
mod loading;
mod media;
mod new_session;
mod new_task;
mod preview;
mod project_picker;
mod session;
mod settings;
mod settings_page;
mod skills;
mod ui;

use gpui_kit::*;

fn main() {
    let home = iv3_core::Home::locate();

    // `iv3 task …` is the board CLI for agents and scripts; no window.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("task") {
        match iv3_core::cli::run(&home, &args[1..]) {
            Ok(out) => println!("{out}"),
            Err(err) => {
                eprintln!("iv3: {err:#}");
                std::process::exit(1);
            }
        }
        return;
    }
    if let Err(err) = home.ensure() {
        eprintln!("iv3: cannot prepare {}: {err:#}", home.root.display());
    }

    let launch = app::Launch::from_args(std::env::args().skip(1));

    application().with_assets(icons::AppAssets).run(move |cx| {
        init(cx);
        ui::init(cx);
        settings::init(home.clone(), cx);
        attach::init(cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(None, size(px(1280.), px(820.)), cx)),
            // Traffic lights sit inside the sidebar, as in the prototype.
            titlebar: Some(TitlebarOptions {
                title: Some("iv3".into()),
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
