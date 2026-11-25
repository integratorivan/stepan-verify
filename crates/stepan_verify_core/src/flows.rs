//! The project's user-flow map: which features exist, how a user reaches
//! them, and what proves they work («что есть → как дойти → чем доказано»),
//! plus the navigation of the whole app (start → screens → transitions).
//!
//! Lives in the project repo as `.stepan_verify/flows/` — `app.yml` and
//! `features/*.yml` in the flow language ([`crate::flowdsl`], `docs/FLOWS.md`),
//! so agents and people read and edit it as the shared vocabulary for the
//! product. Sessions propose changes ([`FlowProposal`]); you accept them and
//! they are applied with [`FlowMap::apply`]. The older single
//! `.stepan_verify/flows.yaml` is migrated on first load.

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::Home;

/// The flows folder (flow language), relative to the repo root.
pub const DIR: &str = crate::flowdsl::DIR;
/// The pre-language single-file map; migrated to [`DIR`] on first load.
pub const FILE: &str = ".stepan_verify/flows.yaml";

/// The legacy single-file map. Prefer [`dir`] / [`exists`] / [`stamp`].
pub fn path(repo: &Path) -> PathBuf {
    repo.join(FILE)
}

/// The flows folder of a repo.
pub fn dir(repo: &Path) -> PathBuf {
    repo.join(DIR)
}

/// The repo has a map (either form).
pub fn exists(repo: &Path) -> bool {
    dir(repo).is_dir() || path(repo).is_file()
}

/// Newest modification among the map's files: «the map changed on disk».
pub fn stamp(repo: &Path) -> Option<SystemTime> {
    let root = dir(repo);
    let mut newest = modified(&path(repo));
    let mut visit = |p: &Path| {
        if let Some(m) = modified(p) {
            newest = Some(newest.map_or(m, |n| n.max(m)));
        }
    };
    for sub in [root.clone(), root.join(crate::flowdsl::FEATURES)] {
        visit(&sub);
        if let Ok(rd) = fs::read_dir(&sub) {
            for e in rd.flatten() {
                visit(&e.path());
            }
        }
    }
    newest
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FlowMap {
    pub version: u32,
    pub project: String,
    /// Product areas features group into: «Награды», «Авторизация».
    pub areas: Vec<Area>,
    pub features: Vec<Feature>,
    /// Entry points deliberately left off the map (debug pages, redirects), with why.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ignored: Vec<Ignored>,
    /// HEAD at the last full build / sync: what «the map lags behind» is measured from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synced_commit: Option<String>,
    pub updated_at: Option<DateTime<Utc>>,
    /// Navigation of the whole app: where it starts and how screens connect (`app.yml`).
    #[serde(skip_serializing_if = "AppNav::is_empty")]
    pub nav: AppNav,
}

/// The whole app as a graph: an explicit start, screens, transitions between them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppNav {
    /// The app's display name.
    pub app: String,
    /// The first screen: «Запуск».
    pub start: String,
    pub screens: Vec<Screen>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Screen {
    pub name: String,
    /// page | dialog | sheet | tab | panel | external.
    pub kind: String,
    pub route: String,
    pub about: String,
    pub edges: Vec<Edge>,
}

/// «Из этого экрана можно перейти туда, сделав вот это» (and the feature it belongs to).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Edge {
    pub to: String,
    pub action: String,
    /// Feature title this transition belongs to, if any.
    pub feature: String,
}

impl AppNav {
    pub fn is_empty(&self) -> bool {
        self.screens.is_empty() && self.start.is_empty()
    }

    pub fn screen(&self, name: &str) -> Option<&Screen> {
        self.screens.iter().find(|s| s.name == name)
    }

    /// Transitions that lead into a screen: (from, edge).
    pub fn incoming<'a>(&'a self, name: &'a str) -> impl Iterator<Item = (&'a Screen, &'a Edge)> + 'a {
        self.screens.iter().flat_map(move |s| s.edges.iter().filter(move |e| e.to == name).map(move |e| (s, e)))
    }

