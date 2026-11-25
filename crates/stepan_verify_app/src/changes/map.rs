//! «Карта изменений»: the changed code as a canvas of meaning. Nodes are the
//! report's code entries (entity, role, signature), grouped into frames by
//! what they do for the user (features, helpers, infra) plus a tests frame
//! whose cards link to what they verify. Edges are `uses` links (and links the
//! session removed) labelled with the DTO they pass.

use std::collections::{HashMap, HashSet};

use super::{CodeChanges, Dto, EdgeRef, Entry, Feature, Report, TestRef, UseRef};

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
    /// Interface pseudocode now, and before the session when it changed.
    pub signature: String,
    pub signature_before: String,
    /// DTO indices it takes and returns.
    pub inputs: Vec<usize>,
    pub outputs: Vec<usize>,
    /// component | function | hook | type | … (from the report or guessed from `kind`).
    pub role: String,
    /// Human name: «Диалог выбора Джокера».
    pub entity: String,
    /// Index into `Graph::groups`.
    pub group: usize,
    /// Set for test cards.
    pub test: Option<TestInfo>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TestInfo {
    /// unit | e2e | snapshot | manual
    pub kind: String,
    /// pass | fail | not_run | ""
    pub result: String,
}

impl MapNode {
    fn plain(key: String, name: String, status: NodeStatus, implicit: bool) -> Self {
        MapNode {
            role: String::new(),
            entity: String::new(),
            group: 0,
            test: None,
            key,
            name,
            kind: String::new(),
            file: String::new(),
            note: String::new(),
            status,
            implicit,
            hunks: Vec::new(),
            risk_ids: Vec::new(),
            check_ids: Vec::new(),
            signature: String::new(),
            signature_before: String::new(),
            inputs: Vec::new(),
            outputs: Vec::new(),
        }
    }

    /// The signature the given mode shows.
    pub fn signature_in(&self, mode: Mode) -> &str {
        match mode {
            Mode::Before if !self.signature_before.is_empty() => &self.signature_before,
            _ => &self.signature,
        }
    }

    /// The interface itself changed (not only the body).
    pub fn signature_changed(&self) -> bool {
        !self.signature_before.is_empty() && self.signature_before.trim() != self.signature.trim()
    }

    /// The card title: the human name, else the code name.
    pub fn title(&self) -> &str {
        if self.entity.trim().is_empty() { &self.name } else { self.entity.trim() }
    }

    /// Normalized role: the report's, else guessed from `kind`.
    pub fn role(&self) -> Role {
        if self.test.is_some() {
            return Role::Test;
        }
        Role::parse(if self.role.trim().is_empty() { &self.kind } else { &self.role })
    }
}

/// What a card is, for its icon and for "без тестов".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Role {
    Component,
    Function,
    Hook,
    Type,
    Dto,
    Config,
    Test,
    Api,
    Store,
    Style,
    Script,
    Other,
}

impl Role {
    pub fn parse(s: &str) -> Role {
        let s = s.trim().to_lowercase();
        match s.as_str() {
            "component" | "компонент" | "page" | "screen" | "view" | "widget" => Role::Component,
            "function" | "функция" | "method" | "fn" | "handler" | "util" | "utility" => Role::Function,
            "hook" | "хук" => Role::Hook,
            "type" | "тип" | "interface" | "enum" | "class" | "struct" => Role::Type,
            "dto" | "props" | "schema" | "model" => Role::Dto,
            "config" | "конфиг" | "constant" | "const" | "env" | "flag" | "file" => Role::Config,
            "test" | "тест" | "spec" | "story" => Role::Test,
            "api" | "endpoint" | "route" | "эндпоинт" | "роут" | "query" | "mutation" => Role::Api,
            "store" | "state" | "atom" | "table" | "db" | "migration" | "таблица" => Role::Store,
            "style" | "css" | "стиль" | "theme" => Role::Style,
            "script" | "скрипт" | "cli" | "command" | "job" => Role::Script,
            _ => Role::Other,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Role::Component => "компонент",
            Role::Function => "функция",
            Role::Hook => "хук",
            Role::Type => "тип",
            Role::Dto => "данные",
            Role::Config => "конфиг",
            Role::Test => "тест",
            Role::Api => "API",
            Role::Store => "хранилище",
            Role::Style => "стиль",
            Role::Script => "скрипт",
            Role::Other => "код",
        }
    }

