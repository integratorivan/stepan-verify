use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::Result;

/// Layout of the shared stepan_verify folder (default `~/.stepan_verify`, override with `STEPAN_VERIFY_HOME`).
///
/// ```text
/// ~/.stepan_verify/
///   config.toml        app settings, project list
///   skills/<name>/     shared skills (SKILL.md), linked into every agent session
///   tasks/<id>.md      tasks: YAML frontmatter + markdown body
///   docs/              md/html documents by agents and you (agents get it as STEPAN_VERIFY_DOCS);
///                      metadata in YAML frontmatter, see `docs` module
///   memory/            what agents must remember: the user's corrections, don'ts,
///                      project facts (log.jsonl + snapshots, see `memory` module)
///   automations/       proactive triggers (schedules, watchers)
///   worktrees/<project>/<task-id>/   a task's own git worktree (branch stepan/<task-id>)
///   state/             app-owned state (session links, event logs);
///     stepan.db        open tabs, session index, last transcripts, feature usage (see `store`, `usage`)
/// ```
#[derive(Clone, Debug)]
pub struct Home {
    pub root: PathBuf,
}

impl Home {
    pub fn locate() -> Self {
        if let Some(root) = std::env::var_os("STEPAN_VERIFY_HOME") {
            return Self { root: PathBuf::from(root) };
        }
        let user = dirs::home_dir().expect("no home dir");
        let root = user.join(".stepan_verify");
        migrate_legacy(&user.join(".iv3"), &root);
        Self { root }
    }

    pub fn skills(&self) -> PathBuf { self.root.join("skills") }
    pub fn tasks(&self) -> PathBuf { self.root.join("tasks") }
    pub fn docs(&self) -> PathBuf { self.root.join("docs") }
    pub fn automations(&self) -> PathBuf { self.root.join("automations") }
    pub fn memory(&self) -> PathBuf { self.root.join("memory") }
    pub fn state(&self) -> PathBuf { self.root.join("state") }
    /// Scratch output per session (before/after media and the like).
    pub fn tmp(&self) -> PathBuf { self.root.join("tmp") }
    /// Separate checkouts of tasks: `worktrees/<project>/<task-id>` (see `worktree`).
    pub fn worktrees(&self) -> PathBuf { self.root.join("worktrees") }

