//! Report runs: every full rebuild and every batch of a long session is kept
//! in its own folder `<out>/runs/<n>/`, so the history of the map is never lost.
//! Also the git side of the input: agents often edit files with scripts, which
//! leaves no tool diffs, so the work tree itself is the source of truth
//! (captured through a shadow index against the session's stable base).

use std::{
    fs,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::Report;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunKind {
    #[default]
    Full,
    Batch,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Meta {
    pub at: Option<DateTime<Utc>>,
    pub kind: RunKind,
    /// How many session log entries this run covers (the next batch starts here).
    pub upto: usize,
    pub base_run: Option<usize>,
    /// Old runs: the work tree when the run started (`git stash create`, or
    /// HEAD when clean). New runs keep `tree` instead.
    pub git_snapshot: Option<String>,
    /// The session's diff base (a commit sha), resolved on its first run and
    /// carried over: `git.patch` of a full run is everything since it.
    pub base: Option<String>,
    /// The work tree when the run started, as a tree object (shadow index):
    /// the diff base of the next batch.
    pub tree: Option<String>,
    /// Edits in the part of the session this run looked at.
    pub edits: usize,
    pub reviewer: String,
    pub error: Option<String>,
    /// The project the run looked at (the flow map reads proposals per project).
    pub cwd: Option<PathBuf>,
}

pub struct Run {
    pub n: usize,
    pub dir: PathBuf,
    pub meta: Meta,
    /// None: no report.json yet (running, or it never got written).
    pub report: Option<Result<Report, String>>,
}

impl Run {
    pub fn ok(&self) -> Option<&Report> {
        self.report.as_ref().and_then(|r| r.as_ref().ok())
    }
}

pub fn runs_dir(out: &Path) -> PathBuf {
    out.join("runs")
}

pub fn load_run(n: usize, dir: PathBuf) -> Run {
    let meta: Meta = fs::read_to_string(dir.join("meta.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    let report = fs::read_to_string(dir.join("report.json"))
        .ok()
        .map(|text| serde_json::from_str::<Report>(&text).map_err(|e| format!("report.json не читается: {e}")));
    Run { n, dir, meta, report }
}

/// All runs, oldest first.
pub fn load_all(out: &Path) -> Vec<Run> {
    let Ok(entries) = fs::read_dir(runs_dir(out)) else { return Vec::new() };
    let mut runs: Vec<Run> = entries
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_str()?.parse::<usize>().ok()?;
            Some(load_run(n, e.path()))
        })
        .collect();
    runs.sort_by_key(|r| r.n);
    runs
}

// ------------------------------------------------------------------ follow

// ------------------------------------------------------------------ git

// ------------------------------------------------------------------ shadow capture
//
// The map's git input, after HumanLayer's daemon: a stable base per session,
// and the work tree captured through a throwaway index, so the agent's own
// commits stay in the diff and the user's staging area is never touched.