    /// Screens by distance from the start (BFS levels), in declaration order
    /// within a level; screens the start does not reach come last, one level each
    /// group of unreachable roots.
    pub fn levels(&self) -> Vec<Vec<usize>> {
        let n = self.screens.len();
        let index = |name: &str| self.screens.iter().position(|s| s.name == name);
        let mut level = vec![usize::MAX; n];
        let mut out: Vec<Vec<usize>> = Vec::new();
        let mut roots: Vec<usize> = index(&self.start).into_iter().collect();
        if roots.is_empty() && n > 0 {
            roots.push(0);
        }
        loop {
            let mut frontier: Vec<usize> = roots.drain(..).filter(|&i| level[i] == usize::MAX).collect();
            let base = out.len();
            let mut depth = 0;
            while !frontier.is_empty() {
                for &i in &frontier {
                    level[i] = base + depth;
                }
                if out.len() <= base + depth {
                    out.push(Vec::new());
                }
                out[base + depth].extend(frontier.iter().copied());
                let mut next = Vec::new();
                for &i in &frontier {
                    for e in &self.screens[i].edges {
                        if let Some(j) = index(&e.to) {
                            if level[j] == usize::MAX && !next.contains(&j) && !frontier.contains(&j) {
                                next.push(j);
                            }
                        }
                    }
                }
                next.sort();
                frontier = next;
                depth += 1;
            }
            // Unreached screens: start again from the first of them.
            match (0..n).find(|&i| level[i] == usize::MAX) {
                Some(i) => roots.push(i),
                None => break,
            }
        }
        for l in &mut out {
            l.sort();
        }
        out
    }

    /// Screens a feature goes through: its `starts_at`, every step screen and
    /// the targets of edges tagged with its title.
    pub fn feature_screens(&self, f: &Feature) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut add = |s: &str| {
            if !s.is_empty() && !out.iter().any(|x| x == s) {
                out.push(s.to_owned());
            }
        };
        add(&f.starts_at);
        for s in f.steps() {
            add(&s.screen);
        }
        for s in &self.screens {
            for e in s.edges.iter().filter(|e| e.feature == f.title) {
                add(&s.name);
                add(&e.to);
            }
        }
        out
    }
}

/// An entry point the map does not describe on purpose.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Ignored {
    /// Route / screen id or file, as in the coverage checklist.
    pub entry: String,
    pub reason: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Area {
    pub id: String,
    pub title: String,
}

/// Something a user can do or see, named the way a user would name it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Feature {
    pub id: String,
    pub title: String,
    pub area: String,
    pub summary: String,
    /// Other names people use for it («та штука в левом сайдбаре»).
    pub aliases: Vec<String>,
    /// Entry points (routes, screens, files) this feature starts from —
    /// how the coverage check knows the feature accounts for them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<String>,
    /// done | in_progress | planned | broken, or `draft` while only inventoried.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub status: String,
    /// The screen (from the app navigation) the feature starts on.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub starts_at: String,
    pub paths: Vec<FlowPath>,
    pub proofs: Vec<Proof>,
}