    /// Lucide icon path (see icons.rs for the bundled ones).
    pub fn icon(self) -> &'static str {
        match self {
            Role::Component => "icons/component.svg",
            Role::Function => "icons/square-function.svg",
            Role::Hook => "icons/anchor.svg",
            Role::Type => "icons/braces.svg",
            Role::Dto => "icons/braces.svg",
            Role::Config => "icons/settings-2.svg",
            Role::Test => "icons/flask-conical.svg",
            Role::Api => "icons/globe.svg",
            Role::Store => "icons/database.svg",
            Role::Style => "icons/palette.svg",
            Role::Script => "icons/terminal.svg",
            Role::Other => "icons/shapes.svg",
        }
    }

    /// Behaviour a test could check (types, configs and styles do not count for "без тестов").
    pub fn testable(self) -> bool {
        matches!(self, Role::Component | Role::Function | Role::Hook | Role::Api | Role::Store | Role::Script | Role::Other)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum GroupKind {
    /// «Эти штуки делают функционал X».
    Feature,
    /// «Эти помогают»: utilities, constants, styles, wrappers.
    Helper,
    Infra,
    /// One frame per file: old reports without `features`.
    File,
    /// Untouched code around the changes.
    Context,
    Tests,
}

impl GroupKind {
    pub fn parse(s: &str) -> GroupKind {
        match s.trim().to_lowercase().as_str() {
            "helper" | "helpers" | "support" | "помогает" => GroupKind::Helper,
            "infra" | "infrastructure" | "config" | "инфраструктура" => GroupKind::Infra,
            "tests" | "test" | "тесты" => GroupKind::Tests,
            "context" => GroupKind::Context,
            _ => GroupKind::Feature,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            GroupKind::Feature => "функционал",
            GroupKind::Helper => "помогает",
            GroupKind::Infra => "инфраструктура",
            GroupKind::File => "файл",
            GroupKind::Context => "окружение",
            GroupKind::Tests => "тесты",
        }
    }
}

/// A frame on the canvas.
#[derive(Clone, Debug)]
pub struct MapGroup {
    pub key: String,
    pub title: String,
    pub summary: String,
    pub kind: GroupKind,
    pub nodes: Vec<usize>,
}

/// A field of a DTO.
#[derive(Clone, Debug, PartialEq)]
pub struct MapField {
    pub name: String,
    pub ty: String,
    pub status: NodeStatus,
    pub note: String,
}

/// Data that travels between nodes.
#[derive(Clone, Debug)]
pub struct MapDto {
    pub key: String,
    pub name: String,
    pub kind: String,
    pub file: String,
    pub note: String,
    pub status: NodeStatus,
    pub fields: Vec<MapField>,
    /// Nodes that take, return or pass it.
    pub users: Vec<usize>,
}

impl MapDto {
    pub fn visible(&self, mode: Mode) -> bool {
        status_visible(self.status, mode)
    }

    pub fn fields_in(&self, mode: Mode) -> impl Iterator<Item = &MapField> {
        self.fields.iter().filter(move |f| status_visible(f.status, mode))
    }

    /// Fields added, removed or changed: the DTO's own diff.
    pub fn touched(&self) -> usize {
        self.fields.iter().filter(|f| f.status != NodeStatus::Context).count()
    }
}

fn status_visible(s: NodeStatus, mode: Mode) -> bool {
    match mode {
        Mode::Before => s != NodeStatus::Added,
        Mode::After => s != NodeStatus::Removed,
        Mode::Diff => true,
    }
}

pub fn parse_status(s: &str) -> NodeStatus {
    match s.trim().to_lowercase().as_str() {
        "added" | "new" | "add" => NodeStatus::Added,
        "changed" | "modified" | "mod" | "change" => NodeStatus::Changed,
        "removed" | "deleted" | "del" | "remove" => NodeStatus::Removed,
        _ => NodeStatus::Context,
    }
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
    /// The DTO passed along this link, index into `Graph::dtos`.
    pub via: Option<usize>,
    /// A test → what it verifies (dashed «проверяет»).
    pub verify: bool,
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
    pub dtos: Vec<MapDto>,
    /// Free-text label of a link: (from, to) → "передаёт выбранную карту".
    pub notes: HashMap<(usize, usize), String>,
    /// Frames: every node belongs to exactly one.
    pub groups: Vec<MapGroup>,
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

pub fn build_report(r: &Report) -> Graph {
    build(&r.code, &r.removed_edges, &r.dtos, &r.features, &r.tests)
}

pub fn build(code: &CodeChanges, removed_edges: &[EdgeRef], dtos: &[Dto], features: &[Feature], tests: &[TestRef]) -> Graph {
    // DTOs first: nodes and links refer to them.
    let mut dto_keys: HashMap<String, usize> = HashMap::new();
    let mut map_dtos: Vec<MapDto> = Vec::new();
    for d in dtos.iter().filter(|d| !d.name.trim().is_empty() || !d.id.trim().is_empty()) {
        let name = if d.name.trim().is_empty() { d.id.trim() } else { d.name.trim() };
        let key = if d.id.trim().is_empty() { normalize(name) } else { normalize(&d.id) };
        if dto_keys.contains_key(&key) {
            continue;
        }
        let ix = map_dtos.len();
        for k in [normalize(&d.id), normalize(name)] {
            if !k.is_empty() {
                dto_keys.entry(k).or_insert(ix);
            }
        }
        map_dtos.push(MapDto {
            key,
            name: name.to_owned(),
            kind: d.kind.clone(),
            file: d.file.trim().to_owned(),
            note: d.note.clone(),
            status: parse_status(&d.status),
            fields: d
                .fields
                .iter()
                .filter(|f| !f.name.trim().is_empty())
                .map(|f| MapField { name: f.name.trim().to_owned(), ty: f.ty.trim().to_owned(), status: parse_status(&f.status), note: f.note.clone() })
                .collect(),
            users: Vec::new(),
        });
    }
    let dto_of = |name: &str| dto_keys.get(&normalize(name)).copied();

    let mut nodes: Vec<MapNode> = Vec::new();
    let mut keys: HashMap<String, usize> = HashMap::new();
    let mut uses: Vec<Vec<UseRef>> = Vec::new();
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
            let mut node = MapNode::plain(key, name.to_owned(), status, false);
            node.kind = e.kind.clone();
            node.file = e.file.trim().to_owned();
            node.note = e.note.clone();
            node.hunks = e.hunks.clone();
            node.risk_ids = e.risk_ids.clone();
            node.check_ids = e.check_ids.clone();
            node.signature = e.signature.trim().to_owned();
            node.signature_before = e.signature_before.trim().to_owned();
            node.inputs = e.inputs.iter().filter_map(|d| dto_of(d)).collect();
            node.outputs = e.outputs.iter().filter_map(|d| dto_of(d)).collect();
            node.role = e.role.trim().to_owned();
            node.entity = e.entity.trim().to_owned();
            nodes.push(node);
            uses.push(e.uses.clone());
            for k in all {
                keys.entry(k).or_insert(ix);
            }
        }
    }
    let lookup = |keys: &mut HashMap<String, usize>, target: &str, nodes: &mut Vec<MapNode>, uses: &mut Vec<Vec<UseRef>>| -> Option<usize> {
        let key = normalize(target);
        if key.is_empty() {
            return None;
        }
        Some(*keys.entry(key.clone()).or_insert_with(|| {
            nodes.push(MapNode::plain(key, target.trim().to_owned(), NodeStatus::Context, true));
            uses.push(Vec::new());
            nodes.len() - 1
        }))
    };
    let mut edges: Vec<MapEdge> = Vec::new();
    let mut notes: HashMap<(usize, usize), String> = HashMap::new();
    let push = |edges: &mut Vec<MapEdge>, e: MapEdge| {
        if e.from == e.to {
            return;
        }
        match edges.iter_mut().find(|x| x.from == e.from && x.to == e.to) {
            Some(x) => x.via = x.via.or(e.via),
            None => edges.push(e),
        }
    };
    for from in 0..uses.len() {
        for u in uses[from].clone() {
            // A link "to" a DTO is the node passing that DTO, not a call.
            if keys.get(&normalize(&u.to)).is_none() {
                if let Some(d) = dto_of(&u.to) {
                    if !nodes[from].outputs.contains(&d) && !nodes[from].inputs.contains(&d) {
                        nodes[from].outputs.push(d);
                    }
                    continue;
                }
            }
            let Some(to) = lookup(&mut keys, &u.to, &mut nodes, &mut uses) else { continue };
            let kind = match (nodes[from].status, nodes[to].status) {
                (NodeStatus::Removed, _) | (_, NodeStatus::Removed) => EdgeKind::Removed,
                (NodeStatus::Added, _) | (_, NodeStatus::Added) => EdgeKind::New,
                _ => EdgeKind::Kept,
            };
            let via = (!u.via.trim().is_empty()).then(|| dto_of(&u.via)).flatten();
            push(&mut edges, MapEdge { from, to, kind, via, verify: false });
            if !u.note.trim().is_empty() {
                notes.insert((from, to), u.note.trim().to_owned());
            }
        }
    }
    for r in removed_edges {
        let (Some(from), Some(to)) = (lookup(&mut keys, &r.from, &mut nodes, &mut uses), lookup(&mut keys, &r.to, &mut nodes, &mut uses)) else { continue };
        if let Some(e) = edges.iter_mut().find(|e| e.from == from && e.to == to) {
            e.kind = EdgeKind::Removed;
        } else {
            push(&mut edges, MapEdge { from, to, kind: EdgeKind::Removed, via: None, verify: false });
        }
    }
    // Who uses each DTO: the report's list, inputs/outputs, and the links it rides on.
    for d in dtos {
        let Some(ix) = dto_of(&d.id).or_else(|| dto_of(&d.name)) else { continue };
        for u in &d.used_by {
            if let Some(&n) = keys.get(&normalize(u)) {
                if !map_dtos[ix].users.contains(&n) {
                    map_dtos[ix].users.push(n);
                }
            }
        }
    }
    for (n, node) in nodes.iter().enumerate() {
        for &d in node.inputs.iter().chain(&node.outputs) {
            if !map_dtos[d].users.contains(&n) {
                map_dtos[d].users.push(n);
            }
        }
    }
    // A link without an explicit DTO passes the one its target takes and its
    // source has at hand (the props a component renders its child with).
    for e in &mut edges {
        if e.via.is_none() {
            let (from, to) = (&nodes[e.from], &nodes[e.to]);
            e.via = to.inputs.iter().copied().find(|&d| from.outputs.contains(&d) || from.inputs.contains(&d) || map_dtos[d].users.contains(&e.from));
        }
    }
    for e in &edges {
        if let Some(d) = e.via {
            for n in [e.from, e.to] {
                if !map_dtos[d].users.contains(&n) {
                    map_dtos[d].users.push(n);
                }
            }
        }
    }
    // Tests: cards of their own, dashed links to what they verify.
    for t in tests.iter().filter(|t| !t.title.trim().is_empty() || !t.id.trim().is_empty() || !t.file.trim().is_empty()) {
        let title = [t.title.trim(), t.id.trim(), t.file.trim()].into_iter().find(|s| !s.is_empty()).unwrap_or_default();
        let key = normalize(if t.id.trim().is_empty() { title } else { &t.id });
        if keys.contains_key(&key) {
            continue;
        }
        let status = match parse_status(&t.status) {
            NodeStatus::Removed => NodeStatus::Removed,
            NodeStatus::Added => NodeStatus::Added,
            NodeStatus::Changed => NodeStatus::Changed,
            NodeStatus::Context => NodeStatus::Context,
        };
        let ix = nodes.len();
        let mut node = MapNode::plain(key.clone(), title.to_owned(), status, false);
        node.kind = "test".into();
        node.role = "test".into();
        node.file = t.file.trim().to_owned();
        node.test = Some(TestInfo { kind: t.kind.trim().to_lowercase(), result: t.result.trim().to_lowercase() });
        nodes.push(node);
        keys.insert(key, ix);
        for target in &t.verifies {
            let Some(&to) = keys.get(&normalize(target)) else { continue };
            if to == ix || nodes[to].test.is_some() {
                continue;
            }
            let kind = match status {
                NodeStatus::Added => EdgeKind::New,
                NodeStatus::Removed => EdgeKind::Removed,
                _ => EdgeKind::Kept,
            };
            edges.push(MapEdge { from: ix, to, kind, via: None, verify: true });
        }
    }
    let groups = group_nodes(&mut nodes, &keys, features);
    Graph { nodes, edges, dtos: map_dtos, notes, groups }
}

