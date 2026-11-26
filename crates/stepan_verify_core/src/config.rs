//! `~/.stepan_verify/config.toml`: app settings. Missing keys fall back to defaults,
//! so an old or hand-written file keeps working.

use std::fs;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::{Home, Provider};

/// How much agents may do without asking.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentMode {
    /// Ask before every command and edit.
    Ask,
    /// File edits go through, commands ask.
    AutoEdits,
    /// Never ask.
    #[default]
    Full,
}

impl AgentMode {
    pub const ALL: [AgentMode; 3] = [AgentMode::Full, AgentMode::AutoEdits, AgentMode::Ask];

    pub fn label(self) -> &'static str {
        match self {
            AgentMode::Full => "Полный доступ",
            AgentMode::AutoEdits => "Правки сами",
            AgentMode::Ask => "Спрашивать всё",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            AgentMode::Full => "Агент ничего не спрашивает: правит файлы, запускает команды, ходит в сеть.",
            AgentMode::AutoEdits => "Правки файлов проходят сами, команды в терминале ждут подтверждения.",
            AgentMode::Ask => "Каждая правка и команда ждёт подтверждения.",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentsConfig {
    pub mode: AgentMode,
    pub default_provider: ProviderSetting,
    /// Parallel agents the autopilot may run.
    pub autopilot_limit: usize,
    /// Model for new Claude tasks and sessions (`opus`, `sonnet`…); empty = Claude Code's own default.
    pub claude_model: String,
    /// Model for new Codex tasks and sessions (a slug); empty = Codex's own default.
    pub codex_model: String,
}

impl Default for AgentsConfig {
    fn default() -> Self {
        Self {
            mode: AgentMode::Full,
            default_provider: ProviderSetting::Claude,
            autopilot_limit: 2,
            claude_model: String::new(),
            codex_model: String::new(),
        }
    }
}

impl AgentsConfig {
    /// The model new work of `provider` runs on; `None` = the CLI's own default.
    pub fn model(&self, provider: Provider) -> Option<String> {
        let model = match provider {
            Provider::Claude => &self.claude_model,
            Provider::Codex => &self.codex_model,
        };
        Some(model.clone()).filter(|m| !m.is_empty())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderSetting {
    #[default]
    Claude,
    Codex,
}

impl From<ProviderSetting> for Provider {
    fn from(p: ProviderSetting) -> Self {
        match p {
            ProviderSetting::Claude => Provider::Claude,
            ProviderSetting::Codex => Provider::Codex,
        }
    }
}

impl From<Provider> for ProviderSetting {
    fn from(p: Provider) -> Self {
        match p {
            Provider::Claude => ProviderSetting::Claude,
            Provider::Codex => ProviderSetting::Codex,
        }
    }
}

/// Helper agents (session distiller, flow map sync). The section keeps its
/// old name so existing config.toml files still load.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ChangesConfig {
    /// Claude model alias for helpers on Claude sessions: haiku, sonnet, opus.
    pub model: String,
    /// Codex model slug for helpers on Codex sessions; empty = Codex's own default.
    pub codex_model: String,
}

impl Default for ChangesConfig {
    fn default() -> Self {
        Self { model: "sonnet".into(), codex_model: String::new() }
    }
}

/// The «Документы» shelf and how agents feed it.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct DocsConfig {
    /// Every agent session stepan_verify starts gets the document rules plus the
    /// project's facts, decisions and open questions.
    pub brief: bool,
    /// After a session goes quiet, a subagent writes down its decisions and facts.
    pub distill: DistillMode,
    /// Minutes without a new turn before a session counts as quiet.
    pub distill_after_min: u64,
}

impl Default for DocsConfig {
    fn default() -> Self {
        Self { brief: true, distill: DistillMode::Auto, distill_after_min: 10 }
    }
}

/// When the post-session distiller runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DistillMode {
    /// When a session has been quiet for `distill_after_min`.
    #[default]
    Auto,
    Off,
}

impl DistillMode {
    pub const ALL: [DistillMode; 2] = [DistillMode::Auto, DistillMode::Off];

    pub fn label(self) -> &'static str {
        match self {
            DistillMode::Auto => "после сессии",
            DistillMode::Off => "выкл",
        }
    }
}

/// Interface fonts; each falls back to a safe face when not installed.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct AppearanceConfig {
    /// Text: Instrument Sans is embedded in the app, so it is always there.
    pub font: String,
    /// Code, paths, ids.
    pub mono_font: String,
    /// Interface scale, ⌘+ / ⌘− / ⌘0 (1.0 = 100%).
    pub zoom: f32,
}

