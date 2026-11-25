//! Updates from GitHub Releases (the repository is public).
//!
//! The app asks `releases/latest` at start and every few hours. A newer
//! version shows a card in the sidebar; «Обновить» downloads the zip for this
//! Mac's architecture, swaps the running `Stepan.app` for the new one and
//! relaunches. A bare `cargo run` binary has no bundle to swap and opens the
//! release page instead. Network and archive work go through `curl` and
//! `ditto`, both part of macOS.

use std::{
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use anyhow::{Context as _, Result, anyhow, bail};
use gpui_kit::*;
use serde::Deserialize;

pub const REPO: &str = "integratorivan/stepan-verify";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// How often a running app looks again.
const RECHECK: Duration = Duration::from_secs(6 * 3600);

/// Zip name in a release for this Mac: `stepan-macos-arm64.zip` / `stepan-macos-x86_64.zip`.
pub fn asset_name() -> String {
    let arch = if cfg!(target_arch = "aarch64") { "arm64" } else { "x86_64" };
    format!("stepan-macos-{arch}.zip")
}

#[derive(Clone, Debug)]
pub struct Release {
    pub version: semver::Version,
    pub page: String,
    /// Download URL of the zip for this architecture, when the release has one.
    pub zip: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub enum State {
    #[default]
    Idle,
    Checking,
    UpToDate,
    Available(Release),
    Installing(Release),
    Failed(String),
}

#[derive(Default)]
pub struct Updates {
    pub state: State,
    _loop: Option<Task<()>>,
}

impl Global for Updates {}

pub fn state(cx: &App) -> State {
    cx.try_global::<Updates>().map(|u| u.state.clone()).unwrap_or_default()
}

fn set(state: State, cx: &mut App) {
    cx.update_global::<Updates, _>(|u, _| u.state = state);
    cx.refresh_windows();
}

/// Start the periodic check (when enabled in settings).
pub fn init(cx: &mut App) {
    cx.set_global(Updates::default());
    let task = cx.spawn(async move |cx| {
        // Let the window settle before going to the network.
        cx.background_executor().timer(Duration::from_secs(4)).await;
        loop {
            let enabled = cx.update(|cx| crate::settings::config(cx).updates.check);
            if enabled {
                cx.update(check);
            }
            cx.background_executor().timer(RECHECK).await;
        }
    });
    cx.update_global::<Updates, _>(|u, _| u._loop = Some(task));
}

/// Ask GitHub for the latest release now.
pub fn check(cx: &mut App) {
    if matches!(state(cx), State::Checking | State::Installing(_)) {
        return;
    }
    set(State::Checking, cx);
    cx.spawn(async move |cx| {
        let result = cx.background_spawn(async { latest() }).await;
        cx.update(|cx| {
            let next = match result {
                Ok(Some(r)) if is_newer(&r.version) => State::Available(r),
                Ok(_) => State::UpToDate,
                Err(err) => State::Failed(format!("{err:#}")),
            };
            set(next, cx);
        });
    })
    .detach();
}

fn is_newer(v: &semver::Version) -> bool {
    semver::Version::parse(VERSION).is_ok_and(|current| *v > current)
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
}

fn latest() -> Result<Option<Release>> {
    let out = Command::new("curl")
        .args(["-fsSL", "--max-time", "20", "-H", "Accept: application/vnd.github+json", "-H", "User-Agent: stepan-updater"])
        .arg(format!("https://api.github.com/repos/{REPO}/releases/latest"))
        .output()
        .context("curl")?;
    if !out.status.success() {
        // 404: the repository has no releases yet.
        let err = String::from_utf8_lossy(&out.stderr);
        if err.contains("404") {
            return Ok(None);
        }
        bail!("GitHub не ответил: {}", err.trim());
    }
    parse_release(&out.stdout)
}

fn parse_release(json: &[u8]) -> Result<Option<Release>> {
    let r: GhRelease = serde_json::from_slice(json).context("ответ GitHub")?;
    if r.draft || r.prerelease {
        return Ok(None);
    }
    let version = semver::Version::parse(r.tag_name.trim_start_matches('v')).with_context(|| format!("тег {}", r.tag_name))?;
    let want = asset_name();
    Ok(Some(Release {
        version,
        page: r.html_url,
        zip: r.assets.into_iter().find(|a| a.name == want).map(|a| a.browser_download_url),
    }))
}

/// The `.app` this process runs from, if any.
pub fn running_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    exe.ancestors().find(|p| p.extension().is_some_and(|e| e == "app")).map(Path::to_owned)
}

/// «Обновить»: swap the bundle and relaunch, or open the release page.
pub fn install(cx: &mut App) {
    let State::Available(release) = state(cx) else { return };
    let (Some(bundle), Some(zip)) = (running_bundle(), release.zip.clone()) else {
        let _ = Command::new("open").arg(&release.page).spawn();
        return;
    };
    set(State::Installing(release.clone()), cx);
    cx.spawn(async move |cx| {
        let result = cx.background_spawn(async move { replace_bundle(&bundle, &zip).map(|_| bundle) }).await;
        cx.update(|cx| match result {
            Ok(bundle) => {
                // A fresh process from the new bundle once this one is gone.
                let _ = Command::new("/bin/sh").arg("-c").arg(format!("sleep 1; open {}", shell_quote(&bundle))).spawn();
                cx.quit();
            }
            Err(err) => set(State::Failed(format!("{err:#}")), cx),
        });
    })
    .detach();
}

fn shell_quote(p: &Path) -> String {
    format!("'{}'", p.display().to_string().replace('\'', r"'\''"))
}

/// Download, unpack next to the bundle, then swap: old → backup, new → place.
fn replace_bundle(bundle: &Path, url: &str) -> Result<()> {
    let parent = bundle.parent().ok_or_else(|| anyhow!("у {} нет папки", bundle.display()))?;
    let work = std::env::temp_dir().join(format!("iv3-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work)?;
    let zip = work.join("iv3.zip");
    run(Command::new("curl").args(["-fsSL", "--max-time", "600", "-o"]).arg(&zip).arg(url), "скачать релиз")?;
    let unpacked = work.join("unpacked");
    run(Command::new("ditto").args(["-x", "-k"]).arg(&zip).arg(&unpacked), "распаковать")?;
    let fresh = std::fs::read_dir(&unpacked)?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "app"))
        .ok_or_else(|| anyhow!("в архиве нет .app"))?;
    if !fresh.join("Contents/MacOS/iv3").is_file() {
        bail!("в архиве повреждённое приложение");
    }
    // Same volume as the bundle, so both moves are renames.
    let staged = parent.join(".iv3-update.app");
    let backup = parent.join(".iv3-previous.app");
    let _ = std::fs::remove_dir_all(&staged);
    let _ = std::fs::remove_dir_all(&backup);
    run(Command::new("ditto").arg(&fresh).arg(&staged), "подготовить новую версию")
        .with_context(|| format!("нет прав на запись в {}", parent.display()))?;
    std::fs::rename(bundle, &backup).with_context(|| format!("не удалось убрать {}", bundle.display()))?;
    if let Err(err) = std::fs::rename(&staged, bundle) {
        let _ = std::fs::rename(&backup, bundle);
        return Err(err).context("не удалось поставить новую версию");
    }
    let _ = std::fs::remove_dir_all(&backup);
    let _ = std::fs::remove_dir_all(&work);
    Ok(())
}

fn run(cmd: &mut Command, what: &str) -> Result<()> {
    let out = cmd.output().with_context(|| what.to_owned())?;
    if !out.status.success() {
        bail!("{what}: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[core::prelude::v1::test]
    fn release_json() {
        let json = format!(
            r#"{{"tag_name":"v0.2.0","html_url":"https://github.com/{REPO}/releases/tag/v0.2.0","body":"notes","draft":false,"prerelease":false,
               "assets":[{{"name":"stepan-macos-arm64.zip","browser_download_url":"https://x/arm"}},{{"name":"stepan-macos-x86_64.zip","browser_download_url":"https://x/intel"}}]}}"#
        );
        let r = parse_release(json.as_bytes()).unwrap().unwrap();
        assert_eq!(r.version, semver::Version::new(0, 2, 0));
        let want = if cfg!(target_arch = "aarch64") { "https://x/arm" } else { "https://x/intel" };
        assert_eq!(r.zip.as_deref(), Some(want));
        assert!(is_newer(&semver::Version::new(99, 0, 0)));
        assert!(!is_newer(&semver::Version::parse(VERSION).unwrap()));
        let pre = r#"{"tag_name":"v9.0.0","html_url":"u","prerelease":true}"#;
        assert!(parse_release(pre.as_bytes()).unwrap().is_none());
    }

    #[core::prelude::v1::test]
    fn swaps_the_bundle_in_place() {
        let root = std::env::temp_dir().join(format!("iv3-swap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let make = |dir: &Path, text: &str| {
            std::fs::create_dir_all(dir.join("Contents/MacOS")).unwrap();
            std::fs::write(dir.join("Contents/MacOS/iv3"), text).unwrap();
        };
        let installed = root.join("Applications/Stepan.app");
        make(&installed, "old");
        let release = root.join("release/Stepan.app");
        make(&release, "new");
        let zip = root.join("stepan-macos-arm64.zip");
        run(Command::new("ditto").args(["-c", "-k", "--keepParent"]).arg(&release).arg(&zip), "zip").unwrap();

        replace_bundle(&installed, &format!("file://{}", zip.display())).unwrap();
        assert_eq!(std::fs::read_to_string(installed.join("Contents/MacOS/iv3")).unwrap(), "new");
        let left: Vec<_> = std::fs::read_dir(root.join("Applications")).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(left.len(), 1, "no staged or backup copies left: {left:?}");

        // A broken archive leaves the installed app alone.
        std::fs::write(&zip, "not a zip").unwrap();
        assert!(replace_bundle(&installed, &format!("file://{}", zip.display())).is_err());
        assert_eq!(std::fs::read_to_string(installed.join("Contents/MacOS/iv3")).unwrap(), "new");
        let _ = std::fs::remove_dir_all(root);
    }
}