/// One way to reach / use a feature: a sequence of steps.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FlowPath {
    pub id: String,
    pub title: String,
    pub steps: Vec<Step>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Step {
    pub id: String,
    /// Where the user is: a screen, dialog, page.
    pub screen: String,
    /// What they do: «Нажать «Выбрать Джокера»».
    pub action: String,
    /// What they see after.
    pub result: String,
    /// Code entities this step goes through (component, function, store names).
    pub entities: Vec<String>,
    /// Repo-relative files behind those entities.
    pub files: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProofKind {
    Test,
    Screenshot,
    Video,
    Cli,
    #[default]
    Manual,
}

/// What proves a feature works: a test, a screenshot, a CLI scenario.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Proof {
    pub kind: ProofKind,
    pub title: String,
    /// Test file / command / media path.
    pub reference: String,
    /// Step ids it covers; empty = the whole feature.
    pub covers: Vec<String>,
    /// When it last passed / was captured.
    pub at: Option<DateTime<Utc>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Freshness {
    Fresh,
    /// Code behind the covered steps changed after the proof was taken.
    Stale,
    /// No proof at all.
    Missing,
}

impl FlowMap {
    /// The map of a repo; `None` when it has none yet. Reads the flow
    /// language folder; an older `flows.yaml` is migrated into it once
    /// (and kept as `flows.yaml.bak`). Syntax errors fail with file:line.
    pub fn load(repo: &Path) -> Result<Option<FlowMap>> {
        if let Some((map, diags)) = crate::flowdsl::compile(repo)? {
            if let Some(errors) = crate::flowdsl::errors_text(&diags) {
                return Err(anyhow::anyhow!("карта флоу с ошибками ({}):\n{errors}", dir(repo).display()));
            }
            return Ok(Some(map));
        }
        let p = path(repo);
        if !p.exists() {
            return Ok(None);
        }
        let text = fs::read_to_string(&p).with_context(|| format!("read {}", p.display()))?;
        let map: FlowMap = serde_yaml::from_str(&text).with_context(|| format!("parse {}", p.display()))?;
        map.migrate(repo)?;
        Ok(crate::flowdsl::compile(repo)?.map(|(m, _)| m).or(Some(map)))
    }

    /// One-time move from `flows.yaml` to the flow language folder.
    fn migrate(&self, repo: &Path) -> Result<()> {
        crate::flowdsl::write(repo, self, true)?;
        let old = path(repo);
        fs::rename(&old, old.with_extension("yaml.bak")).with_context(|| format!("rename {}", old.display()))?;
        Ok(())
    }

    /// Write the map as flow files (only files whose meaning changed).
    pub fn save(&self, repo: &Path) -> Result<()> {
        crate::flowdsl::write(repo, self, false)
    }

    pub fn feature(&self, id: &str) -> Option<&Feature> {
        self.features.iter().find(|f| f.id == id)
    }

    /// Features whose steps go through any of `files` (repo-relative) or `entities`.
    pub fn touched(&self, files: &[String], entities: &[String]) -> Vec<Touched> {
        let mut out = Vec::new();
        for f in &self.features {
            for p in &f.paths {
                let steps: Vec<String> = p
                    .steps
                    .iter()
                    .filter(|s| {
                        s.files.iter().any(|x| files.iter().any(|c| same_file(x, c)))
                            || s.entities.iter().any(|e| entities.iter().any(|c| c == e))
                    })
                    .map(|s| s.id.clone())
                    .collect();
                if !steps.is_empty() {
                    out.push(Touched { feature: f.id.clone(), path: p.id.clone(), steps });
                }
            }
        }
        out
    }

    /// Apply an accepted proposal: upserts replace features by id, removals drop them.
    pub fn apply(&mut self, proposal: &FlowProposal) {
        for a in &proposal.areas {
            match self.areas.iter_mut().find(|x| x.id == a.id) {
                Some(x) => *x = a.clone(),
                None => self.areas.push(a.clone()),
            }
        }
        for f in &proposal.upsert {
            match self.features.iter_mut().find(|x| x.id == f.id) {
                Some(x) => *x = f.clone(),
                None => self.features.push(f.clone()),
            }
        }
        self.features.retain(|f| !proposal.remove.contains(&f.id));
        for i in &proposal.ignored {
            if !self.ignored.iter().any(|x| x.entry == i.entry) {
                self.ignored.push(i.clone());
            }
        }
        if self.version == 0 {
            self.version = 1;
        }
        self.updated_at = Some(Utc::now());
    }

    /// Replace the proofs of the listed features (a proofs pass); other fields stay.
    pub fn apply_proofs(&mut self, part: &ProofsPart) {
        for fp in &part.features {
            if let Some(f) = self.features.iter_mut().find(|f| f.id == fp.feature) {
                f.proofs = fp.proofs.clone();
            }
        }
        self.updated_at = Some(Utc::now());
    }

    /// Features still waiting for their paths.
    pub fn drafts(&self) -> impl Iterator<Item = &Feature> {
        self.features.iter().filter(|f| f.status == "draft")
    }

    /// Split features into detail batches: by area, at most `size` per batch.
    pub fn batches(&self, ids: &[String], size: usize) -> Vec<Batch> {
        let size = size.max(1);
        let mut by_area: Vec<(String, Vec<String>)> = Vec::new();
        for f in self.features.iter().filter(|f| ids.contains(&f.id)) {
            match by_area.iter_mut().find(|(a, _)| *a == f.area) {
                Some((_, list)) => list.push(f.id.clone()),
                None => by_area.push((f.area.clone(), vec![f.id.clone()])),
            }
        }
        let mut out = Vec::new();
        for (area, list) in by_area {
            let title = self.areas.iter().find(|a| a.id == area).map(|a| a.title.clone()).unwrap_or_else(|| if area.is_empty() { "Без области".into() } else { area.clone() });
            let chunks: Vec<&[String]> = list.chunks(size).collect();
            let n = chunks.len();
            for (i, chunk) in chunks.into_iter().enumerate() {
                let suffix = if n > 1 { format!(" · {}/{}", i + 1, n) } else { String::new() };
                out.push(Batch { id: format!("{}-{}", if area.is_empty() { "misc" } else { &area }, i + 1), title: format!("{title}{suffix}"), features: chunk.to_vec() });
            }
        }
        out
    }
}

/// A slice of features one detail agent describes.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Batch {
    pub id: String,
    pub title: String,
    pub features: Vec<String>,
}