impl Default for AppearanceConfig {
    fn default() -> Self {
        Self { font: "Instrument Sans".into(), mono_font: "Fira Code".into(), zoom: 1.0 }
    }
}

/// New versions from GitHub Releases.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdatesConfig {
    /// Look for a new release at start and every few hours.
    pub check: bool,
}

impl Default for UpdatesConfig {
    fn default() -> Self {
        Self { check: true }
    }
}

/// A model Codex offers: (slug, display name), from Codex's own model cache.
pub fn codex_models() -> Vec<(String, String)> {
    let Some(home) = dirs::home_dir() else { return Vec::new() };
    let Ok(text) = fs::read_to_string(home.join(".codex/models_cache.json")) else { return Vec::new() };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { return Vec::new() };
    let list = v.get("models").unwrap_or(&v);
    list.as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let slug = m.get("slug").or_else(|| m.get("id"))?.as_str()?.to_owned();
            let name = m.get("display_name").and_then(|d| d.as_str()).unwrap_or(&slug).to_owned();
            Some((slug, name))
        })
        // Internal helpers, not models you would pick for a report.
        .filter(|(slug, _)| !slug.contains("auto-review") && !slug.contains("reserve"))
        .collect()
}

/// Claude Code model aliases, newest family first; `--model` takes any of them.
pub const CLAUDE_MODELS: [&str; 4] = ["fable", "opus", "sonnet", "haiku"];

/// The model Claude Code uses when none is given (`model` in ~/.claude/settings.json).
pub fn claude_default_model() -> Option<String> {
    let text = fs::read_to_string(dirs::home_dir()?.join(".claude/settings.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    v.get("model")?.as_str().filter(|m| !m.is_empty()).map(str::to_owned)
}

/// The model Codex uses when none is given (`model = ...` in ~/.codex/config.toml).
pub fn codex_default_model() -> Option<String> {
    let text = fs::read_to_string(dirs::home_dir()?.join(".codex/config.toml")).ok()?;
    let v: toml::Value = toml::from_str(&text).ok()?;
    v.get("model")?.as_str().map(str::to_owned)
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub agents: AgentsConfig,
    pub changes: ChangesConfig,
    pub docs: DocsConfig,
    pub appearance: AppearanceConfig,
    pub updates: UpdatesConfig,
    pub flows: FlowsConfig,
}

/// The project flow map («Флоу»).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct FlowsConfig {
    /// Link the `stepan-verify-flows` skill into Claude and Codex skill folders.
    pub skill: bool,
    /// Sync the map by itself when code it describes changed and things went quiet.
    pub auto_sync: bool,
}

impl Default for FlowsConfig {
    fn default() -> Self {
        Self { skill: true, auto_sync: false }
    }
}

impl Config {
    pub fn load(home: &Home) -> Config {
        fs::read_to_string(home.root.join("config.toml"))
            .ok()
            .and_then(|text| match toml::from_str(&text) {
                Ok(config) => Some(config),
                Err(err) => {
                    eprintln!("stepan_verify: config.toml: {err}");
                    None
                }
            })
            .unwrap_or_default()
    }

    pub fn save(&self, home: &Home) -> Result<()> {
        let body = toml::to_string_pretty(self)?;
        fs::write(home.root.join("config.toml"), format!("# stepan_verify settings (also editable in the app: Настройки)\n\n{body}"))
            .context("config.toml")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_means_full_access() {
        let config: Config = toml::from_str("").unwrap();
        assert_eq!(config.agents.mode, AgentMode::Full);
        assert_eq!(config.changes.model, "sonnet");
        assert!(config.docs.brief);
        assert_eq!(config.docs.distill, DistillMode::Auto);
    }

    #[test]
    fn partial_file_keeps_defaults() {
        let config: Config = toml::from_str("[agents]\nmode = \"ask\"\n").unwrap();
        assert_eq!(config.agents.mode, AgentMode::Ask);
        assert_eq!(config.agents.autopilot_limit, 2);
        let back: Config = toml::from_str(&toml::to_string_pretty(&config).unwrap()).unwrap();
        assert_eq!(back.agents.mode, AgentMode::Ask);
    }

    #[test]
    fn empty_agent_model_means_cli_default() {
        let config: Config = toml::from_str("[agents]\nclaude_model = \"opus\"\n").unwrap();
        assert_eq!(config.agents.model(Provider::Claude).as_deref(), Some("opus"));
        assert_eq!(config.agents.model(Provider::Codex), None);
    }
}
