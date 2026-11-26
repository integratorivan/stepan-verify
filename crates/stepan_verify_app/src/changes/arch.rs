//! «Архитектура»: modules (files) and the imports between them, before and
//! after the session, from `arch.json` the app computed when the run started
//! (`stepan_verify_core::arch`). The reviewer only adds what each module is for.

use std::{
    collections::{HashMap, HashSet},
    path::Path,
    rc::Rc,
};

use gpui_kit::component::{Theme, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use stepan_verify_core::arch::{Arch, Module, Status};

use super::{ChangesView, Entry, Report};
use crate::{
    ui::{self, Palette, Tone},
    zoom::px,
};

pub fn load(dir: &Path) -> Option<Arch> {
    serde_json::from_str(&std::fs::read_to_string(dir.join("arch.json")).ok()?).ok()
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    Before,
    After,
    #[default]
    Diff,
}

pub const NODE_W: f32 = 172.;
pub const NODE_H: f32 = 46.;
/// Space between lanes: links between packages run here.
const GAP_X: f32 = 60.;
const GAP_Y: f32 = 12.;
/// Lane title above the boxes.
const HEAD: f32 = 26.;
const PAD: f32 = 12.;
/// A lane taller than this splits by the next folder level.
const MAX_LANE: usize = 12;

/// Where every module box and every link goes.
#[derive(Clone, Debug, Default)]
pub struct Laid {
    /// Top-left of each module, by index in `Arch::modules`.
    pub pos: Vec<(f32, f32)>,
    /// A cubic Bézier per link (by index in `Arch::edges`): start, control 1, control 2, end.
    pub paths: Vec<[(f32, f32); 4]>,
    /// Lane titles: text and left x.
    pub lanes: Vec<(String, f32)>,
    pub size: (f32, f32),
}

/// The lane of every module: its package when there are several, otherwise
/// the top folder under the common root (split further while a lane is huge).
pub fn lane_keys(a: &Arch) -> Vec<String> {
    let inner: Vec<&Module> = a.modules.iter().filter(|m| !m.external).collect();
    let packages: HashSet<&str> = inner.iter().map(|m| m.package.as_str()).collect();
    if packages.len() > 1 {
        return a.modules.iter().map(|m| if m.external { "пакеты".to_owned() } else { m.package.clone() }).collect();
    }
    // One package: folders below the common root.
    let dirs: Vec<Vec<&str>> = inner.iter().map(|m| m.id.split('/').collect::<Vec<_>>()).collect();
    let common = dirs.first().map_or(0, |f| (0..f.len().saturating_sub(1)).take_while(|&i| dirs.iter().all(|d| d.len() > i + 1 && d[i] == f[i])).count());
    let mut depth = 1;
    let key = |m: &Module, depth: usize| -> String {
        let parts: Vec<&str> = m.id.split('/').collect();
        let dir = &parts[common.min(parts.len())..parts.len().saturating_sub(1)];
        if dir.is_empty() { "./".to_owned() } else { dir[..depth.min(dir.len())].join("/") }
    };
    loop {
        let mut count: HashMap<String, usize> = HashMap::new();
        for m in &inner {
            *count.entry(key(m, depth)).or_default() += 1;
        }
        if depth >= 3 || count.values().all(|&c| c <= MAX_LANE) {
            break;
        }
        depth += 1;
    }
    a.modules.iter().map(|m| if m.external { "пакеты".to_owned() } else { key(m, depth) }).collect()
}

/// Lanes left to right by import direction (importers first), modules top to
/// bottom: new, changed, removed, unchanged, then nudged toward their links.
pub fn layout(a: &Arch) -> Laid {
    let n = a.modules.len();
    let keys = lane_keys(a);
    let ix: HashMap<&str, usize> = a.modules.iter().enumerate().map(|(i, m)| (m.id.as_str(), i)).collect();
    let ends: Vec<(usize, usize)> = a.edges.iter().map(|e| (ix.get(e.from.as_str()).copied().unwrap_or(0), ix.get(e.to.as_str()).copied().unwrap_or(0))).collect();
    // Lane order: longest path over lane links, cycles broken by first seen.
    let mut names: Vec<String> = keys.iter().cloned().collect::<HashSet<_>>().into_iter().collect();
    names.sort();
    let lane_ix: HashMap<&str, usize> = names.iter().enumerate().map(|(i, k)| (k.as_str(), i)).collect();
    let l = names.len();
    let mut lane_edges: HashSet<(usize, usize)> = HashSet::new();
    for &(f, t) in &ends {
        let (lf, lt) = (lane_ix[keys[f].as_str()], lane_ix[keys[t].as_str()]);
        if lf != lt {
            lane_edges.insert((lf, lt));
        }
    }
    let mut rank = vec![0usize; l];
    for _ in 0..l {
        for &(f, t) in &lane_edges {
            // Both directions present: a cycle; let the one with more links lead.
            if lane_edges.contains(&(t, f)) && f > t {
                continue;
            }
            rank[t] = rank[t].max(rank[f] + 1).min(l);
        }
    }
    let mut order: Vec<usize> = (0..l).collect();
    order.sort_by_key(|&i| (names[i] == "пакеты", rank[i], names[i].clone()));
    let col: HashMap<usize, usize> = order.iter().enumerate().map(|(c, &i)| (i, c)).collect();
    // Modules inside lanes.
    let weight = |m: &Module| match m.status {
        Status::Added => 0,
        Status::Changed => 1,
        Status::Removed => 2,
        Status::Same => 3,
    };
    let mut lanes: Vec<Vec<usize>> = vec![Vec::new(); l];
    for (i, k) in keys.iter().enumerate() {
        lanes[col[&lane_ix[k.as_str()]]].push(i);
    }
    let mut slot = vec![0f32; n];
    for lane in lanes.iter_mut() {
        lane.sort_by_key(|&i| (weight(&a.modules[i]), a.modules[i].label.clone()));
        for (k, &i) in lane.iter().enumerate() {
            slot[i] = k as f32;
        }
    }
    // Two passes: within a status group, follow the average slot of linked modules in other lanes.
    for _ in 0..2 {
        for lane in lanes.iter_mut() {
            let bc: HashMap<usize, f32> = lane
                .iter()
                .map(|&i| {
                    let linked: Vec<f32> = ends.iter().filter_map(|&(f, t)| if f == i && keys[t] != keys[i] { Some(slot[t]) } else if t == i && keys[f] != keys[i] { Some(slot[f]) } else { None }).collect();
                    (i, if linked.is_empty() { slot[i] } else { linked.iter().sum::<f32>() / linked.len() as f32 })
                })
                .collect();
            lane.sort_by(|&x, &y| weight(&a.modules[x]).cmp(&weight(&a.modules[y])).then(bc[&x].partial_cmp(&bc[&y]).unwrap_or(std::cmp::Ordering::Equal)));
            for (k, &i) in lane.iter().enumerate() {
                slot[i] = k as f32;
            }
        }
    }
    let mut pos = vec![(0f32, 0f32); n];
    let mut titles = Vec::with_capacity(l);
    let mut tallest = 0usize;
    for (c, lane) in lanes.iter().enumerate() {
        let x = PAD + c as f32 * (NODE_W + GAP_X);
        titles.push((names[order[c]].clone(), x));
        tallest = tallest.max(lane.len());
        for (k, &i) in lane.iter().enumerate() {
            pos[i] = (x, PAD + HEAD + k as f32 * (NODE_H + GAP_Y));
        }
    }
    let paths = ends
        .iter()
        .map(|&(f, t)| {
            let (a0, b0) = (pos[f], pos[t]);
            if (a0.0 - b0.0).abs() < 1. {
                // Same lane: an arc out of the right side and back in.
                let (x, y1, y2) = (a0.0 + NODE_W, a0.1 + NODE_H / 2., b0.1 + NODE_H / 2.);
                let bulge = 22. + (y2 - y1).abs().min(240.) * 0.12;
                [(x, y1), (x + bulge, y1), (x + bulge, y2), (x + 2., y2)]
            } else if b0.0 > a0.0 {
                let (s, e) = ((a0.0 + NODE_W, a0.1 + NODE_H / 2.), (b0.0, b0.1 + NODE_H / 2.));
                let dx = (e.0 - s.0) / 2.;
                [s, (s.0 + dx, s.1), (e.0 - dx, e.1), e]
            } else {
                let (s, e) = ((a0.0, a0.1 + NODE_H / 2.), (b0.0 + NODE_W, b0.1 + NODE_H / 2.));
                let dx = (s.0 - e.0) / 2.;
                [s, (s.0 - dx, s.1), (e.0 + dx, e.1), e]
            }
        })
        .collect();
    let width = PAD * 2. + l as f32 * NODE_W + l.saturating_sub(1) as f32 * GAP_X + 30.;
    let height = PAD * 2. + HEAD + tallest as f32 * (NODE_H + GAP_Y);
    Laid { pos, paths, lanes: titles, size: (width, height) }
}

/// A link to paint: Bézier points, color, width, dashed.
type Line = ([(f32, f32); 4], Hsla, f32, bool);

/// What a mode shows.
fn visible(mode: Mode, s: Status) -> bool {
    match mode {
        Mode::Before => s != Status::Added,
        Mode::After => s != Status::Removed,
        Mode::Diff => true,
    }
}

pub fn tone(s: Status, p: &Palette) -> Option<Tone> {
    match s {
        Status::Added => Some(p.green),
        Status::Changed => Some(p.amber),
        Status::Removed => Some(p.red),
        Status::Same => None,
    }
}

pub fn status_word(s: Status) -> &'static str {
    match s {
        Status::Added => "новый",
        Status::Changed => "изменён",
        Status::Removed => "удалён",
        Status::Same => "без изменений",
    }
}