/// Result of a proofs pass: the full proof list per feature.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProofsPart {
    pub features: Vec<FeatureProofs>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FeatureProofs {
    pub feature: String,
    pub proofs: Vec<Proof>,
}

impl Feature {
    /// Is the feature proven, and is the proof younger than the code behind it?
    pub fn freshness(&self, repo: &Path) -> Freshness {
        if self.proofs.is_empty() {
            return Freshness::Missing;
        }
        let stale = self.proofs.iter().all(|proof| {
            let Some(at) = proof.at else { return true };
            let files: BTreeSet<&String> = self
                .paths
                .iter()
                .flat_map(|p| &p.steps)
                .filter(|s| proof.covers.is_empty() || proof.covers.contains(&s.id))
                .flat_map(|s| &s.files)
                .collect();
            files.iter().any(|f| modified(&repo.join(f)).is_some_and(|m| DateTime::<Utc>::from(m) > at))
        });
        if stale { Freshness::Stale } else { Freshness::Fresh }
    }

    pub fn steps(&self) -> impl Iterator<Item = &Step> {
        self.paths.iter().flat_map(|p| &p.steps)
    }
}

/// A session went through these steps of a feature's path.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Touched {
    pub feature: String,
    pub path: String,
    pub steps: Vec<String>,
}

/// What a session's reviewer suggests changing in the map.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FlowProposal {
    pub areas: Vec<Area>,
    /// New or rewritten features (whole feature, matched by id).
    pub upsert: Vec<Feature>,
    /// Feature ids that no longer exist.
    pub remove: Vec<String>,
    /// One line per change, for the review UI: «+ Полёт карты в хаб».
    pub notes: Vec<String>,
    /// Entry points to leave off the map, with a reason.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ignored: Vec<Ignored>,
}

impl FlowProposal {
    pub fn is_empty(&self) -> bool {
        self.areas.is_empty() && self.upsert.is_empty() && self.remove.is_empty() && self.ignored.is_empty()
    }
}

// ------------------------------------------------------------------ sessions

/// What one session's latest «Изменения» run says about the flow map: the
/// steps it went through and the changes it proposes. Read from
/// `state/changes/<session>/runs/<n>/report.json` (field `flows`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionFlows {
    /// Session key (the `state/changes/<key>` folder).
    pub session: String,
    pub run: usize,
    pub title: String,
    pub at: Option<DateTime<Utc>>,
    pub touched: Vec<Touched>,
    pub proposal: FlowProposal,
}