/// Put every node into one frame: the report's features first (in order,
/// first match wins), tests into «Тесты», the rest into «Прочее»/«Окружение»;
/// without features, one frame per file.
fn group_nodes(nodes: &mut [MapNode], keys: &HashMap<String, usize>, features: &[Feature]) -> Vec<MapGroup> {
    let mut groups: Vec<MapGroup> = Vec::new();
    let mut of: Vec<Option<usize>> = vec![None; nodes.len()];
    for (fi, f) in features.iter().enumerate() {
        let title = if f.title.trim().is_empty() { f.id.trim() } else { f.title.trim() };
        if title.is_empty() && f.nodes.is_empty() {
            continue;
        }
        let gi = groups.len();
        groups.push(MapGroup {
            key: if f.id.trim().is_empty() { format!("f{fi}") } else { normalize(&f.id) },
            title: title.to_owned(),
            summary: f.summary.trim().to_owned(),
            kind: GroupKind::parse(&f.kind),
            nodes: Vec::new(),
        });
        for k in &f.nodes {
            if let Some(&n) = keys.get(&normalize(k)) {
                // Test cards always live in the tests frame unless the feature is one.
                if of[n].is_none() && (nodes[n].test.is_none() || groups[gi].kind == GroupKind::Tests) {
                    of[n] = Some(gi);
                }
            }
        }
    }
    let named = |groups: &mut Vec<MapGroup>, key: &str, title: &str, kind: GroupKind| -> usize {
        if let Some(i) = groups.iter().position(|g| g.key == key) {
            return i;
        }
        groups.push(MapGroup { key: key.to_owned(), title: title.to_owned(), summary: String::new(), kind, nodes: Vec::new() });
        groups.len() - 1
    };
    let has_features = !groups.is_empty();
    for i in 0..nodes.len() {
        if of[i].is_some() {
            continue;
        }
        let n = &nodes[i];
        let gi = if n.test.is_some() {
            groups.iter().position(|g| g.kind == GroupKind::Tests).unwrap_or_else(|| named(&mut groups, "__tests", "Тесты", GroupKind::Tests))
        } else if n.status == NodeStatus::Context || n.implicit {
            named(&mut groups, "__context", "Окружение", GroupKind::Context)
        } else if has_features {
            named(&mut groups, "__rest", "Прочее", GroupKind::Helper)
        } else {
            let file = if n.file.is_empty() { "без файла".to_owned() } else { n.file.clone() };
            let title = file.rsplit('/').next().unwrap_or(&file).to_owned();
            named(&mut groups, &format!("__file:{file}"), &title, GroupKind::File)
        };
        of[i] = Some(gi);
    }
    for (i, g) in of.into_iter().enumerate() {
        let g = g.unwrap_or(0);
        nodes[i].group = g;
        groups[g].nodes.push(i);
    }
    // Drop empty frames, keep indices straight.
    let keep: Vec<usize> = (0..groups.len()).filter(|&g| !groups[g].nodes.is_empty()).collect();
    let remap: HashMap<usize, usize> = keep.iter().enumerate().map(|(new, &old)| (old, new)).collect();
    for n in nodes.iter_mut() {
        n.group = remap.get(&n.group).copied().unwrap_or(0);
    }
    keep.into_iter().map(|g| groups[g].clone()).collect()
}