/// Same path, written from the project folder or the repository root.
pub fn same_file(a: &str, b: &str) -> bool {
    let (a, b) = (a.trim_start_matches("./"), b.trim_start_matches("./"));
    !a.is_empty() && !b.is_empty() && (a == b || a.ends_with(&format!("/{b}")) || b.ends_with(&format!("/{a}")))
}

/// Package A imports package B for the first time (or no longer), and import cycles between packages.
pub struct PackageLinks {
    pub added: Vec<(String, String)>,
    pub removed: Vec<(String, String)>,
    pub cycles: Vec<(String, String)>,
}

pub fn package_links(a: &Arch) -> PackageLinks {
    let pkg: HashMap<&str, &str> = a.modules.iter().map(|m| (m.id.as_str(), m.package.as_str())).collect();
    let mut before: HashSet<(String, String)> = HashSet::new();
    let mut after: HashSet<(String, String)> = HashSet::new();
    for e in &a.edges {
        let (Some(f), Some(t)) = (pkg.get(e.from.as_str()), pkg.get(e.to.as_str())) else { continue };
        if f == t {
            continue;
        }
        let k = (f.to_string(), t.to_string());
        if e.status != Status::Added {
            before.insert(k.clone());
        }
        if e.status != Status::Removed {
            after.insert(k);
        }
    }
    let mut added: Vec<_> = after.difference(&before).cloned().collect();
    let mut removed: Vec<_> = before.difference(&after).cloned().collect();
    let mut cycles: Vec<_> = after.iter().filter(|(f, t)| f < t && after.contains(&(t.clone(), f.clone()))).cloned().collect();
    added.sort();
    removed.sort();
    cycles.sort();
    PackageLinks { added, removed, cycles }
}

