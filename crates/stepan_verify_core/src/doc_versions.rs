//! Versions of shelf documents. Agents overwrite documents in place and
//! Stepan does not stand between them and the file, so every docs scan
//! compares each readable document with the last version it stored and keeps
//! the new content when it differs:
//! `state/doc-versions/<rel>/<timestamp>-<hash8>.<ext>`. The first version
//! seen is stored too, so the state before any change always exists. The
//! last [`KEEP`] versions per document are kept.

use std::{
    fs,
    path::{Path, PathBuf},
};

use chrono::{DateTime, NaiveDateTime, Utc};
use similar::{ChangeTag, TextDiff};

use crate::Home;

/// Versions kept per document.
pub const KEEP: usize = 30;

const STAMP: &str = "%Y%m%dT%H%M%S%3fZ";

#[derive(Clone, Debug, PartialEq)]
pub struct Version {
    pub path: PathBuf,
    /// When the scan noticed this content.
    pub at: DateTime<Utc>,
    pub hash: String,
}

pub fn dir(home: &Home, rel: &str) -> PathBuf {
    home.state().join("doc-versions").join(rel)
}

/// First 8 hex digits of a stable hash of the content.
pub fn hash8(content: &[u8]) -> String {
    format!("{:016x}", crate::doc_comments::fnv(content))[..8].to_owned()
}

/// Stored versions, oldest first.
pub fn list(home: &Home, rel: &str) -> Vec<Version> {
    let mut out: Vec<Version> = fs::read_dir(dir(home, rel))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let stem = path.file_stem()?.to_str()?.to_owned();
            let (stamp, hash) = stem.split_once('-')?;
            let at = NaiveDateTime::parse_from_str(stamp, STAMP).ok()?.and_utc();
            Some(Version { at, hash: hash.to_owned(), path })
        })
        .collect();
    out.sort_by(|a, b| a.at.cmp(&b.at).then_with(|| a.path.cmp(&b.path)));
    out
}

/// Store `content` as the newest version of `rel` unless it already is.
/// Returns the new version.
pub fn capture(home: &Home, rel: &str, content: &[u8]) -> Option<Version> {
    capture_at(home, rel, content, Utc::now())
}

fn capture_at(home: &Home, rel: &str, content: &[u8], at: DateTime<Utc>) -> Option<Version> {
    let hash = hash8(content);
    let versions = list(home, rel);
    if versions.last().is_some_and(|v| v.hash == hash) {
        return None;
    }
    let ext = Path::new(rel).extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let dir = dir(home, rel);
    fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("{}-{hash}{ext}", at.format(STAMP)));
    fs::write(&path, content).ok()?;
    for old in versions.iter().take((versions.len() + 1).saturating_sub(KEEP)) {
        let _ = fs::remove_file(&old.path);
    }
    Some(Version { path, at, hash })
}

/// One line of a diff, or a run of unchanged lines left out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiffLine {
    Same(String),
    Added(String),
    Removed(String),
    Skipped(usize),
}

/// Line diff of `old` → `new` with `context` unchanged lines around each change.
pub fn line_diff(old: &str, new: &str, context: usize) -> Vec<DiffLine> {
    let diff = TextDiff::from_lines(old, new);
    let mut out = Vec::new();
    let mut shown_to = 0; // old-side line index the output has reached
    for group in diff.grouped_ops(context) {
        let start = group.first().map_or(0, |op| op.old_range().start);
        if start > shown_to {
            out.push(DiffLine::Skipped(start - shown_to));
        }
        for op in &group {
            for change in diff.iter_changes(op) {
                let line = change.value().trim_end_matches(['\n', '\r']).to_owned();
                out.push(match change.tag() {
                    ChangeTag::Equal => DiffLine::Same(line),
                    ChangeTag::Insert => DiffLine::Added(line),
                    ChangeTag::Delete => DiffLine::Removed(line),
                });
            }
        }
        shown_to = group.last().map_or(shown_to, |op| op.old_range().end);
    }
    let total = old.lines().count();
    if !out.is_empty() && total > shown_to {
        out.push(DiffLine::Skipped(total - shown_to));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn captures_changes_only() {
        let home = Home { root: std::env::temp_dir().join(format!("stepan_verify-versions-{}", std::process::id())) };
        let _ = fs::remove_dir_all(&home.root);
        let rel = "proj/research/p95.md";
        let t0 = Utc::now();
        let first = capture_at(&home, rel, b"v1", t0).expect("the first version is stored");
        assert!(first.path.to_string_lossy().ends_with(".md"));
        assert!(capture_at(&home, rel, b"v1", t0 + Duration::seconds(1)).is_none(), "same content");
        capture_at(&home, rel, b"v2", t0 + Duration::seconds(2)).unwrap();
        let v = list(&home, rel);
        assert_eq!(v.len(), 2);
        assert_eq!(fs::read(&v[1].path).unwrap(), b"v2");
        assert_eq!(v[0].hash, hash8(b"v1"));
        // Back to old content is a change too.
        assert!(capture_at(&home, rel, b"v1", t0 + Duration::seconds(3)).is_some());
        // Capped.
        for i in 0..KEEP + 5 {
            capture_at(&home, rel, format!("n{i}").as_bytes(), t0 + Duration::seconds(10 + i as i64));
        }
        let v = list(&home, rel);
        assert_eq!(v.len(), KEEP);
        assert_eq!(fs::read(&v.last().unwrap().path).unwrap(), format!("n{}", KEEP + 4).as_bytes());
        let _ = fs::remove_dir_all(&home.root);
    }

    #[test]
    fn diff_lines() {
        let old = "a\nb\nc\nd\ne\nf\ng\nh\n";
        let new = "a\nb\nc\nd\nE\nf\ng\nh\n";
        let d = line_diff(old, new, 1);
        assert_eq!(
            d,
            [
                DiffLine::Skipped(3),
                DiffLine::Same("d".into()),
                DiffLine::Removed("e".into()),
                DiffLine::Added("E".into()),
                DiffLine::Same("f".into()),
                DiffLine::Skipped(2),
            ]
        );
        assert!(line_diff("same\n", "same\n", 3).is_empty());
    }
}