    /// Create the folder layout if missing. Never overwrites existing files.
    pub fn ensure(&self) -> Result<()> {
        for dir in [self.skills(), self.tasks(), self.docs(), self.memory(), self.automations(), self.state()] {
            fs::create_dir_all(dir)?;
        }
        let config = self.root.join("config.toml");
        if !config.exists() {
            fs::write(&config, DEFAULT_CONFIG)?;
        }
        let readme = self.docs().join("README.md");
        // A README you edited stays; the stock one is upgraded.
        if fs::read_to_string(&readme).map_or(true, |t| t == DOCS_README_V1 || t == DOCS_README_V2) {
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

/// The app was called iv3 and kept its folder in `~/.iv3`. Move it to the new
/// place once and leave `~/.iv3` as a link to it, so an old build still
/// running (and the agents it started) keeps reading and writing the same files.
fn migrate_legacy(old: &Path, new: &Path) {
    let old_is_dir = fs::symlink_metadata(old).is_ok_and(|m| m.is_dir());
    if new.exists() || !old_is_dir {
        return;
    }
    match fs::rename(old, new) {
        Ok(()) => {
            #[cfg(unix)]
            if let Err(err) = std::os::unix::fs::symlink(new, old) {
                eprintln!("stepan_verify: link {} → {}: {err}", old.display(), new.display());
            }
        }
        Err(err) => eprintln!("stepan_verify: move {} → {}: {err}", old.display(), new.display()),
    }
}

/// Written into a fresh docs/ folder (and over the previous stock README);
/// explains the layout to people and agents.
const DOCS_README: &str = r#"# Документы Stepan

Сюда агенты и ты кладёте документы по проектам — они появляются в разделе
«Документы» сами. Агенты видят эту папку как `$STEPAN_VERIFY_DOCS`; каждая сессия,
запущенная из Stepan, получает правила ниже и то, что полка знает о проекте.

Документы — это то, что пригодится тебе: решения, разборы, передачи. Как устроен
проект, твои поправки и запреты агенты держат не здесь, а в памяти
(`~/.stepan_verify/memory`, раздел «Память», `stepan_verify memory …`).

```
<проект>/
  decisions/ГГГГ-ММ-ДД-x.md  решения: что выбрали, что отвергли, почему
  research/x.md              разборы: замеры, причины, варианты, вопрос тебе
  handoff/x.md               передача незаконченной работы
```

```markdown
---
title: Разбор /events/search
kind: research         # decision | research | handoff | briefing | note
status: open           # решения: proposed | accepted | superseded; вопросы: open | answered
question: Убрать fuzzy-поиск? Я бы оставил…
project: outpoll-web   # если файл не в папке проекта
author: codex          # claude | codex | ты | авто
session: <id сессии>   # откуда документ
task: <id задачи>      # к какой задаче
summary: одна строка о сути
---
```

`kind` можно не писать — он угадывается по папке. В HTML то же самое —
тегами `<meta name="stepan_verify:kind" content="…">`.
"#;

/// The stock README before memory took project facts over.
const DOCS_README_V2: &str = r#"# Документы Stepan

Сюда агенты и ты кладёте документы по проектам — они появляются в разделе
«Документы» сами. Агенты видят эту папку как `$STEPAN_VERIFY_DOCS`; каждая сессия,
запущенная из Stepan, получает правила ниже и то, что полка знает о проекте.

```
<проект>/
  project.md                 фактура: как запускать, где что, инварианты, грабли
  decisions/ГГГГ-ММ-ДД-x.md  решения: что выбрали, что отвергли, почему
  research/x.md              разборы: замеры, причины, варианты, вопрос тебе
  handoff/x.md               передача незаконченной работы
```

```markdown
---
title: Разбор /events/search
kind: research         # facts | decision | research | handoff | briefing | note
status: open           # решения: proposed | accepted | superseded; вопросы: open | answered
question: Убрать fuzzy-поиск? Я бы оставил…
project: outpoll-web   # если файл не в папке проекта
author: codex          # claude | codex | ты | авто
session: <id сессии>   # откуда документ
task: <id задачи>      # к какой задаче
summary: одна строка о сути
---
```

`kind` можно не писать — он угадывается по папке. В HTML то же самое —
тегами `<meta name="stepan_verify:kind" content="…">`.
"#;

/// The README `ensure` wrote before document kinds existed; replaced on start.
const DOCS_README_V1: &str = r#"# Документы stepan_verify

Сюда агенты и ты кладёте отчёты, спеки и HTML-разборы — они появляются в
разделе «Документы» сами. Агенты видят эту папку как `$STEPAN_VERIFY_DOCS`.
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

В HTML то же самое — тегами `<meta name="stepan_verify:project" content="…">`.
"#;

const DEFAULT_CONFIG: &str = r#"# stepan_verify settings (also editable in the app: Настройки)

[agents]
mode = "full"
default_provider = "claude"
autopilot_limit = 2

[changes]
model = "sonnet"
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_folder_moves_and_stays_reachable() {
        let root = std::env::temp_dir().join(format!("stepan_verify-migrate-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (old, new) = (root.join(".iv3"), root.join(".stepan_verify"));
        fs::create_dir_all(old.join("tasks")).unwrap();
        fs::write(old.join("tasks/t-1.md"), "задача").unwrap();
        migrate_legacy(&old, &new);
        assert_eq!(fs::read_to_string(new.join("tasks/t-1.md")).unwrap(), "задача");
        assert!(fs::symlink_metadata(&old).unwrap().file_type().is_symlink());
        assert_eq!(fs::read_to_string(old.join("tasks/t-1.md")).unwrap(), "задача", "old path still works");
        // Second start: nothing to do, nothing breaks.
        migrate_legacy(&old, &new);
        assert!(new.join("tasks/t-1.md").exists());
        let _ = fs::remove_dir_all(root);
    }
}
