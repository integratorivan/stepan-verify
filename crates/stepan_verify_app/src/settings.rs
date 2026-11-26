//! App-wide settings from `~/.stepan_verify/config.toml`, kept as a GPUI global so any
//! view can read them and the settings page can change them.

use gpui_kit::*;
use stepan_verify_agents::Mode;
use stepan_verify_core::{AgentMode, Config, Home};

pub struct Settings {
    pub home: Home,
    pub config: Config,
}

impl Global for Settings {}

pub fn init(home: Home, cx: &mut App) {
    let config = Config::load(&home);
    cx.set_global(Settings { home, config });
}

pub fn config(cx: &App) -> &Config {
    &cx.global::<Settings>().config
}

/// Change settings and persist them.
pub fn update(cx: &mut App, change: impl FnOnce(&mut Config)) {
    let settings = cx.global_mut::<Settings>();
    change(&mut settings.config);
    if let Err(err) = settings.config.save(&settings.home) {
        eprintln!("stepan_verify: save settings: {err:#}");
    }
    cx.refresh_windows();
}

/// Permission mode for new agent processes.
pub fn agent_mode(cx: &App) -> Mode {
    match config(cx).agents.mode {
        AgentMode::Full => Mode::FullAccess,
        AgentMode::AutoEdits => Mode::AutoEdits,
        AgentMode::Ask => Mode::Ask,
    }
}

/// The model a helper agent of `provider`'s family runs on (Настройки →
/// «Служебные агенты»). `None` for Codex means Codex's own default.
pub fn report_model(provider: stepan_verify_agents::Provider, cx: &App) -> Option<String> {
    let changes = &config(cx).changes;
    match provider {
        stepan_verify_agents::Provider::Claude => Some(changes.model.clone()),
        stepan_verify_agents::Provider::Codex => Some(changes.codex_model.clone()).filter(|m| !m.is_empty()),
    }
}
