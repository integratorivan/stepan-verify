//! Keeping the flow map in step with the code: what changed since the map
//! was last synced (git), which features that touches, and the agent skill
//! that tells every Claude / Codex session to update the map itself.

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use crate::{Home, flows::FlowMap};

// ------------------------------------------------------------------ git

fn git(repo: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(repo).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Current HEAD commit, if the project is a git repo with commits.
pub fn head(repo: &Path) -> Option<String> {
    git(repo, &["rev-parse", "HEAD"]).map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
}

/// Plugin and build noise: never a reason to resync.
fn noise(path: &str) -> bool {
    const DIRS: [&str; 9] = [".stepan_verify/", ".omc/", ".claude/", ".git/", "node_modules/", "target/", "dist/", ".next/", "build/"];
    DIRS.iter().any(|d| path.starts_with(d) || path.contains(&format!("/{d}"))) || path.ends_with(".DS_Store")
}

/// Files changed since `since` (committed after it, staged, unstaged, untracked)
/// and the number of commits on top of it. `None` when git cannot tell
/// (not a repo, or the commit is gone after a rebase).
pub fn changed_since(repo: &Path, since: &str) -> Option<(usize, Vec<String>)> {
    let commits = git(repo, &["rev-list", "--count", &format!("{since}..HEAD")])?.trim().parse().unwrap_or(0);
    let mut files = BTreeSet::new();
    for args in [
        vec!["diff", "--name-only", &format!("{since}..HEAD")[..]],
        vec!["diff", "--name-only"],
        vec!["diff", "--name-only", "--cached"],
        vec!["ls-files", "--others", "--exclude-standard"],
    ] {
        if let Some(out) = git(repo, &args) {
            files.extend(out.lines().map(str::trim).filter(|l| !l.is_empty() && !noise(l)).map(str::to_owned));
        }
    }
    Some((commits, files.into_iter().collect()))
}

/// Files the map talks about: step files and file-like entry points.
pub fn map_files(map: &FlowMap) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for f in &map.features {
        out.extend(f.steps().flat_map(|s| s.files.iter().map(|x| x.trim_start_matches("./").to_owned())));
        out.extend(f.entries.iter().filter(|e| e.contains('.') && !e.starts_with('/')).map(|e| e.trim_start_matches("./").to_owned()));
    }
    out
}

/// A changed file that looks like a user entry point (a route, page or screen),
/// so it may add or remove a feature even if the map does not know it yet.
pub fn looks_like_entry(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    let stem = name.split('.').next().unwrap_or(name);
    matches!(stem, "page" | "route" | "+page" | "layout")
        || name.ends_with("Screen.swift")
        || name.ends_with("Screen.tsx")
        || name.ends_with("Page.tsx")
        || path.contains("/pages/")
        || path.contains("/routes/")
        || path.contains("/screens/")
        || (path.starts_with("app/") || path.contains("/app/")) && (name.ends_with(".tsx") || name.ends_with(".jsx"))
}

/// How far the map lags behind the code.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Lag {
    /// Commits since the map was synced.
    pub commits: usize,
    /// Changed files the map refers to, or that look like entry points.
    pub relevant: Vec<String>,
    /// All changed files (for the sync pass).
    pub changed: Vec<String>,
}

impl Lag {
    pub fn matters(&self) -> bool {
        !self.relevant.is_empty()
    }
}

/// `None`: the map was never synced (no `synced_commit`) or git cannot tell.
pub fn lag(repo: &Path, map: &FlowMap) -> Option<Lag> {
    let since = map.synced_commit.as_deref()?;
    let (commits, changed) = changed_since(repo, since)?;
    Some(relevant_lag(map, commits, changed))
}

/// The pure part of [`lag`]: which changed files matter for this map.
pub fn relevant_lag(map: &FlowMap, commits: usize, changed: Vec<String>) -> Lag {
    let known = map_files(map);
    let relevant = changed
        .iter()
        .filter(|c| known.iter().any(|k| k == *c || k.ends_with(&format!("/{c}")) || c.ends_with(&format!("/{k}"))) || looks_like_entry(c))
        .cloned()
        .collect();
    Lag { commits, relevant, changed }
}

// ------------------------------------------------------------------ skill

pub const SKILL_NAME: &str = "stepan-verify-flows";