impl SessionFlows {
    /// Stable id of this proposal for accept / reject bookkeeping.
    pub fn id(&self) -> String {
        format!("{}#{}", self.session, self.run)
    }
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct ReportFlows {
    touched: Vec<Touched>,
    proposal: FlowProposal,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct ReportHead {
    title: String,
    flows: Option<ReportFlows>,
}

/// The newest run (with a `flows` section) of every session that worked in `repo`, newest first.
pub fn session_flows(home: &Home, repo: &Path) -> Vec<SessionFlows> {
    let root = home.state().join("changes");
    let Ok(sessions) = fs::read_dir(&root) else { return Vec::new() };
    let repo = repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf());
    let mut out = Vec::new();
    for entry in sessions.flatten() {
        let session = entry.file_name().to_string_lossy().into_owned();
        let Ok(runs) = fs::read_dir(entry.path().join("runs")) else { continue };
        let mut runs: Vec<(usize, PathBuf)> =
            runs.flatten().filter_map(|r| Some((r.file_name().to_str()?.parse().ok()?, r.path()))).collect();
        runs.sort_by(|a, b| b.0.cmp(&a.0));
        for (n, dir) in runs {
            let Some(cwd) = run_project(&dir) else { continue };
            if cwd.canonicalize().unwrap_or(cwd) != repo {
                break;
            }
            let Ok(text) = fs::read_to_string(dir.join("report.json")) else { continue };
            let Ok(head) = serde_json::from_str::<ReportHead>(&text) else { continue };
            let Some(flows) = head.flows else { continue };
            let at = fs::metadata(dir.join("report.json")).and_then(|m| m.modified()).ok().map(DateTime::<Utc>::from);
            out.push(SessionFlows { session: session.clone(), run: n, title: head.title, at, touched: flows.touched, proposal: flows.proposal });
            break;
        }
    }
    out.sort_by(|a, b| b.at.cmp(&a.at));
    out
}

/// The project a run looked at: `cwd` in meta.json, else the «Проект:» line of context.md.
fn run_project(dir: &Path) -> Option<PathBuf> {
    if let Ok(text) = fs::read_to_string(dir.join("meta.json")) {
        if let Some(cwd) = serde_json::from_str::<serde_json::Value>(&text).ok().and_then(|v| v.get("cwd")?.as_str().map(PathBuf::from)) {
            return Some(cwd);
        }
    }
    let text = fs::read_to_string(dir.join("context.md")).ok()?;
    text.lines().find_map(|l| l.strip_prefix("Проект: ")).map(|p| PathBuf::from(p.trim()))
}

/// Proposals you accepted or rejected, per project: `state/flows/<hash>/decisions.json`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Decisions {
    pub accepted: BTreeSet<String>,
    pub rejected: BTreeSet<String>,
}

impl Decisions {
    pub fn path(home: &Home, repo: &Path) -> PathBuf {
        home.state().join("flows").join(repo_key(repo)).join("decisions.json")
    }