impl Graph {
    pub fn node_visible(&self, i: usize, mode: Mode) -> bool {
        let n = &self.nodes[i];
        let real = status_visible(n.status, mode);
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

    pub fn dto_index(&self, key: &str) -> Option<usize> {
        self.dtos.iter().position(|d| d.key == key)
    }

    /// Keys of nodes and DTOs that are new or different compared with an
    /// earlier map (status, description, signature, fields).
    pub fn changed_since(&self, earlier: &Graph) -> HashSet<String> {
        let before: HashMap<&str, &MapNode> = earlier.nodes.iter().map(|n| (n.key.as_str(), n)).collect();
        let mut out: HashSet<String> = self
            .nodes
            .iter()
            .filter(|n| !n.implicit)
            .filter(|n| {
                before.get(n.key.as_str()).is_none_or(|b| b.status != n.status || b.note != n.note || b.signature != n.signature)
            })
            .map(|n| n.key.clone())
            .collect();
        let dtos_before: HashMap<&str, &MapDto> = earlier.dtos.iter().map(|d| (d.key.as_str(), d)).collect();
        out.extend(
            self.dtos
                .iter()
                .filter(|d| dtos_before.get(d.key.as_str()).is_none_or(|b| b.status != d.status || b.fields != d.fields))
                .map(|d| format!("dto:{}", d.key)),
        );
        out
    }

    /// DTOs the mode shows, in report order.
    pub fn visible_dtos(&self, mode: Mode) -> impl Iterator<Item = (usize, &MapDto)> {
        self.dtos.iter().enumerate().filter(move |(_, d)| d.visible(mode))
    }

    pub fn has_tests(&self) -> bool {
        self.nodes.iter().any(|n| n.test.is_some())
    }

    /// Test cards that verify node `i`.
    pub fn tests_of(&self, i: usize) -> Vec<usize> {
        self.edges.iter().filter(|e| e.verify && e.to == i).map(|e| e.from).collect()
    }

    /// Changed behaviour that no test checks (only meaningful when the map has tests).
    pub fn untested(&self, i: usize) -> bool {
        let n = &self.nodes[i];
        !n.implicit && matches!(n.status, NodeStatus::Added | NodeStatus::Changed) && n.role().testable() && !self.edges.iter().any(|e| e.verify && e.to == i)
    }

    pub fn group_index(&self, key: &str) -> Option<usize> {
        self.groups.iter().position(|g| g.key == key)
    }
}

// ------------------------------------------------------------------ canvas geometry

/// Card width; heights follow the content.
pub const NODE_W: f32 = 236.;
/// Between cards inside a frame.
pub const INNER_GAP_X: f32 = 46.;
pub const ROW_GAP: f32 = 12.;
/// Frame padding and gaps between frames.
pub const GROUP_PAD: f32 = 14.;
pub const GROUP_GAP_X: f32 = 96.;
pub const GROUP_GAP_Y: f32 = 40.;
/// Most cards stacked in one column of a frame before it wraps sideways.
const MAX_STACK: usize = 5;
pub const TITLE_LINE: f32 = 17.;
pub const NAME_LINE: f32 = 14.;
pub const SIG_LINE: f32 = 14.;
/// Characters per line (SF Rounded 13px title, Fira Code 11px name, 10.5px signature)
/// minus what word wrapping wastes.
const TITLE_CHARS: usize = 27;
const NAME_CHARS: usize = 30;
const SIG_CHARS: usize = 31;
pub const TITLE_MAX_LINES: usize = 2;
pub const SIG_MAX_LINES: usize = 2;
/// Summary characters per pixel of frame width (12px SF Rounded).
const SUMMARY_PX_PER_CHAR: f32 = 6.4;

fn lines_for(chars: usize, per_line: usize, max: usize) -> usize {
    chars.div_ceil(per_line.max(1)).clamp(1, max)
}

/// Text the card shows as the signature in a mode, as diff segments.
pub fn signature_parts(n: &MapNode, mode: Mode) -> Vec<(String, Part)> {
    if mode == Mode::Diff && n.signature_changed() {
        sig_diff(&n.signature_before, &n.signature)
    } else {
        let s = n.signature_in(mode);
        if s.is_empty() { Vec::new() } else { vec![(s.to_owned(), Part::Same)] }
    }
}

pub fn title_lines(n: &MapNode) -> usize {
    lines_for(n.title().chars().count(), TITLE_CHARS, TITLE_MAX_LINES)
}

/// The code name under a human title (none when the title is the code name).
pub fn shows_code_name(n: &MapNode) -> bool {
    !n.entity.trim().is_empty() && n.entity.trim() != n.name.trim()
}

pub fn sig_lines(n: &MapNode, mode: Mode) -> usize {
    let chars: usize = signature_parts(n, mode).iter().map(|(t, _)| t.chars().count()).sum();
    if chars == 0 { 0 } else { lines_for(chars, SIG_CHARS, SIG_MAX_LINES) }
}

/// Card on the canvas at rest: role row and the human title only. The code
/// name, signature and file show when you point at it (the card grows over
/// its neighbours) and in the side panel.
pub fn card_height(n: &MapNode) -> f32 {
    8. + 15. + 4. + title_lines(n) as f32 * TITLE_LINE + 9.
}

/// Card height with everything: role row, title, code name, signature, file row.
pub fn node_height(n: &MapNode, mode: Mode) -> f32 {
    let sig = sig_lines(n, mode);
    let name = if shows_code_name(n) { 2. + lines_for(n.name.chars().count(), NAME_CHARS, 1) as f32 * NAME_LINE } else { 0. };
    8. + 15. + 4. + title_lines(n) as f32 * TITLE_LINE + name + if sig > 0 { 5. + sig as f32 * SIG_LINE } else { 0. } + 5. + 15. + 8.
}

/// Frame header: title row, then the summary wrapped to the frame width.
pub fn group_head(g: &MapGroup, width: f32) -> f32 {
    let per_line = ((width - GROUP_PAD * 2.) / SUMMARY_PX_PER_CHAR) as usize;
    let summary = if g.summary.is_empty() { 0. } else { 3. + lines_for(g.summary.chars().count(), per_line, 2) as f32 * 16. };
    12. + 20. + summary + 12.
}

/// A frame on the canvas, in world coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GroupBox {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub head: f32,
}

