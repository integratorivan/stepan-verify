//! Sessions you put away: hidden from the dashboard, kept on disk.
//! `state/archived.json` holds `provider:id` keys.

use std::{collections::BTreeSet, fs, path::PathBuf};

use anyhow::Result;

use crate::{Home, Provider};

pub fn key(provider: Provider, id: &str) -> String {
    format!("{}:{id}", provider.label().to_lowercase())
}

#[derive(Debug, Default)]
pub struct Archive {
    path: PathBuf,
    keys: BTreeSet<String>,
}

impl Archive {
    pub fn load(home: &Home) -> Self {
        let path = home.state().join("archived.json");
        let keys = fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        Self { path, keys }
    }

    pub fn contains(&self, provider: Provider, id: &str) -> bool {
        self.keys.contains(&key(provider, id))
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    pub fn set(&mut self, provider: Provider, id: &str, archived: bool) -> Result<()> {
        let k = key(provider, id);
        let changed = if archived { self.keys.insert(k) } else { self.keys.remove(&k) };
        if changed {
            if let Some(dir) = self.path.parent() {
                fs::create_dir_all(dir)?;
            }
            let tmp = self.path.with_extension("json.tmp");
            fs::write(&tmp, serde_json::to_string_pretty(&self.keys)?)?;
            fs::rename(tmp, &self.path)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let dir = std::env::temp_dir().join(format!("stepan_verify-archive-{}", std::process::id()));
        let home = Home { root: dir.clone() };
        let mut a = Archive::load(&home);
        a.set(Provider::Codex, "abc", true).unwrap();
        assert!(Archive::load(&home).contains(Provider::Codex, "abc"));
        assert!(!Archive::load(&home).contains(Provider::Claude, "abc"));
        a.set(Provider::Codex, "abc", false).unwrap();
        assert!(Archive::load(&home).is_empty());
        let _ = fs::remove_dir_all(dir);
    }
}