    pub fn load(home: &Home, repo: &Path) -> Decisions {
        fs::read_to_string(Self::path(home, repo)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save(&self, home: &Home, repo: &Path) -> Result<()> {
        let p = Self::path(home, repo);
        fs::create_dir_all(p.parent().unwrap_or(&p))?;
        fs::write(&p, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn decided(&self, id: &str) -> bool {
        self.accepted.contains(id) || self.rejected.contains(id)
    }
}

/// Proposals still waiting for you.
pub fn pending<'a>(sessions: &'a [SessionFlows], decisions: &Decisions) -> Vec<&'a SessionFlows> {
    sessions.iter().filter(|s| !s.proposal.is_empty() && !decisions.decided(&s.id())).collect()
}

/// Short stable folder name for a project path.
pub fn repo_key(repo: &Path) -> String {
    let repo = repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf());
    let name = repo.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut h: u64 = 0xcbf29ce484222325;
    for b in repo.to_string_lossy().bytes() {
        h = (h ^ b as u64).wrapping_mul(0x100000001b3);
    }
    format!("{name}-{:08x}", h as u32)
}

fn modified(p: &Path) -> Option<SystemTime> {
    fs::metadata(p).and_then(|m| m.modified()).ok()
}

fn same_file(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.trim_start_matches("./").to_owned();
    let (a, b) = (norm(a), norm(b));
    a == b || a.ends_with(&format!("/{b}")) || b.ends_with(&format!("/{a}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> FlowMap {
        serde_yaml::from_str(
            r#"
version: 1
project: outpoll-web
areas: [{id: rewards, title: Награды}]
features:
  - id: rewards.joker
    title: Выбор карты Джокера
    area: rewards
    paths:
      - id: daily
        title: Из ежедневной карты
        steps:
          - {id: open, screen: Rewards Hub, action: Нажать «Выбрать», result: Диалог открыт, entities: [GlobalDailyCard], files: [apps/web/GlobalDailyCard.tsx]}
          - {id: pick, screen: Диалог, action: Выбрать карту, result: Карта летит в хаб, entities: [JokerPickDialog, takeFlight], files: [apps/web/JokerPickDialog.tsx]}
    proofs:
      - {kind: video, title: Полёт карты, reference: media/1/flight.mp4, covers: [pick]}
"#,
        )
        .unwrap()
    }

    #[test]
    fn parses_and_finds_touched_steps() {
        let map = sample();
        let t = map.touched(&["./apps/web/JokerPickDialog.tsx".into()], &[]);
        assert_eq!(t, vec![Touched { feature: "rewards.joker".into(), path: "daily".into(), steps: vec!["pick".into()] }]);
        let t = map.touched(&[], &["GlobalDailyCard".into()]);
        assert_eq!(t[0].steps, vec!["open".to_owned()]);
        assert!(map.touched(&["other.ts".into()], &[]).is_empty());
    }

    #[test]
    fn batches_split_by_area_and_size() {
        let mut map = sample();
        map.areas.push(Area { id: "auth".into(), title: "Вход".into() });
        for i in 0..7 {
            map.features.push(Feature { id: format!("auth.{i}"), area: "auth".into(), status: "draft".into(), ..Default::default() });
        }
        let ids: Vec<String> = map.drafts().map(|f| f.id.clone()).collect();
        assert_eq!(ids.len(), 7);
        let b = map.batches(&ids, 3);
        assert_eq!(b.iter().map(|b| b.features.len()).collect::<Vec<_>>(), vec![3, 3, 1]);
        assert_eq!(b[0].title, "Вход · 1/3");
        let all: Vec<String> = map.features.iter().map(|f| f.id.clone()).collect();
        assert_eq!(map.batches(&all, 6).len(), 3, "rewards 1 + auth 6 + auth 1");
    }

    #[test]
    fn proofs_pass_and_ignored_merge() {
        let mut map = sample();
        let before = map.features[0].paths.clone();
        map.apply_proofs(&ProofsPart {
            features: vec![FeatureProofs { feature: "rewards.joker".into(), proofs: vec![Proof { kind: ProofKind::Test, reference: "a.test.ts".into(), ..Default::default() }] }],
        });
        assert_eq!(map.features[0].proofs.len(), 1);
        assert_eq!(map.features[0].paths, before, "proofs pass keeps paths");
        let ig = Ignored { entry: "/debug".into(), reason: "служебная".into() };
        map.apply(&FlowProposal { ignored: vec![ig.clone(), ig], ..Default::default() });
        assert_eq!(map.ignored.len(), 1);
    }

    #[test]
    fn proposal_upserts_and_removes() {
        let mut map = sample();
        let mut f = map.features[0].clone();
        f.title = "Джокер".into();
        let new = Feature { id: "auth.login".into(), title: "Вход".into(), ..Default::default() };
        map.apply(&FlowProposal { upsert: vec![f, new], ..Default::default() });
        assert_eq!(map.features.len(), 2);
        assert_eq!(map.feature("rewards.joker").unwrap().title, "Джокер");
        map.apply(&FlowProposal { remove: vec!["auth.login".into()], ..Default::default() });
        assert_eq!(map.features.len(), 1);
    }

    #[test]
    fn reads_session_proposals_and_decisions() {
        let dir = std::env::temp_dir().join(format!("sv-flows-s-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let home = Home { root: dir.join("home") };
        let repo = dir.join("repo");
        fs::create_dir_all(&repo).unwrap();
        let run = home.state().join("changes/sess-a/runs/2");
        fs::create_dir_all(&run).unwrap();
        fs::write(run.join("context.md"), format!("# Сессия: x\n\nПроект: {}\n", repo.display())).unwrap();
        fs::write(
            run.join("report.json"),
            r#"{"title":"Полёт карты","flows":{"touched":[{"feature":"rewards.joker","path":"daily","steps":["pick"]}],"proposal":{"upsert":[{"id":"rewards.fly","title":"Полёт в хаб"}],"notes":["+ Полёт в хаб"]}}}"#,
        )
        .unwrap();
        // A run of another project is ignored.
        let other = home.state().join("changes/sess-b/runs/1");
        fs::create_dir_all(&other).unwrap();
        fs::write(other.join("meta.json"), r#"{"cwd":"/elsewhere"}"#).unwrap();
        fs::write(other.join("report.json"), r#"{"flows":{"proposal":{"remove":["x"]}}}"#).unwrap();

        let found = session_flows(&home, &repo);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id(), "sess-a#2");
        assert_eq!(found[0].title, "Полёт карты");
        assert_eq!(found[0].touched[0].steps, vec!["pick".to_owned()]);
        let mut d = Decisions::load(&home, &repo);
        assert_eq!(pending(&found, &d).len(), 1);
        d.rejected.insert(found[0].id());
        d.save(&home, &repo).unwrap();
        assert!(pending(&found, &Decisions::load(&home, &repo)).is_empty());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn freshness_and_round_trip() {
        let dir = std::env::temp_dir().join(format!("sv-flows-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("apps/web")).unwrap();
        fs::write(dir.join("apps/web/JokerPickDialog.tsx"), "x").unwrap();
        let mut map = sample();
        // Proof without a timestamp is stale; one taken after the edit is fresh.
        assert_eq!(map.features[0].freshness(&dir), Freshness::Stale);
        map.features[0].proofs[0].at = Some(Utc::now() + chrono::Duration::seconds(5));
        assert_eq!(map.features[0].freshness(&dir), Freshness::Fresh);
        map.features[0].proofs.clear();
        assert_eq!(map.features[0].freshness(&dir), Freshness::Missing);
        map.save(&dir).unwrap();
        // The flow language keeps meaning; ids become positions / title slugs.
        let back = FlowMap::load(&dir).unwrap().unwrap();
        assert_eq!(back.features[0].title, map.features[0].title);
        assert_eq!(back.features[0].paths[0].title, "Из ежедневной карты");
        let steps: Vec<&str> = back.features[0].steps().map(|s| s.action.as_str()).collect();
        assert_eq!(steps, ["Нажать «Выбрать»", "Выбрать карту"]);
        assert_eq!(back.areas[0].title, "Награды");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn migrates_old_yaml_into_flow_files() {
        let dir = std::env::temp_dir().join(format!("sv-flows-mig-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".stepan_verify")).unwrap();
        fs::write(path(&dir), serde_yaml::to_string(&sample()).unwrap()).unwrap();
        let map = FlowMap::load(&dir).unwrap().unwrap();
        assert_eq!(map.features.len(), 1);
        assert!(!path(&dir).exists() && path(&dir).with_extension("yaml.bak").exists());
        assert!(dir.join(DIR).join("features/rewards.joker.yml").exists());
        assert!(stamp(&dir).is_some() && exists(&dir));
        // Second load reads the folder.
        assert_eq!(FlowMap::load(&dir).unwrap().unwrap().features[0].title, "Выбор карты Джокера");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn navigation_levels_from_start() {
        let nav = AppNav {
            app: "X".into(),
            start: "Запуск".into(),
            screens: vec![
                Screen { name: "Вход".into(), edges: vec![Edge { to: "Дашборд".into(), ..Default::default() }], ..Default::default() },
                Screen { name: "Запуск".into(), edges: vec![Edge { to: "Вход".into(), ..Default::default() }, Edge { to: "Дашборд".into(), ..Default::default() }], ..Default::default() },
                Screen { name: "Дашборд".into(), edges: vec![Edge { to: "Сессия".into(), ..Default::default() }], ..Default::default() },
                Screen { name: "Сессия".into(), ..Default::default() },
                Screen { name: "Отладка".into(), ..Default::default() },
            ],
        };
        // Start first; both «Вход» and «Дашборд» one step away; orphans last.
        assert_eq!(nav.levels(), vec![vec![1], vec![0, 2], vec![3], vec![4]]);
        assert_eq!(nav.incoming("Дашборд").count(), 2);
    }
}