/// Where everything sits on the canvas (world coordinates, before pan/zoom).
#[derive(Clone, Debug, Default)]
pub struct Canvas {
    /// (x, y, height) of each visible card.
    pub boxes: Vec<Option<(f32, f32, f32)>>,
    pub groups: Vec<Option<GroupBox>>,
    pub width: f32,
    pub height: f32,
}

/// Longest-path levels over `edges` (pairs of local indices), bounded so
/// cycles stop; nodes without links stay on level 0.
fn levels(n: usize, edges: &[(usize, usize)]) -> Vec<usize> {
    let mut level = vec![0usize; n];
    for _ in 0..n {
        let mut moved = false;
        for &(a, b) in edges {
            if a != b && level[b] < level[a] + 1 && level[a] + 1 < n {
                level[b] = level[a] + 1;
                moved = true;
            }
        }
        if !moved {
            break;
        }
    }
    level
}

fn status_rank(s: NodeStatus) -> u8 {
    match s {
        NodeStatus::Changed => 0,
        NodeStatus::Added => 1,
        NodeStatus::Removed => 2,
        NodeStatus::Context => 3,
    }
}

/// Cards of one frame: columns by dependency inside the frame (callers left),
/// long columns wrap sideways. Returns (local x, local y, h) per member and the content size.
fn layout_group(g: &Graph, members: &[usize], mode: Mode) -> (Vec<(usize, f32, f32, f32)>, f32, f32) {
    if members.is_empty() {
        return (Vec::new(), 0., 0.);
    }
    let local: HashMap<usize, usize> = members.iter().enumerate().map(|(li, &i)| (i, li)).collect();
    let inner: Vec<(usize, usize)> = g
        .edges
        .iter()
        .filter(|e| !e.verify && g.edge_visible(e, mode))
        .filter_map(|e| Some((*local.get(&e.from)?, *local.get(&e.to)?)))
        .collect();
    let level = levels(members.len(), &inner);
    let cols = level.iter().max().map_or(1, |m| m + 1);
    let mut columns: Vec<Vec<usize>> = vec![Vec::new(); cols];
    for (li, &l) in level.iter().enumerate() {
        columns[l].push(li);
    }
    for col in &mut columns {
        col.sort_by(|&a, &b| {
            let (x, y) = (&g.nodes[members[a]], &g.nodes[members[b]]);
            (status_rank(x.status), x.title()).cmp(&(status_rank(y.status), y.title()))
        });
    }
    // Wrap long columns into several side by side.
    let mut placed: Vec<Vec<usize>> = Vec::new();
    for col in columns {
        if col.is_empty() {
            continue;
        }
        for chunk in col.chunks(MAX_STACK) {
            placed.push(chunk.to_vec());
        }
    }
    // Room between columns for the DTO labels riding on the links.
    let widest = g
        .edges
        .iter()
        .filter(|e| !e.verify && g.edge_visible(e, mode) && local.contains_key(&e.from) && local.contains_key(&e.to))
        .filter_map(|e| e.via.filter(|&d| g.dtos[d].visible(mode)))
        .map(|d| pill_width(&g.dtos[d].name))
        .fold(0f32, f32::max);
    let gap = if widest > 0. { (widest + 24.).max(INNER_GAP_X) } else { INNER_GAP_X };
    let mut out = Vec::new();
    let mut height: f32 = 0.;
    for (c, col) in placed.iter().enumerate() {
        let x = c as f32 * (NODE_W + gap);
        let mut y = 0.;
        for &li in col {
            let i = members[li];
            let h = card_height(&g.nodes[i]);
            out.push((i, x, y, h));
            y += h + ROW_GAP;
        }
        height = height.max(y - ROW_GAP);
    }
    let width = placed.len() as f32 * NODE_W + placed.len().saturating_sub(1) as f32 * gap;
    (out, width, height)
}

/// The whole canvas: frames packed in rows as wide as `target_w` (the view
/// at 100%), so the map fills the screen instead of one long strip. Order:
/// features first (callers before what they use), then helpers, infra,
/// context, tests last.
pub fn canvas_layout(g: &Graph, mode: Mode, target_w: f32) -> Canvas {
    let n = g.nodes.len();
    let mut boxes = vec![None; n];
    let mut frames: Vec<Option<GroupBox>> = vec![None; g.groups.len()];
    // Content of every frame that has something visible.
    let mut content: Vec<Option<(Vec<(usize, f32, f32, f32)>, f32, f32)>> = Vec::new();
    for gr in &g.groups {
        let members: Vec<usize> = gr.nodes.iter().copied().filter(|&i| g.node_visible(i, mode)).collect();
        content.push((!members.is_empty()).then(|| layout_group(g, &members, mode)));
    }
    let visible: Vec<usize> = (0..g.groups.len()).filter(|&gi| content[gi].is_some()).collect();
    if visible.is_empty() {
        return Canvas { boxes, groups: frames, width: 0., height: 0. };
    }
    // Levels by links between frames: users before what they use.
    let local: HashMap<usize, usize> = visible.iter().enumerate().map(|(li, &gi)| (gi, li)).collect();
    let tail = |gi: usize| matches!(g.groups[gi].kind, GroupKind::Tests);
    let cross: Vec<(usize, usize)> = g
        .edges
        .iter()
        .filter(|e| !e.verify && g.edge_visible(e, mode))
        .filter_map(|e| {
            let (a, b) = (g.nodes[e.from].group, g.nodes[e.to].group);
            (a != b && !tail(a) && !tail(b)).then_some((*local.get(&a)?, *local.get(&b)?))
        })
        .collect();
    let level = levels(visible.len(), &cross);
    let mut order: Vec<usize> = visible.clone();
    order.sort_by_key(|&gi| (tail(gi), g.groups[gi].kind, level[local[&gi]], gi));
    // Shelf packing: left to right, a new row when the next frame would pass the edge.
    let sized: Vec<(usize, f32, f32, f32)> = order
        .iter()
        .map(|&gi| {
            let (_, cw, ch) = content[gi].as_ref().unwrap();
            // Room for the title next to its chips (kind, proof level), not just the cards.
            let title_w = (g.groups[gi].title.chars().count() as f32 * 8.6 + 230.).min(620.);
            let w = (cw + GROUP_PAD * 2.).max(NODE_W + GROUP_PAD * 2.).max(title_w);
            let head = group_head(&g.groups[gi], w);
            (gi, w, head, head + ch + GROUP_PAD)
        })
        .collect();
    let limit = target_w.max(sized.iter().map(|s| s.1).fold(0., f32::max));
    let (mut x, mut y, mut row_h, mut width): (f32, f32, f32, f32) = (0., 0., 0., 0.);
    for (gi, w, head, h) in sized {
        if x > 0. && x + w > limit {
            x = 0.;
            y += row_h + GROUP_GAP_Y;
            row_h = 0.;
        }
        frames[gi] = Some(GroupBox { x, y, w, h, head });
        if let Some((cards, _, _)) = &content[gi] {
            for &(i, cx, cy, chh) in cards {
                boxes[i] = Some((x + GROUP_PAD + cx, y + head + cy, chh));
            }
        }
        width = width.max(x + w);
        row_h = row_h.max(h);
        x += w + GROUP_GAP_X;
    }
    Canvas { boxes, groups: frames, width, height: y + row_h }
}