/// The skill every agent gets: read the map, keep it current after changing behaviour.
pub fn skill_text() -> String {
    r#"---
name: stepan-verify-flows
description: Карта пользовательских флоу проекта (Stepan) — навигация приложения и фичи в .stepan_verify/flows/ (app.yml + features/*.yml, свой язык). Используй, когда задача меняет то, что видит или делает пользователь (новый экран, переход, шаг, действие, удаление или переименование фичи), когда нужно понять, о какой фиче или экране говорит человек («та штука в левом сайдбаре»), и в конце такой задачи — чтобы обновить карту.
---

# Карта флоу проекта (Stepan)

Карта — общий словарь продукта для людей и агентов: **откуда стартует приложение и куда из каждого экрана можно перейти**, **какие фичи есть и какими шагами пользователь их проходит**, **что доказывает, что они работают**. Лежит в репозитории проекта, в папке `$STEPAN_VERIFY_FLOWS` (если задана), иначе `<корень репо>/.stepan_verify/flows/`:

- `app.yml` — навигация всего приложения: `start`, экраны (`screens`), переходы `-> Экран: действие (Фича)`;
- `features/<id>.yml` — одна фича на файл: название, область, статус, шаги, доказательства;
- `.sync.yml` — служебный, не трогай.

## Когда читать
- Человек называет фичу или экран своими словами — найди в `features/*.yml` по `feature` и `aliases`, экран — в `app.yml`; дальше по ссылкам в `[...]` в конце шага иди в код.
- Перед изменением пользовательского поведения — посмотри шаги затронутой фичи и переходы её экранов, чтобы не сломать соседние пути.

## Когда обновлять
Если задача поменяла пользовательское поведение, в **конце задачи** обнови карту:
1. Новый или удалённый экран / переход — поправь `app.yml` (экран, `route`, строки `-> …`). Переход, который принадлежит фиче, помечай `(Название фичи)` в конце.
2. Изменились шаги фичи — поправь её файл в `features/`. Новая фича — новый файл `features/<область>.<коротко>.yml` (латиница, точки и дефисы). Удалённая — удали файл.
3. `status`: `done` — работает целиком; `in_progress` — сделано частично; `planned` — описано, но в коде нет; `broken` — только если есть доказательство поломки (упавший тест, воспроизведение).
4. Не выдумывай: только то, что есть в коде после твоих изменений. Чужие фичи и экраны не трогай.
5. Запускал тест или снимал скриншот/видео, которые доказывают фичу — добавь строку в `proofs` с `covers` (номера шагов) и `at` (время ISO 8601).
6. Проверь: `stepan_verify flows check` (покажет ошибки с файлом и строкой). В ответе одной строкой скажи, что карта обновлена и что поменялось.

Если папки карты нет — ничего не создавай; упомяни, что карту можно построить в Stepan (экран «Флоу»).

## Язык (коротко; полностью — docs/FLOWS.md в Stepan)
`app.yml`:
```yaml
app: Shop
start: Запуск
areas: [Каталог, Оформление заказа]

screens:
  Запуск:
    -> Каталог: открыть приложение
  Каталог:
    route: /catalog
    -> Товар: клик по карточке
  Товар:
    route: /catalog/[id]
    -> Корзина: «В корзину» (Покупка товара)
  Корзина:
    kind: sheet          # page | dialog | sheet | tab | panel | external
    -> Оформление: «Оформить»
```
`features/checkout.promo.yml`:
```yaml
feature: Промокод при оформлении
area: Оформление заказа
status: done
summary: Вводишь промокод — сумма пересчитывается со скидкой
aliases: [купон, скидка]
entries: [/checkout, app/checkout/page.tsx]
starts_at: Оформление

steps:
  - Оформление: ввести промокод, выйти из поля -> сумма со скидкой  [Checkout, applyPromo, lib/checkout.ts]
  - Оформление: нажать «Оплатить» -> экран оплаты

proofs:
  - test: lib/checkout.test.ts::applies_promo  covers: 1  at: 2026-11-25T12:00:00Z
```
Шаг: `Экран: действие -> результат  [ссылки на код]` — экран как в `app.yml`, ссылки в квадратных скобках в самом конце, через два пробела (сущности и файлы). Несколько путей — вместо `steps:` блок `paths:` с названиями путей. Комментарий — `#` в начале строки или через два пробела после текста.
"#
    .to_owned()
}

/// Where the agents look for user skills: Claude Code and Codex (`~/.agents/skills`).
/// `STEPAN_VERIFY_SKILL_TARGETS` (`:`-separated dirs) overrides it — test instances
/// must not link into the real `~/.claude` / `~/.agents`.
pub fn skill_targets() -> Vec<PathBuf> {
    if let Some(list) = std::env::var_os("STEPAN_VERIFY_SKILL_TARGETS") {
        return std::env::split_paths(&list).filter(|p| !p.as_os_str().is_empty()).collect();
    }
    dirs::home_dir().map(|h| vec![h.join(".claude/skills"), h.join(".agents/skills")]).unwrap_or_default()
}

#[derive(Debug, Default, PartialEq)]
pub struct SkillReport {
    /// Links that now point at our skill.
    pub linked: Vec<PathBuf>,
    /// Places left alone because something else lives there.
    pub warnings: Vec<String>,
}

/// Our copy: `<home>/skills/stepan-verify-flows/`.
pub fn skill_dir(home: &Home) -> PathBuf {
    home.skills().join(SKILL_NAME)
}

/// Write (or refresh) the skill and link it into every target; idempotent.
/// A target that is not our symlink (a real folder, or a link elsewhere) is never touched.
pub fn install_skill(home: &Home, targets: &[PathBuf]) -> SkillReport {
    install_bundled(home, SKILL_NAME, &skill_text(), targets)
}

/// Remove only our own links; the skill file in our home stays.
pub fn uninstall_skill(home: &Home, targets: &[PathBuf]) -> SkillReport {
    uninstall_bundled(home, SKILL_NAME, targets)
}

/// Any skill Stepan ships: `SKILL.md` written to `<home>/skills/<name>/` and that
/// folder symlinked into every target; idempotent. A target that is not our
/// symlink (a real folder, or a link elsewhere) is never touched.
pub fn install_bundled(home: &Home, name: &str, text: &str, targets: &[PathBuf]) -> SkillReport {
    let mut report = SkillReport::default();
    let dir = home.skills().join(name);
    let file = dir.join("SKILL.md");
    if fs::read_to_string(&file).ok().as_deref() != Some(text) {
        if let Err(err) = fs::create_dir_all(&dir).and_then(|_| fs::write(&file, text)) {
            report.warnings.push(format!("{}: {err}", file.display()));
            return report;
        }
    }
    for target in targets {
        let link = target.join(name);
        match fs::symlink_metadata(&link) {
            Ok(meta) if meta.file_type().is_symlink() => {
                if points_to(&link, &dir) {
                    report.linked.push(link);
                } else {
                    report.warnings.push(format!("{} — ссылка ведёт не на скилл Stepan, не трогаю", link.display()));
                }
            }
            Ok(_) => report.warnings.push(format!("{} уже существует (не ссылка Stepan), не трогаю", link.display())),
            Err(_) => {
                let made = fs::create_dir_all(target).and_then(|_| symlink(&dir, &link));
                match made {
                    Ok(()) => report.linked.push(link),
                    Err(err) => report.warnings.push(format!("{}: {err}", link.display())),
                }
            }
        }
    }
    report
}

/// Remove only our own links to a bundled skill; its file in our home stays.
pub fn uninstall_bundled(home: &Home, name: &str, targets: &[PathBuf]) -> SkillReport {
    let mut report = SkillReport::default();
    let dir = home.skills().join(name);
    for target in targets {
        let link = target.join(name);
        let ours = fs::symlink_metadata(&link).is_ok_and(|m| m.file_type().is_symlink()) && points_to(&link, &dir);
        if ours {
            if let Err(err) = fs::remove_file(&link) {
                report.warnings.push(format!("{}: {err}", link.display()));
            }
        } else if fs::symlink_metadata(&link).is_ok() {
            report.warnings.push(format!("{} не ссылка Stepan, не удаляю", link.display()));
        }
    }
    report
}

fn points_to(link: &Path, dir: &Path) -> bool {
    let Ok(target) = fs::read_link(link) else { return false };
    let target = if target.is_relative() { link.parent().unwrap_or(Path::new("/")).join(target) } else { target };
    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    canon(&target) == canon(dir)
}

#[cfg(unix)]
fn symlink(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(src, dst)
}

#[cfg(not(unix))]
fn symlink(_: &Path, _: &Path) -> std::io::Result<()> {
    Err(std::io::Error::other("symlinks are unix-only here"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flows::{Feature, FlowPath, Step};

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("sv-sync-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn map() -> FlowMap {
        FlowMap {
            features: vec![Feature {
                id: "checkout.promo".into(),
                entries: vec!["/checkout".into(), "app/checkout/page.tsx".into()],
                paths: vec![FlowPath { steps: vec![Step { files: vec!["lib/checkout.ts".into()], ..Default::default() }], ..Default::default() }],
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn changed_files_that_matter() {
        let m = map();
        let lag = relevant_lag(&m, 3, vec!["lib/checkout.ts".into(), "README.md".into(), "app/wishlist/page.tsx".into(), "lib/util.ts".into()]);
        assert_eq!(lag.commits, 3);
        assert_eq!(lag.relevant, vec!["lib/checkout.ts".to_owned(), "app/wishlist/page.tsx".to_owned()]);
        assert!(lag.matters());
        assert!(!relevant_lag(&m, 1, vec!["README.md".into()]).matters());
        assert!(noise(".stepan_verify/flows.yaml") && noise("apps/web/node_modules/x.js") && !noise("lib/a.ts"));
    }

    #[test]
    fn git_diff_since_synced_commit() {
        let repo = tmp("git");
        let run = |args: &[&str]| {
            Command::new("git").arg("-C").arg(&repo).args(args).env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@t").env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@t").output().unwrap()
        };
        run(&["init", "-q"]);
        fs::create_dir_all(repo.join("lib")).unwrap();
        fs::write(repo.join("lib/checkout.ts"), "a").unwrap();
        run(&["add", "-A"]);
        run(&["commit", "-qm", "one"]);
        let mut m = map();
        m.synced_commit = head(&repo);
        assert!(m.synced_commit.is_some());
        assert_eq!(lag(&repo, &m).unwrap(), Lag::default(), "nothing changed yet");
        fs::write(repo.join("lib/checkout.ts"), "b").unwrap();
        run(&["commit", "-qam", "two"]);
        fs::create_dir_all(repo.join("app/wishlist")).unwrap();
        fs::write(repo.join("app/wishlist/page.tsx"), "new").unwrap();
        fs::create_dir_all(repo.join(".stepan_verify")).unwrap();
        fs::write(repo.join(".stepan_verify/flows.yaml"), "x").unwrap();
        let l = lag(&repo, &m).unwrap();
        assert_eq!(l.commits, 1);
        assert_eq!(l.relevant, vec!["app/wishlist/page.tsx".to_owned(), "lib/checkout.ts".to_owned()]);
        let touched = m.touched(&l.changed, &[]);
        assert_eq!(touched.len(), 1, "the promo feature goes through lib/checkout.ts");
        let _ = fs::remove_dir_all(repo);
    }

    #[test]
    fn skill_links_installed_and_removed_safely() {
        let root = tmp("skill");
        let home = Home { root: root.join("home") };
        let claude = root.join("claude/skills");
        let agents = root.join("agents/skills");
        let foreign = root.join("foreign/skills");
        fs::create_dir_all(foreign.join(SKILL_NAME)).unwrap();
        fs::write(foreign.join(SKILL_NAME).join("SKILL.md"), "someone else's").unwrap();

        let r = install_skill(&home, &[claude.clone(), agents.clone(), foreign.clone()]);
        assert_eq!(r.linked.len(), 2);
        assert_eq!(r.warnings.len(), 1, "a real folder is left alone: {:?}", r.warnings);
        assert!(fs::read_to_string(claude.join(SKILL_NAME).join("SKILL.md")).unwrap().starts_with("---\nname: stepan-verify-flows"));
        assert!(!skill_text().contains("disable-model-invocation"));
        assert_eq!(install_skill(&home, &[claude.clone(), agents.clone()]).linked.len(), 2, "idempotent");

        let u = uninstall_skill(&home, &[claude.clone(), agents.clone(), foreign.clone()]);
        assert!(fs::symlink_metadata(claude.join(SKILL_NAME)).is_err() && fs::symlink_metadata(agents.join(SKILL_NAME)).is_err());
        assert_eq!(fs::read_to_string(foreign.join(SKILL_NAME).join("SKILL.md")).unwrap(), "someone else's");
        assert_eq!(u.warnings.len(), 1);
        assert!(skill_dir(&home).join("SKILL.md").exists(), "our copy stays");
        let _ = fs::remove_dir_all(root);
    }
}
