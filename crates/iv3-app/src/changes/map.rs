//! «Карта изменений»: the changed code as a native graph. Nodes are the
//! report's code entries, edges their `uses` links (plus links the session
//! removed), laid out in dependency levels from entry points to leaves.

use std::collections::{HashMap, HashSet};

use super::{CodeChanges, EdgeRef, Entry};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NodeStatus {
    Added,
    Changed,
    Removed,
    Context,
}

#[derive(Clone, Debug)]
pub struct MapNode {
    /// Stable key across iterations: normalized id, else name.
    pub key: String,
    pub name: String,
    pub kind: String,
    pub file: String,
    pub note: String,
    pub status: NodeStatus,
    /// Made up from a `uses` target that matched no entry.
    pub implicit: bool,
    pub hunks: Vec<String>,
    pub risk_ids: Vec<String>,
    pub check_ids: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdgeKind {
    /// There before and after.
    Kept,
    /// Only after: from or to something added.
    New,
    /// Only before: from something removed, or listed in `removed_edges`.
    Removed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MapEdge {
    pub from: usize,
    pub to: usize,
    pub kind: EdgeKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Before,
    After,
    Diff,
}

#[derive(Clone, Debug, Default)]
pub struct Graph {
    pub nodes: Vec<MapNode>,
    pub edges: Vec<MapEdge>,
}

pub fn normalize(s: &str) -> String {
    s.trim().trim_matches('`').trim().to_lowercase()
}

/// Every way `uses` may refer to an entry: its id, its name, and each part
/// of a compound name like "NewSessionForm / StartSession" or "SessionView (start, …)".
pub fn aliases(e: &Entry) -> Vec<String> {
    let mut out = vec![normalize(&e.id), normalize(&e.name)];
    let head = e.name.split('(').next().unwrap_or_default();
    out.extend(head.split(['/', ',']).map(normalize));
    out.retain(|k| !k.is_empty());
    out.dedup();
    out
}

pub fn build(code: &CodeChanges, removed_edges: &[EdgeRef]) -> Graph {
    let mut nodes: Vec<MapNode> = Vec::new();
    let mut keys: HashMap<String, usize> = HashMap::new();
    let mut uses: Vec<Vec<String>> = Vec::new();
    let groups = [
        (NodeStatus::Added, &code.added),
        (NodeStatus::Changed, &code.changed),
        (NodeStatus::Removed, &code.removed),
        (NodeStatus::Context, &code.context),
    ];
    for (status, entries) in groups {
        for e in entries.iter().filter(|e| !e.name.trim().is_empty() || !e.id.trim().is_empty()) {
            let all = aliases(e);
            if status == NodeStatus::Context && all.iter().any(|k| keys.contains_key(k)) {
                continue; // context duplicate of a real entry
            }
            let ix = nodes.len();
            let name = if e.name.trim().is_empty() { e.id.trim() } else { e.name.trim() };
            let key = if e.id.trim().is_empty() { normalize(name) } else { normalize(&e.id) };
            nodes.push(MapNode {
                key,
                name: name.to_owned(),
                kind: e.kind.clone(),
                file: e.file.trim().to_owned(),
                note: e.note.clone(),
                status,
                implicit: false,
                hunks: e.hunks.clone(),
                risk_ids: e.risk_ids.clone(),
                check_ids: e.check_ids.clone(),
            });
            uses.push(e.uses.clone());
            for k in all {
                keys.entry(k).or_insert(ix);
            }
        }
    }
    let mut lookup = |target: &str, nodes: &mut Vec<MapNode>, uses: &mut Vec<Vec<String>>| -> Option<usize> {
        let key = normalize(target);
        if key.is_empty() {
            return None;
        }
        Some(*keys.entry(key.clone()).or_insert_with(|| {
            nodes.push(MapNode {
                key,
                name: target.trim().to_owned(),
                kind: String::new(),
                file: String::new(),
                note: String::new(),
                status: NodeStatus::Context,
                implicit: true,
                hunks: Vec::new(),
                risk_ids: Vec::new(),
                check_ids: Vec::new(),
            });
            uses.push(Vec::new());
            nodes.len() - 1
        }))
    };
    let mut edges: Vec<MapEdge> = Vec::new();
    let push = |edges: &mut Vec<MapEdge>, e: MapEdge| {
        if e.from != e.to && !edges.iter().any(|x| x.from == e.from && x.to == e.to) {
            edges.push(e);
        }
    };
    for from in 0..uses.len() {
        for target in uses[from].clone() {
            let Some(to) = lookup(&target, &mut nodes, &mut uses) else { continue };
            let kind = match (nodes[from].status, nodes[to].status) {
                (NodeStatus::Removed, _) | (_, NodeStatus::Removed) => EdgeKind::Removed,
                (NodeStatus::Added, _) | (_, NodeStatus::Added) => EdgeKind::New,
                _ => EdgeKind::Kept,
            };
            push(&mut edges, MapEdge { from, to, kind });
        }
    }
    for r in removed_edges {
        let (Some(from), Some(to)) = (lookup(&r.from, &mut nodes, &mut uses), lookup(&r.to, &mut nodes, &mut uses)) else { continue };
        if let Some(e) = edges.iter_mut().find(|e| e.from == from && e.to == to) {
            e.kind = EdgeKind::Removed;
        } else {
            push(&mut edges, MapEdge { from, to, kind: EdgeKind::Removed });
        }
    }
    Graph { nodes, edges }
}

impl Graph {
    pub fn node_visible(&self, i: usize, mode: Mode) -> bool {
        let n = &self.nodes[i];
        let real = match mode {
            Mode::Before => n.status != NodeStatus::Added,
            Mode::After => n.status != NodeStatus::Removed,
            Mode::Diff => true,
        };
        // A made-up node shows only where something links to it.
        real && (!n.implicit || self.edges.iter().any(|e| (e.from == i || e.to == i) && self.edge_visible(e, mode)))
    }

    pub fn edge_visible(&self, e: &MapEdge, mode: Mode) -> bool {
        match mode {
            Mode::Before => e.kind != EdgeKind::New,
            Mode::After => e.kind != EdgeKind::Removed,
            Mode::Diff => true,
        }
    }

    pub fn index(&self, key: &str) -> Option<usize> {
        self.nodes.iter().position(|n| n.key == key)
    }

    /// Keys of nodes that are new or different compared with an earlier map.
    pub fn changed_since(&self, earlier: &Graph) -> HashSet<String> {
        let before: HashMap<&str, &MapNode> = earlier.nodes.iter().map(|n| (n.key.as_str(), n)).collect();
        self.nodes
            .iter()
            .filter(|n| !n.implicit)
            .filter(|n| before.get(n.key.as_str()).is_none_or(|b| b.status != n.status || b.note != n.note))
            .map(|n| n.key.clone())
            .collect()
    }
}

/// Grid position of every node: column = dependency level (callers left,
/// callees right), rows ordered to keep links short. Without any link the
/// columns are files.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layout {
    pub pos: Vec<(usize, usize)>,
    pub cols: usize,
    pub rows: usize,
    /// Column headers.
    pub headers: Vec<String>,
}

pub fn layout(g: &Graph) -> Layout {
    let n = g.nodes.len();
    if n == 0 {
        return Layout::default();
    }
    let linked: Vec<bool> = (0..n).map(|i| g.edges.iter().any(|e| e.from == i || e.to == i)).collect();
    let mut level = vec![0usize; n];
    let headers: Vec<String>;
    if g.edges.is_empty() {
        let mut files: Vec<&str> = Vec::new();
        for node in &g.nodes {
            if !files.contains(&node.file.as_str()) {
                files.push(&node.file);
            }
        }
        for (i, node) in g.nodes.iter().enumerate() {
            level[i] = files.iter().position(|f| *f == node.file).unwrap_or(0);
        }
        headers = files.iter().map(|f| if f.is_empty() { "ПРОЧЕЕ".to_owned() } else { f.rsplit('/').next().unwrap_or(f).to_owned() }).collect();
    } else {
        // Longest path from the callers; bounded so cycles cannot loop.
        for _ in 0..n {
            let mut moved = false;
            for e in &g.edges {
                if level[e.to] < level[e.from] + 1 && level[e.from] + 1 < n {
                    level[e.to] = level[e.from] + 1;
                    moved = true;
                }
            }
            if !moved {
                break;
            }
        }
        // Compact empty levels away.
        let mut used: Vec<usize> = (0..n).filter(|&i| linked[i]).map(|i| level[i]).collect();
        used.sort_unstable();
        used.dedup();
        for i in 0..n {
            level[i] = if linked[i] { used.iter().position(|&l| l == level[i]).unwrap_or(0) } else { 0 };
        }
        let cols = used.len().max(1);
        headers = (0..cols)
            .map(|c| match (c, cols) {
                (_, 1) => "УЗЛЫ".to_owned(),
                (0, _) => "ВХОД".to_owned(),
                (c, cols) if c + 1 == cols => "ЛИСТЬЯ".to_owned(),
                (1, _) => "ВЫЗЫВАЕТ".to_owned(),
                _ => "ДАЛЬШЕ".to_owned(),
            })
            .collect();
    }
    let cols = headers.len().max(1);
    let status_rank = |s: NodeStatus| match s {
        NodeStatus::Changed => 0,
        NodeStatus::Added => 1,
        NodeStatus::Removed => 2,
        NodeStatus::Context => 3,
    };
    let mut columns: Vec<Vec<usize>> = vec![Vec::new(); cols];
    for i in 0..n {
        columns[level[i].min(cols - 1)].push(i);
    }
    for col in &mut columns {
        col.sort_by(|&a, &b| {
            let (x, y) = (&g.nodes[a], &g.nodes[b]);
            // Unlinked nodes at the bottom of their column.
            (!linked[a], &x.file, status_rank(x.status), &x.name).cmp(&(!linked[b], &y.file, status_rank(y.status), &y.name))
        });
    }
    // Two barycenter sweeps: order each column by the mean row of its callers.
    for _ in 0..2 {
        for c in 1..cols {
            let row_of: HashMap<usize, usize> = columns[c - 1].iter().enumerate().map(|(r, &i)| (i, r)).collect();
            let center = |i: usize| -> Option<f32> {
                let rows: Vec<f32> = g.edges.iter().filter(|e| e.to == i).filter_map(|e| row_of.get(&e.from)).map(|&r| r as f32).collect();
                (!rows.is_empty()).then(|| rows.iter().sum::<f32>() / rows.len() as f32)
            };
            let mut keyed: Vec<(f32, usize, usize)> =
                columns[c].iter().enumerate().map(|(r, &i)| (center(i).unwrap_or(r as f32 + 0.5), r, i)).collect();
            keyed.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
            columns[c] = keyed.into_iter().map(|(_, _, i)| i).collect();
        }
    }
    let mut pos = vec![(0, 0); n];
    for (c, col) in columns.iter().enumerate() {
        for (r, &i) in col.iter().enumerate() {
            pos[i] = (c, r);
        }
    }
    let rows = columns.iter().map(Vec::len).max().unwrap_or(0);
    Layout { pos, cols, rows, headers }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::changes::Report;

    fn report(json: &str) -> Report {
        serde_json::from_str(json).unwrap()
    }

    #[core::prelude::v1::test]
    fn edges_and_sides() {
        let r = report(
            r#"{"code":{
                "added":[{"id":"card","name":"live_card::build","file":"src/live_card.rs","uses":["Item"]}],
                "changed":[{"id":"dash","name":"render_dashboard","file":"src/dashboard.rs","uses":["card","old"]}],
                "removed":[{"id":"old","name":"session_row","file":"src/dashboard.rs"}],
                "context":[{"name":"Item","kind":"type","file":"agents/model.rs"}]},
              "removed_edges":[["dash","Item"]]}"#,
        );
        let g = build(&r.code, &r.removed_edges);
        let ix = |k: &str| g.index(k).unwrap();
        let edge = |a: &str, b: &str| g.edges.iter().find(|e| e.from == ix(a) && e.to == ix(b)).map(|e| e.kind);
        assert_eq!(edge("dash", "card"), Some(EdgeKind::New));
        assert_eq!(edge("dash", "old"), Some(EdgeKind::Removed));
        assert_eq!(edge("card", "item"), Some(EdgeKind::New));
        assert_eq!(edge("dash", "item"), Some(EdgeKind::Removed));
        assert!(!g.node_visible(ix("card"), Mode::Before) && g.node_visible(ix("card"), Mode::After));
        assert!(g.node_visible(ix("old"), Mode::Before) && !g.node_visible(ix("old"), Mode::After));
        assert!(g.nodes.iter().all(|n| !n.implicit));
    }

    #[core::prelude::v1::test]
    fn levels_go_from_callers_to_leaves() {
        let r = report(
            r#"{"code":{"changed":[{"id":"a","name":"A","uses":["b","c"]},{"id":"b","name":"B","uses":["d"]}],
                "added":[{"id":"c","name":"C","uses":["d"]},{"id":"d","name":"D"},{"id":"lone","name":"Lone"}]}}"#,
        );
        let g = build(&r.code, &[]);
        let l = layout(&g);
        let col = |k: &str| l.pos[g.index(k).unwrap()].0;
        assert_eq!((col("a"), col("b"), col("c"), col("d")), (0, 1, 1, 2));
        assert_eq!(l.cols, 3);
        assert_eq!(l.headers, vec!["ВХОД", "ВЫЗЫВАЕТ", "ЛИСТЬЯ"]);
        // The unlinked node sits under the linked ones of column 0.
        assert_eq!(l.pos[g.index("lone").unwrap()], (0, 1));
    }

    #[core::prelude::v1::test]
    fn cycles_terminate_and_unlinked_maps_group_by_file() {
        let r = report(r#"{"code":{"changed":[{"id":"a","name":"A","uses":["b"]},{"id":"b","name":"B","uses":["a"]}]}}"#);
        let l = layout(&build(&r.code, &[]));
        assert_eq!(l.pos.len(), 2);
        let r = report(r#"{"code":{"added":[{"name":"sub","file":"m.js"},{"name":"x","file":"n.js"}],"removed":[{"name":"mul","file":"m.js"}]}}"#);
        let g = build(&r.code, &[]);
        let l = layout(&g);
        assert_eq!(l.headers, vec!["m.js", "n.js"]);
        assert_eq!(l.pos[g.index("x").unwrap()].0, 1);
    }

    #[core::prelude::v1::test]
    fn unknown_targets_become_context_and_iterations_diff() {
        let r = report(r#"{"code":{"added":[{"name":"x","uses":["B"]}],"changed":[{"name":"NewSessionForm / StartSession","note":"v1"}]}}"#);
        let g1 = build(&r.code, &[]);
        assert!(g1.nodes.iter().any(|n| n.implicit && n.name == "B"));
        let r2 = report(r#"{"code":{"added":[{"name":"x","uses":["B"]},{"name":"y"}],"changed":[{"name":"NewSessionForm / StartSession","note":"v2"}]}}"#);
        let g2 = build(&r2.code, &[]);
        let changed = g2.changed_since(&g1);
        assert!(changed.contains("y") && changed.contains("newsessionform / startsession") && !changed.contains("x"));
        assert_eq!(aliases(&Entry { name: "NewSessionForm / StartSession".into(), ..Default::default() }), vec!["newsessionform / startsession", "newsessionform", "startsession"]);
    }
}
