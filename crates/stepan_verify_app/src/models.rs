//! Which model an agent runs on: the choices per agent and a dropdown to pick one.
//! `None` everywhere means "not chosen here": the task follows Настройки, and
//! Настройки follow the CLI's own config.

use std::rc::Rc;

use gpui_kit::component::{
    Sizable as _,
    button::Button,
    menu::{DropdownMenu as _, PopupMenuItem},
};
use gpui_kit::*;
use stepan_verify_core::{Provider, config};

/// Models you can pick for `provider`: Claude aliases, or slugs from Codex's cache.
pub fn choices(provider: Provider) -> Vec<String> {
    match provider {
        Provider::Claude => config::CLAUDE_MODELS.map(str::to_owned).to_vec(),
        Provider::Codex => config::codex_models().into_iter().map(|(slug, _)| slug).collect(),
    }
}

/// What the CLI runs when stepan_verify gives no model (its own config file).
pub fn cli_default(provider: Provider) -> Option<String> {
    match provider {
        Provider::Claude => config::claude_default_model(),
        Provider::Codex => config::codex_default_model(),
    }
}

/// Настройки → «Модель по умолчанию» for `provider`; `None` = the CLI's default.
pub fn settings_default(provider: Provider, cx: &App) -> Option<String> {
    crate::settings::config(cx).agents.model(provider)
}

/// The model a task runs on when `provider` takes it: its own choice, else the settings default.
pub fn for_task(task: &stepan_verify_core::Task, provider: Provider, cx: &App) -> Option<String> {
    task.model(provider).map(str::to_owned).or_else(|| settings_default(provider, cx))
}

/// «по умолчанию · opus»: what "no choice" resolves to, when it is known.
pub fn default_label(resolved: Option<&str>) -> String {
    match resolved {
        Some(m) => format!("по умолчанию · {m}"),
        None => "по умолчанию".into(),
    }
}

/// A dropdown with «по умолчанию» (`None`, resolving to `fallback`) and every
/// model of `provider`. `on_pick` gets the new choice.
pub fn picker(
    id: impl Into<ElementId>,
    provider: Provider,
    current: Option<String>,
    fallback: Option<String>,
    on_pick: impl Fn(Option<String>, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let label = match &current {
        Some(m) => m.clone(),
        None => default_label(fallback.as_deref()),
    };
    let on_pick = Rc::new(on_pick);
    Button::new(id).small().outline().dropdown_caret(true).label(label).dropdown_menu(move |menu, _, _| {
        let mut options = vec![(None, default_label(fallback.as_deref()))];
        let mut models = choices(provider);
        // A model set by hand (config file, CLI) still shows as chosen.
        if let Some(m) = current.as_ref().filter(|m| !models.contains(m)) {
            models.push(m.clone());
        }
        options.extend(models.into_iter().map(|m| (Some(m.clone()), m)));
        options.into_iter().fold(menu.label(format!("Модель {}", provider.label())), |menu, (model, label)| {
            let checked = model == current;
            let on_pick = on_pick.clone();
            menu.item(PopupMenuItem::new(label).checked(checked).on_click(move |_, window, cx| on_pick(model.clone(), window, cx)))
        })
    })
}