/// The common prefix of package names (`stepan_verify_`), dropped on screen.
pub fn package_prefix(a: &Arch) -> String {
    let names: Vec<&str> = a.modules.iter().filter(|m| !m.external).map(|m| m.package.as_str()).collect::<HashSet<_>>().into_iter().collect();
    if names.len() < 2 {
        return String::new();
    }
    let first = names[0];
    let mut len = first.len();
    for n in &names[1..] {
        len = len.min(first.bytes().zip(n.bytes()).take_while(|(x, y)| x == y).count());
    }
    let pre = &first[..len];
    // Cut at a separator so «stepan_verify_» stays whole.
    match pre.rfind(['_', '-', '/']) {
        Some(i) => pre[..=i].to_owned(),
        None => String::new(),
    }
}

pub fn short_package<'a>(name: &'a str, prefix: &str) -> &'a str {
    name.strip_prefix(prefix).filter(|s| !s.is_empty()).unwrap_or(name)
}

/// The graph of a run: the structure and where everything goes.
pub struct Graph {
    pub arch: Arch,
    pub laid: Laid,
}

impl Graph {
    pub fn new(arch: Arch) -> Self {
        let laid = layout(&arch);
        Graph { arch, laid }
    }
}

impl ChangesView {
    /// arch.json of the run on screen, laid out once per run.
    pub(super) fn arch_graph(&self) -> Option<Rc<Graph>> {
        let dir = self.run_dir();
        if let Some((d, g)) = self.arch_cache.borrow().as_ref() {
            if *d == dir {
                return g.clone();
            }
        }
        let g = load(&dir).filter(|a| !a.modules.is_empty()).map(|a| Rc::new(Graph::new(a)));
        *self.arch_cache.borrow_mut() = Some((dir, g.clone()));
        g
    }