/// The frame to open the canvas on: the first feature, else the first frame.
pub fn main_frame(g: &Graph, c: &Canvas) -> Option<GroupBox> {
    let first = |kind: Option<GroupKind>| {
        g.groups.iter().enumerate().filter(|(_, gr)| kind.is_none_or(|k| gr.kind == k)).find_map(|(gi, _)| c.groups[gi])
    };
    first(Some(GroupKind::Feature)).or_else(|| first(None))
}

/// Width of a DTO label on a link.
pub fn pill_width(name: &str) -> f32 {
    name.chars().count().min(24) as f32 * 6.3 + 16.
}

/// Link endpoints and bezier controls between two cards: side to side when
/// they are apart horizontally, else bottom to top (or top to bottom).
pub fn link_curve(from: (f32, f32, f32), to: (f32, f32, f32)) -> [(f32, f32); 4] {
    let (fx, fy, fh) = from;
    let (tx, ty, th) = to;
    let (a, b, horizontal) = if tx >= fx + NODE_W * 0.6 {
        ((fx + NODE_W, fy + 22.), (tx, ty + 22.), true)
    } else if tx + NODE_W * 0.6 <= fx {
        ((fx, fy + 22.), (tx + NODE_W, ty + 22.), true)
    } else if ty >= fy {
        ((fx + NODE_W / 2., fy + fh), (tx + NODE_W / 2., ty), false)
    } else {
        ((fx + NODE_W / 2., fy), (tx + NODE_W / 2., ty + th), false)
    };
    let (c1, c2) = if horizontal {
        let dx = ((b.0 - a.0).abs() / 2.).max(28.) * if b.0 >= a.0 { 1. } else { -1. };
        ((a.0 + dx, a.1), (b.0 - dx, b.1))
    } else {
        let dy = ((b.1 - a.1).abs() / 2.).max(18.) * if b.1 >= a.1 { 1. } else { -1. };
        ((a.0, a.1 + dy), (b.0, b.1 - dy))
    };
    [a, c1, c2, b]
}

/// Point of the curve at t = 0.5: where a link's DTO label sits.
pub fn curve_mid(c: &[(f32, f32); 4]) -> (f32, f32) {
    ((c[0].0 + 3. * c[1].0 + 3. * c[2].0 + c[3].0) / 8., (c[0].1 + 3. * c[1].1 + 3. * c[2].1 + c[3].1) / 8.)
}

// ------------------------------------------------------------------ signature diff

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Same,
    Added,
    Removed,
}

/// Identifiers stay whole, every other character is its own token.
fn tokens(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, ch) in s.char_indices() {
        let word = ch.is_alphanumeric() || ch == '_' || ch == '$';
        match (word, start) {
            (true, None) => start = Some(i),
            (false, Some(st)) => {
                out.push(&s[st..i]);
                start = None;
                out.push(&s[i..i + ch.len_utf8()]);
            }
            (false, None) => out.push(&s[i..i + ch.len_utf8()]),
            (true, Some(_)) => {}
        }
    }
    if let Some(st) = start {
        out.push(&s[st..]);
    }
    out
}

