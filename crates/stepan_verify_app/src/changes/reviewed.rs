//! «Просмотрено»: nodes of the map you have looked at, in
//! `~/.stepan_verify/state/changes/<session>/reviewed.json`. A mark keeps the
//! run it was made on and a fingerprint of the node then, so a later run tells
//! nodes that stayed the same (dimmed) from nodes that changed since you
//! looked (flagged).

use std::{collections::BTreeMap, fs, path::Path};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::map::{MapNode, NodeStatus};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Mark {
    /// The run the node was reviewed on.
    pub run: usize,
    pub fingerprint: String,
    #[serde(default)]
    pub at: Option<DateTime<Utc>>,
}

/// Node key → mark.
pub type Marks = BTreeMap<String, Mark>;

fn file(out: &Path) -> std::path::PathBuf {
    out.join("reviewed.json")
}

pub fn load(out: &Path) -> Marks {
    fs::read_to_string(file(out)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

pub fn save(out: &Path, marks: &Marks) -> std::io::Result<()> {
    fs::create_dir_all(out)?;
    fs::write(file(out), serde_json::to_string_pretty(marks).unwrap_or_default())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Review {
    New,
    Seen,
    /// Reviewed, then a later run changed it.
    ChangedSince,
}

pub fn review(marks: &Marks, key: &str, fingerprint: &str) -> Review {
    match marks.get(key) {
        None => Review::New,
        Some(m) if m.fingerprint == fingerprint => Review::Seen,
        Some(_) => Review::ChangedSince,
    }
}

/// Cards worth a review: what the session changed and its tests, not the
/// context around it or links made up from `uses`.
pub fn reviewable(n: &MapNode) -> bool {
    !n.implicit && (n.status != NodeStatus::Context || n.test.is_some())
}

/// What identifies "this node changed": its status, its interface and the
/// added / removed lines of its hunks (line numbers left out, so edits
/// elsewhere in the file don't count). The reviewer rewords descriptions
/// between full rebuilds, so the description counts only when there is no diff.
pub fn fingerprint(n: &MapNode, hunks: &[String]) -> String {
    let status = match n.status {
        NodeStatus::Added => "added",
        NodeStatus::Changed => "changed",
        NodeStatus::Removed => "removed",
        NodeStatus::Context => "context",
    };
    let lines: Vec<&str> = hunks
        .iter()
        .flat_map(|h| h.lines())
        .filter(|l| (l.starts_with('+') && !l.starts_with("+++")) || (l.starts_with('-') && !l.starts_with("---")))
        .map(str::trim_end)
        .collect();
    let diff = lines.join("\n");
    let note = if lines.is_empty() { n.note.trim() } else { "" };
    fnv(&[status, n.signature.trim(), n.signature_before.trim(), &diff, note])
}

/// FNV-1a: stable across builds (std's hasher is not), enough to tell versions apart.
fn fnv(parts: &[&str]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for part in parts {
        for b in part.bytes().chain([0xff]) {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    format!("{h:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::changes::{CodeChanges, Entry, map};

    fn node(note: &str, signature: &str) -> MapNode {
        let code = CodeChanges { changed: vec![Entry { id: "sum".into(), name: "sum".into(), note: note.into(), signature: signature.into(), ..Entry::default() }], ..CodeChanges::default() };
        map::build(&code, &[], &[], &[], &[]).nodes.remove(0)
    }

    #[core::prelude::v1::test]
    fn fingerprint_follows_the_code_not_the_wording() {
        let hunk = |at: &str, body: &str| vec![format!("@@ {at} @@ fn sum\n a\n{body}\n")];
        let n = node("складывает", "sum(a, b) → n");
        let fp = fingerprint(&n, &hunk("-1,2 +1,3", "+b"));
        // Same change, moved down the file and described in other words: still reviewed.
        assert_eq!(fingerprint(&node("суммирует", "sum(a, b) → n"), &hunk("-10,2 +12,3", "+b")), fp);
        // Another body, interface or status: changed since.
        assert_ne!(fingerprint(&n, &hunk("-1,2 +1,3", "+c")), fp);
        assert_ne!(fingerprint(&node("складывает", "sum(a, b, c) → n"), &hunk("-1,2 +1,3", "+b")), fp);
        let mut removed = n.clone();
        removed.status = NodeStatus::Removed;
        assert_ne!(fingerprint(&removed, &hunk("-1,2 +1,3", "+b")), fp);
        // Without a diff the description is all there is.
        assert_ne!(fingerprint(&node("складывает", "sum"), &[]), fingerprint(&node("вычитает", "sum"), &[]));

        let marks: Marks = [("sum".to_owned(), Mark { run: 2, fingerprint: fp.clone(), at: None })].into();
        assert_eq!(review(&marks, "sum", &fp), Review::Seen);
        assert_eq!(review(&marks, "sum", "other"), Review::ChangedSince);
        assert_eq!(review(&marks, "sub", &fp), Review::New);
    }

    #[core::prelude::v1::test]
    fn marks_round_trip() {
        let out = std::env::temp_dir().join(format!("stepan_verify-reviewed-{}", std::process::id()));
        let _ = fs::remove_dir_all(&out);
        assert!(load(&out).is_empty());
        let mut marks = Marks::new();
        marks.insert("sum".into(), Mark { run: 3, fingerprint: "00ff".into(), at: Some(Utc::now()) });
        marks.insert("ui::card".into(), Mark { run: 4, fingerprint: "abcd".into(), at: None });
        save(&out, &marks).unwrap();
        assert_eq!(load(&out), marks);
        // A file from an older build without `at` still reads.
        fs::write(out.join("reviewed.json"), r#"{"sum":{"run":1,"fingerprint":"x"}}"#).unwrap();
        assert_eq!(load(&out)["sum"].run, 1);
        let _ = fs::remove_dir_all(out);
    }
}
