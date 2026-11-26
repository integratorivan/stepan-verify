//! What the window had open, kept in the store (`state/stepan.db`) so the next
//! start shows the same tabs and page at once, and each open session's last
//! transcript so a restored tab shows its conversation before the agent has
//! re-read it.

use std::path::PathBuf;

use gpui_kit::*;
use serde::{Deserialize, Serialize};
use stepan_verify_agents::Item;
use stepan_verify_core::{Home, Provider, Store};

const KEY: &str = "workspace";
/// A restored tab only needs the recent part of a long conversation to look right.
const SNAPSHOT_ITEMS: usize = 400;

pub struct Db(pub Option<Store>);

impl Global for Db {}

pub fn init(home: &Home, cx: &mut App) {
    let store = Store::open(home)
        .inspect_err(|err| eprintln!("stepan_verify: state db: {err:#}"))
        .ok();
    if let Some(store) = &store {
        stepan_verify_core::usage::init(store.clone());
        let store = store.clone();
        // Snapshots of sessions closed a month ago are dead weight.
        cx.background_spawn(async move { store.prune_snapshots(30) }).detach();
    }
    cx.set_global(Db(store));
}

pub fn store(cx: &App) -> Option<Store> {
    cx.try_global::<Db>().and_then(|db| db.0.clone())
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Workspace {
    /// dashboard | tasks | flows | docs | memory | skills | settings | session
    pub page: String,
    /// With `page = "session"`: which of `open`.
    pub session: Option<usize>,
    pub open: Vec<OpenTab>,
    /// Document open on «Документы» (path inside docs/).
    pub doc: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OpenTab {
    pub provider: Provider,
    pub id: String,
    pub title: String,
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub task_id: Option<String>,
    /// The «Изменения» tab was in front.
    #[serde(default)]
    pub changes: bool,
}

pub fn load(cx: &App) -> Workspace {
    store(cx).and_then(|s| s.get(KEY)).unwrap_or_default()
}

pub fn save(workspace: &Workspace, cx: &App) {
    if let Some(store) = store(cx) {
        if let Err(err) = store.put(KEY, workspace) {
            eprintln!("stepan_verify: save workspace: {err:#}");
        }
    }
}

fn snapshot_key(provider: Provider, id: &str) -> String {
    format!("{}:{id}", provider.label().to_lowercase())
}

/// Keep the tail of a transcript for the next start (written off the UI thread).
pub fn save_snapshot(provider: Provider, id: &str, items: &[Item], cx: &App) {
    let Some(store) = store(cx) else { return };
    let items = items[items.len().saturating_sub(SNAPSHOT_ITEMS)..].to_vec();
    let key = snapshot_key(provider, id);
    cx.background_spawn(async move {
        match serde_json::to_string(&items) {
            Ok(json) => {
                if let Err(err) = store.put_snapshot(&key, &json) {
                    eprintln!("stepan_verify: snapshot {key}: {err:#}");
                }
            }
            Err(err) => eprintln!("stepan_verify: snapshot {key}: {err}"),
        }
    })
    .detach();
}

/// The transcript saved last time, read and parsed off the UI thread.
pub fn load_snapshot(provider: Provider, id: &str, cx: &App) -> Task<Option<Vec<Item>>> {
    let Some(store) = store(cx) else { return Task::ready(None) };
    let key = snapshot_key(provider, id);
    cx.background_spawn(async move { serde_json::from_str(&store.snapshot(&key)?).ok() })
}
