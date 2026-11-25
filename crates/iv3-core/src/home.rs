use std::{fs, path::PathBuf};

use anyhow::Result;

/// Layout of the shared iv3 folder (default `~/.iv3`, override with `IV3_HOME`).
///
/// ```text
/// ~/.iv3/
///   config.toml        app settings, project list
///   skills/<name>/     shared skills (SKILL.md), linked into every agent session
///   tasks/<id>.md      tasks: YAML frontmatter + markdown body
///   docs/              md/html documents by agents and you (agents get it as IV3_DOCS);
///                      metadata in YAML frontmatter, see `docs` module
///   automations/       proactive triggers (schedules, watchers)
///   state/             app-owned state (session links, event logs)
/// ```
#[derive(Clone, Debug)]
pub struct Home {
    pub root: PathBuf,
}

impl Home {
    pub fn locate() -> Self {
        let root = std::env::var_os("IV3_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| dirs::home_dir().expect("no home dir").join(".iv3"));
        Self { root }
    }

    pub fn skills(&self) -> PathBuf { self.root.join("skills") }
    pub fn tasks(&self) -> PathBuf { self.root.join("tasks") }
    pub fn docs(&self) -> PathBuf { self.root.join("docs") }
    pub fn automations(&self) -> PathBuf { self.root.join("automations") }
    pub fn state(&self) -> PathBuf { self.root.join("state") }

    /// Create the folder layout if missing. Never overwrites existing files.
    pub fn ensure(&self) -> Result<()> {
        for dir in [self.skills(), self.tasks(), self.docs(), self.automations(), self.state()] {
            fs::create_dir_all(dir)?;
        }
        let config = self.root.join("config.toml");
        if !config.exists() {
            fs::write(&config, DEFAULT_CONFIG)?;
        }
        let readme = self.docs().join("README.md");
        if !readme.exists() {
            fs::write(&readme, DOCS_README)?;
        }
        Ok(())
    }

    /// Skill names: every directory under skills/ that holds a SKILL.md.
    pub fn list_skills(&self) -> Vec<String> {
        let mut out: Vec<String> = fs::read_dir(self.skills())
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().join("SKILL.md").is_file())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        out.sort();
        out
    }
}

/// Written once into a fresh docs/ folder; explains the format to people and agents.
const DOCS_README: &str = r#"# Документы iv3

Сюда агенты и ты кладёте отчёты, спеки и HTML-разборы — они появляются в
разделе «Документы» сами. Агенты видят эту папку как `$IV3_DOCS`.
Подпапка = проект (`outpoll-web/p95.md`), или укажи `project` явно:

```markdown
---
title: Разбор /events/search
project: outpoll-web
author: codex          # claude | codex | ты | авто
session: <id сессии>   # откуда документ
task: <id задачи>      # к какой задаче
summary: одна строка о сути
---
```

В HTML то же самое — тегами `<meta name="iv3:project" content="…">`.
"#;

const DEFAULT_CONFIG: &str = r#"# iv3 settings (also editable in the app: Настройки)

[agents]
mode = "full"
default_provider = "claude"
autopilot_limit = 2

[changes]
model = "sonnet"
"#;