    pub(super) fn render_arch(&self, r: &Report, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let Some(g) = self.arch_graph() else {
            return empty_state(
                "Графа модулей у этой итерации нет",
                "Его считает приложение при сборке отчёта (с Stepan 0.2.9). Нажми «Пересобрать» — модули и импорты было → стало появятся здесь.",
                p,
            );
        };
        let a = &g.arch;
        let mode = self.arch_mode;
        let sel = self.arch_sel.clone().filter(|id| a.module(id).is_some()).or_else(|| a.modules.iter().find(|m| m.status == Status::Added && !m.external).or(a.modules.iter().find(|m| m.status != Status::Same)).map(|m| m.id.clone()));
        let prefix = package_prefix(a);
        let pkg_links = package_links(a);
        let new_edges = a.edges.iter().filter(|e| e.status == Status::Added).count();
        let gone_edges = a.edges.iter().filter(|e| e.status == Status::Removed).count();
        let mono = theme.mono_font_family.clone();

        let seg = |id: &'static str, label: &'static str, m: Mode, cx: &mut Context<Self>| {
            ui::segment(id, label, mode == m, p).px(px(11.)).h(px(26.)).text_size(px(12.5)).on_click(cx.listener(move |this, _, _, cx| {
                this.arch_mode = m;
                cx.notify();
            }))
        };
        let toolbar = h_flex()
            .gap(px(14.))
            .flex_wrap()
            .child(ui::segmented(p).child(seg("arch-before", "Было", Mode::Before, cx)).child(seg("arch-after", "Стало", Mode::After, cx)).child(seg("arch-diff", "Разница", Mode::Diff, cx)))
            .child(legend_line(p.green.solid, false, "новый импорт", p))
            .child(legend_line(p.red.solid, true, "убран", p))
            .child(legend_line(p.border_strong, false, "был", p))
            .child(ui::mono(format!("{} модулей · +{new_edges} −{gone_edges} связей", a.modules.len()), theme).text_color(p.faint));

        // The graph: links on a canvas under absolutely placed boxes.
        let (w, h) = g.laid.size;
        let ix: HashMap<&str, usize> = a.modules.iter().enumerate().map(|(i, m)| (m.id.as_str(), i)).collect();
        let lines: Vec<Line> = a
            .edges
            .iter()
            .enumerate()
            .filter(|(_, e)| visible(mode, e.status) && [&e.from, &e.to].iter().all(|id| ix.get(id.as_str()).is_some_and(|&i| visible(mode, a.modules[i].status))))
            // Unchanged links only around the module you picked: the change stays readable.
            .filter(|(_, e)| e.status != Status::Same || sel.as_ref().is_some_and(|s| *s == e.from || *s == e.to))
            .map(|(i, e)| {
                let hot = sel.as_ref().is_some_and(|s| *s == e.from || *s == e.to);
                let base = match (mode, e.status) {
                    (Mode::Before, _) | (Mode::After, Status::Same) => p.border_strong,
                    (_, Status::Added) => p.green.solid,
                    (_, Status::Removed) => p.red.solid,
                    _ => p.border_strong,
                };
                let color = if hot {
                    if base == p.border_strong { p.ink.opacity(0.75) } else { base }
                } else if sel.is_some() || (mode == Mode::Diff && e.status == Status::Same) {
                    base.opacity(0.35)
                } else {
                    base
                };
                let width = if hot { 2.2 } else if e.status != Status::Same && mode != Mode::Before { 1.9 } else { 1.3 };
                (g.laid.paths[i], color, width, e.status == Status::Removed && mode == Mode::Diff)
            })
            .collect();
        let links = canvas(
            move |_, _, _| lines,
            move |bounds, lines, window, _| {
                let o = bounds.origin;
                let pt = |(x, y): (f32, f32)| point(px(x), px(y)) + o;
                for (c, color, width, dashed) in lines {
                    let mut path = PathBuilder::stroke(px(width));
                    if dashed {
                        path = path.dash_array(&[px(4.), px(3.)]);
                    }
                    path.move_to(pt(c[0]));
                    path.cubic_bezier_to(pt(c[3]), pt(c[1]), pt(c[2]));
                    let last_c2 = pt(c[2]);
                    if let Ok(path) = path.build() {
                        window.paint_path(path, color);
                    }
                    let b = pt(c[3]);
                    let (dx, dy) = (f32::from(b.x - last_c2.x), f32::from(b.y - last_c2.y));
                    let len = (dx * dx + dy * dy).sqrt().max(0.001);
                    let (ux, uy) = (dx / len, dy / len);
                    let back = point(b.x - px(ux * 7.), b.y - px(uy * 7.));
                    let side = (px(-uy * 3.5), px(ux * 3.5));
                    let mut head = PathBuilder::fill();
                    head.move_to(b);
                    head.line_to(point(back.x + side.0, back.y + side.1));
                    head.line_to(point(back.x - side.0, back.y - side.1));
                    head.close();
                    if let Ok(head) = head.build() {
                        window.paint_path(head, color);
                    }
                }
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();
        let boxes = a.modules.iter().enumerate().filter(|(_, m)| visible(mode, m.status)).map(|(i, m)| {
            let (x, y) = g.laid.pos[i];
            let t = if mode == Mode::Before { None } else { tone(m.status, p) };
            let picked = sel.as_deref() == Some(m.id.as_str());
            let dim = mode == Mode::Diff && m.status == Status::Same && !picked;
            let id = m.id.clone();
            let sub = if m.external {
                "пакет".to_owned()
            } else if mode == Mode::Before || m.status == Status::Same {
                short_package(&m.package, &prefix).to_owned()
            } else {
                format!("{} · +{} −{}", short_package(&m.package, &prefix), m.plus, m.minus)
            };
            div()
                .id(SharedString::from(format!("arch-node-{i}")))
                .absolute()
                .left(px(x))
                .top(px(y))
                .w(px(NODE_W))
                .h(px(NODE_H))
                .px(px(11.))
                .py(px(7.))
                .rounded(px(9.))
                .border_1()
                .when(m.external || m.status == Status::Removed && mode == Mode::Diff, |d| d.border_dashed())
                .border_color(if picked { p.ink } else { t.map_or(p.border_strong, |t| t.solid) })
                .when(picked, |d| d.border_2())
                .bg(match t {
                    Some(t) if m.status == Status::Added => ui::mix(t.soft, p.surface, 0.55),
                    _ if m.external => p.surface2,
                    _ => p.surface,
                })
                .when(dim, |d| d.opacity(0.55))
                .cursor_pointer()
                .hover(|s| s.shadow(ui::card_shadow()))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.arch_sel = Some(id.clone());
                    cx.notify();
                }))
                .child(
                    h_flex()
                        .gap(px(5.))
                        .child(div().min_w_0().truncate().text_size(px(12.5)).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).child(m.label.clone()))
                        .when(m.status == Status::Added && mode != Mode::Before, |d| d.child(div().text_size(px(11.)).text_color(p.green.text).child("✦"))),
                )
                .child(div().truncate().font_family(mono.clone()).text_size(px(10.5)).text_color(p.muted).child(sub))
        });
        let graph = div()
            .id("arch-scroll")
            .w_full()
            .overflow_x_scroll()
            .child(
                div()
                    .relative()
                    .w(px(w))
                    .h(px(h))
                    .children(g.laid.lanes.iter().map(|(name, x)| {
                        div()
                            .absolute()
                            .left(px(*x))
                            .top(px(PAD))
                            .w(px(NODE_W))
                            .truncate()
                            .font_family(mono.clone())
                            .text_size(px(10.5))
                            .text_color(p.faint)
                            .child(short_package(name, &prefix).to_uppercase())
                    }))
                    .child(links)
                    .children(boxes),
            );

        // Package-level news: a crate or package that starts (or stops) importing another, and cycles.
        let mut notes: Vec<AnyElement> = Vec::new();
        for (f, t) in &pkg_links.added {
            notes.push(pkg_note(&format!("{} → {}", short_package(f, &prefix), short_package(t, &prefix)), "новая зависимость между пакетами", p.green, p).into_any_element());
        }
        for (f, t) in &pkg_links.removed {
            notes.push(pkg_note(&format!("{} → {}", short_package(f, &prefix), short_package(t, &prefix)), "больше не импортирует", super::contracts::gray(p), p).into_any_element());
        }
        for (f, t) in &pkg_links.cycles {
            notes.push(pkg_note(&format!("{} ⇄ {}", short_package(f, &prefix), short_package(t, &prefix)), "цикл импортов между пакетами", p.red, p).into_any_element());
        }
        if pkg_links.cycles.is_empty() {
            notes.push(div().text_size(px(12.5)).text_color(p.green.text).child("✓ циклов между пакетами нет").into_any_element());
        }
        if a.skipped > 0 {
            notes.push(div().text_size(px(12.5)).text_color(p.muted).child(format!("ещё {} изменённых файлов с кодом не вошли в граф", a.skipped)).into_any_element());
        }

        let detail = sel.as_ref().and_then(|id| a.module(id)).map(|m| self.render_module_detail(m, a, r, &prefix, p, theme));
        v_flex()
            .gap(px(12.))
            .child(toolbar)
            .child(ui::card(p).p(px(10.)).child(graph))
            .child(h_flex().gap(px(14.)).flex_wrap().children(notes))
            .when_some(detail, |d, panel| d.child(panel))
            .into_any_element()
    }

    fn render_module_detail(&self, m: &Module, a: &Arch, r: &Report, prefix: &str, p: &Palette, theme: &Theme) -> impl IntoElement {
        let mono = theme.mono_font_family.clone();
        let note = r.modules.iter().find(|n| same_file(&n.path, &m.id)).map(|n| n.note.clone());
        let entries: Vec<(&'static str, &Entry)> = [("новое", &r.code.added), ("изменено", &r.code.changed), ("удалено", &r.code.removed)]
            .into_iter()
            .flat_map(|(w, list)| list.iter().filter(|e| same_file(&e.file, &m.id)).map(move |e| (w, e)))
            .collect();
        let types: Vec<&super::Dto> = r.dtos.iter().filter(|d| same_file(&d.file, &m.id) && d.status != "context").collect();
        let risks: Vec<&super::Risk> = r.risks.iter().filter(|x| entries.iter().any(|(_, e)| e.risk_ids.contains(&x.id))).collect();
        let label_of = |id: &str| a.module(id).map(|x| x.label.clone()).unwrap_or_else(|| id.to_owned());
        let out: Vec<(String, Status)> = a.edges.iter().filter(|e| e.from == m.id).map(|e| (label_of(&e.to), e.status)).collect();
        let inc: Vec<(String, Status)> = a.edges.iter().filter(|e| e.to == m.id).map(|e| (label_of(&e.from), e.status)).collect();
        let head = |t: &'static str| div().mt(px(14.)).mb(px(4.)).font_family(mono.clone()).text_size(px(10.5)).text_color(p.faint).child(t);
        let link_row = |arrow: &'static str, (name, s): (String, Status)| {
            h_flex()
                .gap(px(6.))
                .py(px(2.))
                .text_size(px(12.5))
                .child(div().font_family(mono.clone()).text_color(p.faint).child(arrow))
                .child(div().min_w_0().truncate().text_color(p.fg).child(name))
                .when_some(tone(s, p).filter(|_| s != Status::Same), |d, t| d.child(ui::badge(if s == Status::Added { "новое" } else { "убрано" }, t).text_size(px(10.5))))
        };
        let column = |title: &'static str| v_flex().flex_1().min_w(px(200.)).gap(px(2.)).child(head(title));
        let inside = column("ЧТО ВНУТРИ").children(entries.iter().take(14).map(|(w, e)| {
            let sig = if e.signature.is_empty() { e.name.clone() } else { e.signature.clone() };
            v_flex()
                .py(px(4.))
                .border_t_1()
                .border_color(p.border)
                .child(
                    h_flex()
                        .gap(px(6.))
                        .child(ui::badge(*w, match *w { "новое" => p.green, "удалено" => p.red, _ => p.amber }).text_size(px(10.)))
                        .child(div().min_w_0().truncate().text_size(px(12.5)).font_weight(FontWeight::MEDIUM).text_color(p.ink).child(if e.entity.is_empty() { e.name.clone() } else { e.entity.clone() })),
                )
                .when(!e.signature_before.is_empty(), |d| d.child(div().font_family(mono.clone()).text_size(px(11.)).text_color(p.red.text).child(format!("− {}", e.signature_before))))
                .child(div().font_family(mono.clone()).text_size(px(11.)).text_color(if e.signature_before.is_empty() { p.muted } else { p.green.text }).child(if e.signature_before.is_empty() { sig } else { format!("+ {sig}") }))
        }))
        .when(!types.is_empty(), |d| d.child(div().mt(px(6.)).text_size(px(12.)).text_color(p.muted).child(format!("типы: {} — поля во вкладке «Контракты»", types.iter().map(|t| t.name.clone()).collect::<Vec<_>>().join(", ")))))
        .when(entries.is_empty() && types.is_empty(), |d| d.child(div().text_size(px(12.5)).text_color(p.faint).child(if m.external { "внешний пакет" } else { "ревьюер не разбирал этот модуль по сущностям" })));
        let imports = column("ИМПОРТИРУЕТ").children(out.into_iter().map(|x| link_row("→", x))).when(m.external, |d| d.child(div().text_size(px(12.5)).text_color(p.faint).child("—")));
        let dependents = column("КТО ИМПОРТИРУЕТ").children(inc.into_iter().map(|x| link_row("←", x)));
        let why_dep: Vec<String> = r.deps.iter().filter(|x| x.name == m.package || x.name.replace('-', "_") == m.package).map(|x| x.why.clone()).collect();
        let dep_lines: Vec<String> = a.deps.iter().filter(|x| x.name == m.package || x.name.replace('-', "_") == m.package).map(|x| format!("{} · {} → {}", x.manifest, x.before.as_deref().unwrap_or("—"), x.after.as_deref().unwrap_or("—"))).collect();
        let fourth = if m.external {
            column("ЗАВИСИМОСТЬ")
                .children(dep_lines.into_iter().map(|l| div().font_family(mono.clone()).text_size(px(11.)).text_color(p.fg).child(l)))
                .children(why_dep.into_iter().map(|w| div().mt(px(4.)).text_size(px(12.5)).text_color(p.fg).child(w)))
        } else {
            column("РИСКИ")
                .children(risks.into_iter().map(|x| {
                    h_flex()
                        .gap(px(6.))
                        .items_start()
                        .py(px(2.))
                        .child(ui::dot(match x.severity.as_str() { "high" => p.red.solid, "mid" => p.amber.solid, _ => p.border_strong }).mt(px(5.)))
                        .child(div().text_size(px(12.5)).text_color(p.fg).child(x.title.clone()))
                }))
                .when(entries.iter().all(|(_, e)| e.risk_ids.is_empty()), |d| d.child(div().text_size(px(12.5)).text_color(p.faint).child("рисков у модуля нет")))
        };
        v_flex()
            .p(px(16.))
            .gap(px(4.))
            .rounded(px(12.))
            .border_1()
            .border_color(p.border)
            .bg(p.surface2)
            .child(
                h_flex()
                    .gap(px(10.))
                    .items_baseline()
                    .flex_wrap()
                    .child(ui::badge(status_word(m.status), tone(m.status, p).unwrap_or(super::contracts::gray(p))))
                    .child(div().text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).child(m.label.clone()))
                    .child(div().font_family(mono.clone()).text_size(px(11.)).text_color(p.faint).child(if m.external { format!("пакет {}", m.package) } else { format!("{} · +{} −{} · {}", m.id, m.plus, m.minus, short_package(&m.package, prefix)) })),
            )
            .when_some(note, |d, n| d.child(div().text_size(px(13.5)).text_color(p.fg).child(n)))
            .child(h_flex().gap(px(22.)).items_start().flex_wrap().child(inside).child(imports).child(dependents).child(fourth))
    }
}