/// Before → after as runs of same / removed / added text.
pub fn sig_diff(before: &str, after: &str) -> Vec<(String, Part)> {
    use similar::{Algorithm, DiffOp, capture_diff_slices};
    let (old, new) = (tokens(before), tokens(after));
    let mut out: Vec<(String, Part)> = Vec::new();
    let mut push = |text: String, part: Part| {
        if text.is_empty() {
            return;
        }
        match out.last_mut() {
            Some((t, p)) if *p == part => t.push_str(&text),
            _ => out.push((text, part)),
        }
    };
    for op in capture_diff_slices(Algorithm::Myers, &old, &new) {
        match op {
            DiffOp::Equal { old_index, len, .. } => push(old[old_index..old_index + len].concat(), Part::Same),
            DiffOp::Delete { old_index, old_len, .. } => push(old[old_index..old_index + old_len].concat(), Part::Removed),
            DiffOp::Insert { new_index, new_len, .. } => push(new[new_index..new_index + new_len].concat(), Part::Added),
            DiffOp::Replace { old_index, old_len, new_index, new_len } => {
                push(old[old_index..old_index + old_len].concat(), Part::Removed);
                push(new[new_index..new_index + new_len].concat(), Part::Added);
            }
        }
    }
    out
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
        let g = build_report(&r);
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
    fn features_group_nodes_and_tests_verify() {
        let r = report(
            r#"{"code":{
                "added":[{"id":"dialog","name":"JokerPickDialog","role":"component","entity":"Диалог выбора Джокера","uses":["fly"]},
                         {"id":"fly","name":"takeFlight","kind":"function","entity":"Полёт карты в альбом"},
                         {"id":"rect","name":"toRect","kind":"function"}],
                "changed":[{"id":"cfg","name":"JOKER_CARD_ATTR","kind":"config"}],
                "context":[{"id":"layout","name":"AppLayout","uses":["dialog"]}]},
              "features":[{"id":"f1","title":"Полёт Джокера","summary":"Карта улетает в альбом","kind":"feature","nodes":["dialog","fly"]},
                          {"id":"f2","title":"Помогает","kind":"helper","nodes":["rect","cfg"]}],
              "tests":[{"id":"t1","title":"полёт стартует после выбора","file":"fly.test.ts","verifies":["fly"],"kind":"unit","status":"added","result":"pass"}]}"#,
        );
        let g = build_report(&r);
        let ix = |k: &str| g.index(k).unwrap();
        let group = |k: &str| g.groups[g.nodes[ix(k)].group].title.as_str();
        assert_eq!((group("dialog"), group("fly")), ("Полёт Джокера", "Полёт Джокера"));
        assert_eq!((group("rect"), group("cfg")), ("Помогает", "Помогает"));
        assert_eq!(group("layout"), "Окружение");
        assert_eq!(group("t1"), "Тесты");
        assert_eq!(g.groups.iter().map(|x| x.kind).collect::<Vec<_>>(), [GroupKind::Feature, GroupKind::Helper, GroupKind::Context, GroupKind::Tests]);
        // Roles: explicit, else from kind; the test card and its dashed link.
        assert_eq!((g.nodes[ix("dialog")].role(), g.nodes[ix("cfg")].role(), g.nodes[ix("t1")].role()), (Role::Component, Role::Config, Role::Test));
        assert_eq!(g.nodes[ix("dialog")].title(), "Диалог выбора Джокера");
        assert_eq!(g.nodes[ix("t1")].test.as_ref().map(|t| t.result.as_str()), Some("pass"));
        let verify = g.edges.iter().find(|e| e.verify).unwrap();
        assert_eq!((verify.from, verify.to, verify.kind), (ix("t1"), ix("fly"), EdgeKind::New));
        assert_eq!(g.tests_of(ix("fly")), vec![ix("t1")]);
        // Untested: changed behaviour without a test; configs do not count.
        assert!(g.has_tests() && g.untested(ix("dialog")) && !g.untested(ix("fly")) && !g.untested(ix("cfg")) && !g.untested(ix("layout")));
    }

    #[core::prelude::v1::test]
    fn old_reports_frame_by_file() {
        let r = report(r#"{"code":{"added":[{"name":"sub","file":"src/m.js"},{"name":"x","file":"n.js"}],"removed":[{"name":"mul","file":"src/m.js"}]}}"#);
        let g = build_report(&r);
        let titles: Vec<&str> = g.groups.iter().map(|x| x.title.as_str()).collect();
        assert_eq!(titles, ["m.js", "n.js"]);
        assert!(g.groups.iter().all(|x| x.kind == GroupKind::File));
        assert!(!g.has_tests());
    }

    #[core::prelude::v1::test]
    fn canvas_frames_do_not_overlap_and_follow_links() {
        let long = "x".repeat(120);
        let json = format!(
            r#"{{"code":{{"changed":[{{"id":"a","name":"A","signature":"{long}","uses":["b","c","d","h"]}}],
                "added":[{{"id":"b","name":"VeryLongComponentNameThatWrapsOnTwoLinesForSure","signature":"B() → JSX"}},{{"id":"c","name":"C"}},{{"id":"d","name":"D","signature":"{long}"}},
                         {{"id":"h","name":"helper"}},{{"id":"h2","name":"helper2"}},{{"id":"h3","name":"h3"}},{{"id":"h4","name":"h4"}},{{"id":"h5","name":"h5"}},{{"id":"h6","name":"h6"}}],
                "removed":[{{"id":"e","name":"E"}}]}},
              "features":[{{"id":"f","title":"Фича","nodes":["a","b","c","d","e"]}},{{"id":"hp","title":"Помощники","kind":"helper","nodes":["h","h2","h3","h4","h5","h6"]}}],
              "tests":[{{"id":"t","title":"тест","verifies":["a"]}}]}}"#
        );
        let r = report(&json);
        let g = build_report(&r);
        for mode in [Mode::Before, Mode::After, Mode::Diff] {
            let c = canvas_layout(&g, mode, 1200.);
            let frames: Vec<GroupBox> = c.groups.iter().flatten().copied().collect();
            for (i, a) in frames.iter().enumerate() {
                for b in &frames[i + 1..] {
                    let apart = a.x + a.w <= b.x || b.x + b.w <= a.x || a.y + a.h <= b.y || b.y + b.h <= a.y;
                    assert!(apart, "{mode:?}: frames overlap {a:?} {b:?}");
                }
            }
            let cards: Vec<(usize, (f32, f32, f32))> = c.boxes.iter().enumerate().filter_map(|(i, b)| b.map(|b| (i, b))).collect();
            for (i, a) in &cards {
                // Inside its frame.
                let f = c.groups[g.nodes[*i].group].unwrap();
                assert!(a.0 >= f.x && a.0 + NODE_W <= f.x + f.w + 0.1 && a.1 >= f.y + f.head && a.1 + a.2 <= f.y + f.h + 0.1, "{mode:?}: card {i} outside frame");
                for (j, b) in &cards {
                    if i < j && (a.0 - b.0).abs() < 1. {
                        assert!(a.1 + a.2 <= b.1 || b.1 + b.2 <= a.1, "{mode:?}: cards {i} and {j} overlap");
                    }
                }
            }
            assert_eq!(c.boxes[g.index("e").unwrap()].is_some(), mode != Mode::After);
        }
        let c = canvas_layout(&g, Mode::After, 1200.);
        let at = |key: &str| {
            let b = c.groups[g.group_index(key).unwrap()].unwrap();
            (b.y, b.x)
        };
        // Reading order: the feature, then the helpers it uses, tests last.
        assert!(at("f") < at("hp") && at("hp") < at("__tests"));
        // Six helpers wrap into two columns of the frame.
        let xs: HashSet<i32> = ["h", "h2", "h3", "h4", "h5", "h6"].iter().map(|k| c.boxes[g.index(k).unwrap()].unwrap().0 as i32).collect();
        assert_eq!(xs.len(), 2);
        let a = &g.nodes[g.index("a").unwrap()];
        assert_eq!(sig_lines(a, Mode::After), SIG_MAX_LINES);
        assert!(node_height(a, Mode::After) > node_height(&g.nodes[g.index("c").unwrap()], Mode::After));
        // At rest a card is just the role row and the title.
        assert!(card_height(a) < node_height(a, Mode::After));
        assert_eq!(c.boxes[g.index("a").unwrap()].unwrap().2, card_height(a));
    }

    #[core::prelude::v1::test]
    fn frames_pack_into_rows_as_wide_as_the_view() {
        // Six independent features: a narrow view wraps them into rows, a wide one keeps one row.
        let features: Vec<String> = (0..6).map(|i| format!(r#"{{"id":"f{i}","title":"Фича {i}","nodes":["n{i}"]}}"#)).collect();
        let nodes: Vec<String> = (0..6).map(|i| format!(r#"{{"id":"n{i}","name":"N{i}"}}"#)).collect();
        let r = report(&format!(r#"{{"code":{{"added":[{}]}},"features":[{}]}}"#, nodes.join(","), features.join(",")));
        let g = build_report(&r);
        let narrow = canvas_layout(&g, Mode::After, 700.);
        let rows: HashSet<i32> = narrow.groups.iter().flatten().map(|b| b.y as i32).collect();
        assert!(rows.len() >= 3, "{rows:?}");
        assert!(narrow.width <= 700. + 1.);
        let wide = canvas_layout(&g, Mode::After, 4000.);
        let rows: HashSet<i32> = wide.groups.iter().flatten().map(|b| b.y as i32).collect();
        assert_eq!(rows.len(), 1);
        // Opens on the first feature frame.
        assert_eq!(main_frame(&g, &narrow), narrow.groups[0]);
    }

    #[core::prelude::v1::test]
    fn unknown_targets_become_context_and_iterations_diff() {
        let r = report(r#"{"code":{"added":[{"name":"x","uses":["B"]}],"changed":[{"name":"NewSessionForm / StartSession","note":"v1"}]}}"#);
        let g1 = build_report(&r);
        assert!(g1.nodes.iter().any(|n| n.implicit && n.name == "B"));
        let r2 = report(r#"{"code":{"added":[{"name":"x","uses":["B"]},{"name":"y"}],"changed":[{"name":"NewSessionForm / StartSession","note":"v2"}]}}"#);
        let g2 = build_report(&r2);
        let changed = g2.changed_since(&g1);
        assert!(changed.contains("y") && changed.contains("newsessionform / startsession") && !changed.contains("x"));
        assert_eq!(aliases(&Entry { name: "NewSessionForm / StartSession".into(), ..Default::default() }), vec!["newsessionform / startsession", "newsessionform", "startsession"]);
    }

    #[core::prelude::v1::test]
    fn signatures_dtos_and_labelled_links() {
        let r = report(
            r#"{"code":{
                "changed":[{"id":"layout","name":"AppLayout","kind":"component","signature":"AppLayout({ user }: LayoutProps) → JSX",
                            "uses":["prompt", {"to":"dialog","via":"dialog_props","note":"открывает диалог"}]}],
                "added":[{"id":"prompt","name":"WelcomeBonusPrompt","signature":"WelcomeBonusPrompt({ amount }: PromptProps) → JSX","inputs":["prompt_props"]},
                         {"id":"dialog","name":"WelcomeBonusDialog","signature":"WelcomeBonusDialog({ open, onClose, amount }) → JSX",
                          "signature_before":"WelcomeBonusDialog({ open }) → JSX","inputs":["dialog_props"]}],
                "context":[{"id":"api","name":"fetchBonus","uses":["bonus_dto"]}]},
              "dtos":[{"id":"dialog_props","name":"DialogProps","kind":"props","status":"changed",
                       "fields":[{"name":"open","type":"boolean"},{"name":"amount","type":"number","status":"added"},"legacy: string"]},
                      {"id":"prompt_props","name":"PromptProps","status":"added","fields":["amount: number"],"used_by":["layout"]},
                      {"id":"bonus_dto","name":"BonusResponse","kind":"response"}]}"#,
        );
        let g = build_report(&r);
        let ix = |k: &str| g.index(k).unwrap();
        let edge = |a: &str, b: &str| *g.edges.iter().find(|e| e.from == ix(a) && e.to == ix(b)).unwrap();
        let d = |k: &str| g.dto_index(k).unwrap();
        // Explicit `via`, and the one inferred from what both sides share.
        assert_eq!(edge("layout", "dialog").via, Some(d("dialog_props")));
        assert_eq!(g.notes.get(&(ix("layout"), ix("dialog"))).map(String::as_str), Some("открывает диалог"));
        assert_eq!(edge("layout", "prompt").via, Some(d("prompt_props")));
        // A `uses` pointing at a DTO is an output, not a node.
        assert!(g.nodes[ix("api")].outputs.contains(&d("bonus_dto")));
        assert!(g.nodes.iter().all(|n| n.name != "bonus_dto"));
        // Fields: objects and "name: type" strings, statuses.
        let props = &g.dtos[d("dialog_props")];
        assert_eq!(props.status, NodeStatus::Changed);
        assert_eq!(props.fields[1], MapField { name: "amount".into(), ty: "number".into(), status: NodeStatus::Added, note: String::new() });
        assert_eq!((props.fields[2].name.as_str(), props.fields[2].ty.as_str()), ("legacy", "string"));
        assert_eq!(props.touched(), 1);
        assert!(props.users.contains(&ix("layout")) && props.users.contains(&ix("dialog")));
        assert!(g.dtos[d("prompt_props")].users.contains(&ix("layout")));
        assert!(!g.dtos[d("prompt_props")].visible(Mode::Before));
        // Signatures: the mode decides which one shows.
        let dialog = &g.nodes[ix("dialog")];
        assert!(dialog.signature_changed());
        assert_eq!(dialog.signature_in(Mode::Before), "WelcomeBonusDialog({ open }) → JSX");
        let parts = signature_parts(dialog, Mode::Diff);
        assert!(parts.iter().any(|(t, p)| *p == Part::Added && t.contains("onClose")));
        assert!(parts.iter().any(|(t, p)| *p == Part::Same && t.starts_with("WelcomeBonusDialog")));
    }

    #[core::prelude::v1::test]
    fn sig_diff_keeps_identifiers_whole() {
        let parts = sig_diff("takeFlight(card: Card) → void", "takeFlight(card: JokerCard, from: Rect) → Promise<void>");
        let text = |p: Part| parts.iter().filter(|(_, x)| *x == p).map(|(t, _)| t.as_str()).collect::<String>();
        assert!(text(Part::Removed).contains("Card"));
        assert!(text(Part::Added).contains("JokerCard") && text(Part::Added).contains("Promise"));
        assert!(text(Part::Same).starts_with("takeFlight(card: "));
        // Joining the same + added runs gives the new signature back.
        let after: String = parts.iter().filter(|(_, x)| *x != Part::Removed).map(|(t, _)| t.as_str()).collect();
        assert_eq!(after, "takeFlight(card: JokerCard, from: Rect) → Promise<void>");
    }
}
