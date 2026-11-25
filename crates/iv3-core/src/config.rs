//! `~/.iv3/config.toml`: app settings. Missing keys fall back to defaults,
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
}

impl Default for AgentsConfig {
    fn default() -> Self {
        Self { mode: AgentMode::Full, default_provider: ProviderSetting::Claude, autopilot_limit: 2 }
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

/// The "Изменения" report subagent.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ChangesConfig {
    /// Claude model alias for reports on Claude sessions: haiku, sonnet, opus.
    pub model: String,
    /// Codex model slug for reports on Codex sessions; empty = Codex's own default.
    pub codex_model: String,
}

impl Default for ChangesConfig {
    fn default() -> Self {
        Self { model: "sonnet".into(), codex_model: String::new() }
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
}

impl Config {
    pub fn load(home: &Home) -> Config {
        fs::read_to_string(home.root.join("config.toml"))
            .ok()
            .and_then(|text| match toml::from_str(&text) {
                Ok(config) => Some(config),
                Err(err) => {
                    eprintln!("iv3: config.toml: {err}");
                    None
                }
            })
            .unwrap_or_default()
    }

    pub fn save(&self, home: &Home) -> Result<()> {
        let body = toml::to_string_pretty(self)?;
        fs::write(home.root.join("config.toml"), format!("# iv3 settings (also editable in the app: Настройки)\n\n{body}"))
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
    }

    #[test]
    fn partial_file_keeps_defaults() {
        let config: Config = toml::from_str("[agents]\nmode = \"ask\"\n").unwrap();
        assert_eq!(config.agents.mode, AgentMode::Ask);
        assert_eq!(config.agents.autopilot_limit, 2);
        let back: Config = toml::from_str(&toml::to_string_pretty(&config).unwrap()).unwrap();
        assert_eq!(back.agents.mode, AgentMode::Ask);
    }
}