fn legend_line(color: Hsla, dashed: bool, label: &'static str, p: &Palette) -> impl IntoElement {
    h_flex()
        .gap(px(6.))
        .child(div().w(px(16.)).h(px(0.)).border_t_2().when(dashed, |d| d.border_dashed()).border_color(color))
        .child(div().text_size(px(12.)).text_color(p.muted).child(label))
}

fn pkg_note(text: &str, what: &'static str, t: Tone, p: &Palette) -> impl IntoElement {
    h_flex()
        .gap(px(6.))
        .child(ui::badge(text.to_owned(), t))
        .child(div().text_size(px(12.5)).text_color(p.muted).child(what))
}

pub fn empty_state(title: &'static str, hint: &'static str, p: &Palette) -> AnyElement {
    v_flex()
        .gap(px(4.))
        .p(px(16.))
        .rounded(px(12.))
        .border_1()
        .border_dashed()
        .border_color(p.border_strong)
        .child(div().text_size(px(13.5)).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).child(title))
        .child(div().text_size(px(12.5)).text_color(p.muted).child(hint))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn module(id: &str, pkg: &str, status: Status) -> Module {
        Module { id: id.into(), package: pkg.into(), label: id.rsplit('/').next().unwrap().into(), status, ..Default::default() }
    }
    fn edge(f: &str, t: &str, status: Status) -> stepan_verify_core::arch::Edge {
        stepan_verify_core::arch::Edge { from: f.into(), to: t.into(), status }
    }

    #[core::prelude::v1::test]
    fn lanes_follow_import_direction_and_new_comes_first() {
        let a = Arch {
            modules: vec![module("core/a.rs", "core", Status::Same), module("app/x.rs", "app", Status::Changed), module("app/y.rs", "app", Status::Added), module("pkg:sqlite", "sqlite", Status::Added)],
            edges: vec![edge("app/x.rs", "core/a.rs", Status::Same), edge("app/y.rs", "core/a.rs", Status::Added), edge("core/a.rs", "pkg:sqlite", Status::Added)],
            ..Default::default()
        };
        let a = Arch { modules: a.modules.into_iter().enumerate().map(|(i, mut m)| { m.external = i == 3; m }).collect(), ..a };
        let l = layout(&a);
        let titles: Vec<&str> = l.lanes.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(titles, ["app", "core", "пакеты"], "importers left, packages last");
        assert!(l.pos[2].1 < l.pos[1].1, "the new module sits above the changed one");
        assert!(l.paths[1][0].0 < l.paths[1][3].0, "a link to the right goes left to right");
    }

    #[core::prelude::v1::test]
    fn one_package_splits_into_folders() {
        let mut modules: Vec<Module> = (0..14).map(|i| module(&format!("src/lib/api/f{i}.ts"), "web", Status::Changed)).collect();
        modules.push(module("src/app/page.tsx", "web", Status::Added));
        let a = Arch { modules, ..Default::default() };
        let keys = lane_keys(&a);
        assert_eq!(keys[0], "lib/api");
        assert_eq!(keys[14], "app");
        let l = layout(&a);
        assert!(l.size.0 > 0. && l.size.1 > 0.);
    }

    #[core::prelude::v1::test]
    fn package_news_and_prefix() {
        let m = |id: &str, pkg: &str| Module { id: id.into(), package: pkg.into(), label: id.into(), ..Default::default() };
        let e = |f: &str, t: &str, s| stepan_verify_core::arch::Edge { from: f.into(), to: t.into(), status: s };
        let a = Arch {
            modules: vec![m("a", "stepan_app"), m("b", "stepan_core"), m("c", "stepan_core")],
            edges: vec![e("a", "b", Status::Added), e("b", "c", Status::Same)],
            ..Default::default()
        };
        let links = package_links(&a);
        assert_eq!(links.added, [("stepan_app".to_owned(), "stepan_core".to_owned())]);
        assert!(links.cycles.is_empty());
        assert_eq!(package_prefix(&a), "stepan_");
        assert!(same_file("crates/x/src/a.rs", "x/src/a.rs") && !same_file("a.rs", "ba.rs"));
    }
}
