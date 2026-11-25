//! Deterministic inventory of a project's user entry points — the coverage
//! checklist for the flow map. No agent involved: routing files, screens,
//! menus and tests are found by stack heuristics, so a map built by agents in
//! several passes can be checked for what it missed.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::flows::FlowMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    /// A URL route / page.
    Route,
    /// A mobile or desktop screen.
    Screen,
    /// A menu item / command.
    Menu,
    /// A user-facing API endpoint.
    Api,
}

impl EntryKind {
    pub fn label(self) -> &'static str {
        match self {
            EntryKind::Route => "маршрут",
            EntryKind::Screen => "экран",
            EntryKind::Menu => "меню",
            EntryKind::Api => "API",
        }
    }
}

/// Something a user can land on: `/rewards/hub` in `app/rewards/hub/page.tsx`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EntryPoint {
    pub kind: EntryKind,
    /// Route path, screen or menu name.
    pub id: String,
    /// Repo-relative file it lives in.
    pub file: String,
    /// First meaningful route segment / folder: a hint for grouping into areas.
    #[serde(default)]
    pub area: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Inventory {
    /// Detected stacks: next, expo-router, react-router, vue-router, sveltekit, swiftui, electron…
    pub stacks: Vec<String>,
    pub entries: Vec<EntryPoint>,
    /// Test files (repo-relative).
    pub tests: Vec<String>,
    /// Storybook stories.
    pub stories: Vec<String>,
}

const SKIP_DIRS: [&str; 18] = [
    "node_modules", ".git", "target", "dist", "build", ".next", ".expo", "Pods", ".stepan_verify", ".omc", ".claude",
    "vendor", "coverage", ".turbo", ".svelte-kit", "DerivedData", ".output", ".nuxt",
];
const MAX_FILES: usize = 40_000;
const CODE_EXT: [&str; 9] = ["tsx", "ts", "jsx", "js", "mjs", "vue", "svelte", "swift", "mdx"];

/// Walk the repo once and collect entry points, tests and stories.
pub fn scan(repo: &Path) -> Inventory {
    let files = walk(repo);
    let rel = |p: &Path| p.strip_prefix(repo).unwrap_or(p).to_string_lossy().replace('\\', "/");
    let mut inv = Inventory::default();
    let mut stacks = BTreeSet::new();
    let mut entries = BTreeSet::new();

    // Package roots and their dependencies decide which conventions apply.
    for pkg in files.iter().filter(|p| p.file_name().is_some_and(|n| n == "package.json")) {
        let Ok(text) = fs::read_to_string(pkg) else { continue };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
        let has = |dep: &str| ["dependencies", "devDependencies"].iter().any(|k| v.get(*k).and_then(|d| d.get(dep)).is_some());
        let root = pkg.parent().unwrap_or(repo);
        let under = |dirs: &[&str]| -> Vec<&PathBuf> {
            files.iter().filter(|f| dirs.iter().any(|d| f.starts_with(root.join(d)))).collect()
        };
        if has("next") {
            stacks.insert("next".to_owned());
            for f in under(&["app", "src/app"]) {
                let base = if f.starts_with(root.join("src/app")) { root.join("src/app") } else { root.join("app") };
                if let Some(e) = next_app_entry(&base, f) {
                    entries.insert(EntryPoint { file: rel(f), ..e });
                }
            }
            for f in under(&["pages", "src/pages"]) {
                let base = if f.starts_with(root.join("src/pages")) { root.join("src/pages") } else { root.join("pages") };
                if let Some(e) = next_pages_entry(&base, f) {
                    entries.insert(EntryPoint { file: rel(f), ..e });
                }
            }
        }
        if has("expo-router") {
            stacks.insert("expo-router".to_owned());
            for f in under(&["app", "src/app"]) {
                let base = if f.starts_with(root.join("src/app")) { root.join("src/app") } else { root.join("app") };
                if let Some(e) = expo_entry(&base, f) {
                    entries.insert(EntryPoint { file: rel(f), ..e });
                }
            }
        }
        if has("@sveltejs/kit") {
            stacks.insert("sveltekit".to_owned());
            for f in under(&["src/routes"]) {
                if f.file_name().is_some_and(|n| n == "+page.svelte") {
                    let route = route_from_dir(f.parent().unwrap_or(root).strip_prefix(root.join("src/routes")).unwrap_or(Path::new("")));
                    entries.insert(EntryPoint { kind: EntryKind::Route, area: area_of(&route), id: route, file: rel(f) });
                }
            }
        }
        for (dep, name) in [("react-router", "react-router"), ("react-router-dom", "react-router"), ("vue-router", "vue-router"), ("@react-navigation/native", "react-navigation"), ("electron", "electron")] {
            if has(dep) {
                stacks.insert(name.to_owned());
            }
        }
    }

    // Source-level conventions: route tables, navigators, SwiftUI views, menus.
    for f in &files {
        let ext = f.extension().and_then(|e| e.to_str()).unwrap_or_default();
        if !CODE_EXT.contains(&ext) {
            continue;
        }
        let Ok(text) = fs::read_to_string(f) else { continue };
        let file = rel(f);
        if text.contains("react-router") || text.contains("vue-router") || text.contains("createBrowserRouter") {
            for route in route_literals(&text) {
                entries.insert(EntryPoint { kind: EntryKind::Route, area: area_of(&route), id: route, file: file.clone() });
            }
        }
        for name in navigator_screens(&text) {
            entries.insert(EntryPoint { kind: EntryKind::Screen, area: area_of(&name), id: name, file: file.clone() });
        }
        if ext == "swift" {
            stacks.insert("swiftui".to_owned());
            for name in swiftui_screens(&text) {
                entries.insert(EntryPoint { kind: EntryKind::Screen, area: area_of(&name), id: name, file: file.clone() });
            }
        }
        if text.contains("Menu.buildFromTemplate") {
            for label in quoted_after(&text, "label:") {
                entries.insert(EntryPoint { kind: EntryKind::Menu, area: "menu".into(), id: label, file: file.clone() });
            }
        }
    }
    if !files.iter().any(|f| f.extension().is_some_and(|e| e == "swift")) {
        stacks.remove("swiftui");
    }

    for f in &files {
        let r = rel(f);
        let name = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if is_test(&r, &name) {
            inv.tests.push(r.clone());
        } else if name.contains(".stories.") {
            inv.stories.push(r);
        }
    }
    inv.stacks = stacks.into_iter().collect();
    inv.entries = entries.into_iter().collect();
    inv.tests.sort();
    inv.stories.sort();
    inv
}

fn walk(repo: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![repo.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let path = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                if !SKIP_DIRS.contains(&name.as_str()) && !(name.starts_with('.') && name != ".github") {
                    stack.push(path);
                }
            } else if ft.is_file() {
                out.push(path);
                if out.len() >= MAX_FILES {
                    return out;
                }
            }
        }
    }
    out.sort();
    out
}

fn is_test(rel: &str, name: &str) -> bool {
    name.contains(".test.")
        || name.contains(".spec.")
        || name.ends_with("_test.go")
        || (name.starts_with("test_") && name.ends_with(".py"))
        || name.ends_with("Tests.swift")
        || rel.split('/').any(|s| s == "__tests__" || s == "e2e")
}

/// `app/(shop)/rewards/[id]/page.tsx` → `/rewards/[id]`; route handlers → API.
fn next_app_entry(base: &Path, f: &Path) -> Option<EntryPoint> {
    let stem = f.file_stem()?.to_str()?;
    let ext = f.extension()?.to_str()?;
    if !["tsx", "ts", "jsx", "js", "mdx"].contains(&ext) {
        return None;
    }
    let kind = match stem {
        "page" => EntryKind::Route,
        "route" => EntryKind::Api,
        _ => return None,
    };
    let route = route_from_dir(f.parent()?.strip_prefix(base).ok()?);
    Some(EntryPoint { kind, area: area_of(&route), id: route, file: String::new() })
}

/// `pages/rewards/index.tsx` → `/rewards`; `pages/api/*` → API; `_app`, `_document` skipped.
fn next_pages_entry(base: &Path, f: &Path) -> Option<EntryPoint> {
    let ext = f.extension()?.to_str()?;
    if !["tsx", "ts", "jsx", "js", "mdx"].contains(&ext) || f.to_string_lossy().contains(".test.") {
        return None;
    }
    let rel = f.strip_prefix(base).ok()?.with_extension("");
    let rel = rel.to_string_lossy().replace('\\', "/");
    if rel.split('/').any(|s| s.starts_with('_')) {
        return None;
    }
    let kind = if rel.starts_with("api/") || rel == "api" { EntryKind::Api } else { EntryKind::Route };
    let route = format!("/{}", rel.trim_end_matches("index").trim_end_matches('/'));
    let route = if route == "/" { route } else { route.trim_end_matches('/').to_owned() };
    Some(EntryPoint { kind, area: area_of(&route), id: route, file: String::new() })
}

/// Expo Router: every file in `app/` that is not a layout or special file is a screen.
fn expo_entry(base: &Path, f: &Path) -> Option<EntryPoint> {
    let ext = f.extension()?.to_str()?;
    if !["tsx", "jsx", "ts", "js"].contains(&ext) {
        return None;
    }
    let stem = f.file_stem()?.to_str()?;
    if stem.starts_with('_') || stem.starts_with('+') || stem.contains(".test") {
        return None;
    }
    let rel = f.strip_prefix(base).ok()?.with_extension("");
    let route = route_from_dir(&rel);
    let route = route.strip_suffix("/index").map(str::to_owned).unwrap_or(route);
    let route = if route.is_empty() || route == "/index" { "/".to_owned() } else { route };
    Some(EntryPoint { kind: EntryKind::Screen, area: area_of(&route), id: route, file: String::new() })
}

/// Folder path → route: drops `(groups)` and `@slots`.
fn route_from_dir(rel: &Path) -> String {
    let parts: Vec<String> = rel
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .filter(|s| !(s.starts_with('(') && s.ends_with(')')) && !s.starts_with('@') && !s.is_empty())
        .map(str::to_owned)
        .collect();
    format!("/{}", parts.join("/"))
}

/// First meaningful segment: `/rewards/[id]` → `rewards`, `ProfileScreen` → `profile`.
pub fn area_of(id: &str) -> String {
    let first = id
        .trim_start_matches('/')
        .split('/')
        .find(|s| !s.is_empty() && !s.starts_with('[') && !s.starts_with(':'))
        .unwrap_or("");
    if first.is_empty() {
        return if id.starts_with('/') { "home".into() } else { String::new() };
    }
    let lower = first.to_lowercase();
    let lower = ["screen", "page", "view"].iter().fold(lower, |s, suf| s.strip_suffix(suf).map(str::to_owned).unwrap_or(s));
    lower
}

/// `path: "/x"` / `path="/x"` literals in a router file.
fn route_literals(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for key in ["path:", "path=", "path :"] {
        for s in quoted_after(text, key) {
            if s.starts_with('/') || s.chars().all(|c| c.is_alphanumeric() || "-_:/*".contains(c)) && !s.is_empty() {
                let r = if s.starts_with('/') { s } else { format!("/{s}") };
                if r != "/*" {
                    out.push(r);
                }
            }
        }
    }
    out
}

/// React Navigation: `<Stack.Screen name="Profile"` / `<Tab.Screen name='Feed'`.
fn navigator_screens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find(".Screen") {
        let after = &rest[i + ".Screen".len()..];
        let tag_end = after.find('>').unwrap_or(after.len()).min(400);
        if let Some(name) = quoted_after(&after[..tag_end], "name=").into_iter().next() {
            out.push(name);
        }
        rest = &rest[i + 1..];
    }
    out
}

/// SwiftUI: `struct ProfileScreen: View` (names ending in Screen / Page / Scene).
fn swiftui_screens(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| {
            let l = l.trim();
            let rest = l.strip_prefix("struct ").or_else(|| l.strip_prefix("public struct "))?;
            let (name, tail) = rest.split_once(':')?;
            let name = name.trim();
            (tail.contains("View") && ["Screen", "Page", "Scene"].iter().any(|s| name.ends_with(s))).then(|| name.to_owned())
        })
        .collect()
}

/// Quoted string literals right after `key` (`key "x"`, `key='x'`, `key: \`x\``).
fn quoted_after(text: &str, key: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find(key) {
        let after = rest[i + key.len()..].trim_start();
        let after = after.strip_prefix('{').unwrap_or(after).trim_start();
        if let Some(q) = after.chars().next().filter(|c| ['"', '\'', '`'].contains(c)) {
            if let Some(end) = after[1..].find(q) {
                let s = &after[1..1 + end];
                if !s.contains('\n') && s.len() < 120 {
                    out.push(s.to_owned());
                }
            }
        }
        rest = &rest[i + key.len()..];
    }
    out
}

/// Entry points the map does not account for: no feature lists them in
/// `entries`, no step goes through their file, and they are not ignored.
pub fn uncovered<'a>(map: &FlowMap, inv: &'a Inventory) -> Vec<&'a EntryPoint> {
    let mut known: BTreeSet<String> = BTreeSet::new();
    let mut files: BTreeSet<String> = BTreeSet::new();
    for f in &map.features {
        known.extend(f.entries.iter().map(|e| norm(e)));
        files.extend(f.steps().flat_map(|s| s.files.iter().map(|x| norm(x))));
    }
    known.extend(map.ignored.iter().map(|i| norm(&i.entry)));
    inv.entries
        .iter()
        .filter(|e| {
            let id = norm(&e.id);
            let file = norm(&e.file);
            !(known.contains(&id) || known.contains(&file) || files.contains(&file) || files.iter().any(|f| !file.is_empty() && (f.ends_with(&file) || file.ends_with(f.as_str()))))
        })
        .collect()
}

fn norm(s: &str) -> String {
    s.trim().trim_start_matches("./").to_owned()
}

/// Entry points grouped by area hint (for prompts and the coverage list).
pub fn by_area(entries: &[&EntryPoint]) -> BTreeMap<String, Vec<String>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for e in entries {
        out.entry(if e.area.is_empty() { "—".into() } else { e.area.clone() }).or_default().push(format!("{} {} ({})", e.kind.label(), e.id, e.file));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flows::{Feature, FlowPath, Ignored, Step};

    fn write(root: &Path, rel: &str, text: &str) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("sv-scan-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn next_app_and_pages_router() {
        let d = tmp("next");
        write(&d, "apps/web/package.json", r#"{"dependencies":{"next":"15"}}"#);
        write(&d, "apps/web/src/app/page.tsx", "x");
        write(&d, "apps/web/src/app/(shop)/rewards/[id]/page.tsx", "x");
        write(&d, "apps/web/src/app/rewards/layout.tsx", "x");
        write(&d, "apps/web/src/app/api/bonus/route.ts", "x");
        write(&d, "apps/web/pages/settings/index.tsx", "x");
        write(&d, "apps/web/pages/_app.tsx", "x");
        write(&d, "apps/web/src/app/rewards/Hub.test.tsx", "x");
        write(&d, "apps/web/node_modules/next/app/page.tsx", "x");
        let inv = scan(&d);
        let ids: Vec<(EntryKind, &str)> = inv.entries.iter().map(|e| (e.kind, e.id.as_str())).collect();
        assert!(ids.contains(&(EntryKind::Route, "/")));
        assert!(ids.contains(&(EntryKind::Route, "/rewards/[id]")));
        assert!(ids.contains(&(EntryKind::Api, "/api/bonus")));
        assert!(ids.contains(&(EntryKind::Route, "/settings")));
        assert_eq!(inv.entries.len(), 4, "{ids:?}");
        assert_eq!(inv.entries.iter().find(|e| e.id == "/rewards/[id]").unwrap().area, "rewards");
        assert_eq!(inv.tests, vec!["apps/web/src/app/rewards/Hub.test.tsx".to_owned()]);
        assert!(inv.stacks.contains(&"next".to_owned()));
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn react_router_and_expo_and_navigation() {
        let d = tmp("rr");
        write(&d, "package.json", r#"{"dependencies":{"react-router-dom":"6","expo-router":"3"}}"#);
        write(
            &d,
            "src/routes.tsx",
            "import { createBrowserRouter } from 'react-router-dom';\nexport const r = createBrowserRouter([{ path: '/', element: <Home/> }, { path: \"/profile/:id\", element: <P/> }, { path: '*', element: <NotFound/> }]);\n<Route path=\"/login\" element={<Login/>} />",
        );
        write(&d, "app/_layout.tsx", "x");
        write(&d, "app/index.tsx", "x");
        write(&d, "app/(tabs)/feed.tsx", "x");
        write(&d, "app/settings/index.tsx", "x");
        write(&d, "src/Nav.tsx", "<Stack.Screen name=\"Checkout\" component={C} />\n<Tab.Screen\n  name='Wallet' />");
        let inv = scan(&d);
        let ids: BTreeSet<&str> = inv.entries.iter().map(|e| e.id.as_str()).collect();
        for want in ["/", "/profile/:id", "/login", "/feed", "/settings", "Checkout", "Wallet"] {
            assert!(ids.contains(want), "missing {want}: {ids:?}");
        }
        assert!(!ids.contains("*") && !ids.contains("/*") && !ids.contains("/_layout"));
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn swiftui_and_area_hints() {
        let d = tmp("swift");
        write(&d, "App/ProfileScreen.swift", "struct ProfileScreen: View {\n}\nstruct Row: View {}\n");
        let inv = scan(&d);
        assert_eq!(inv.entries.len(), 1);
        assert_eq!(inv.entries[0].id, "ProfileScreen");
        assert_eq!(inv.entries[0].area, "profile");
        assert_eq!(area_of("/"), "home");
        assert_eq!(area_of("/[locale]/shop/cart"), "shop");
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn coverage_by_entries_files_and_ignored() {
        let inv = Inventory {
            entries: vec![
                EntryPoint { kind: EntryKind::Route, id: "/a".into(), file: "app/a/page.tsx".into(), area: "a".into() },
                EntryPoint { kind: EntryKind::Route, id: "/b".into(), file: "app/b/page.tsx".into(), area: "b".into() },
                EntryPoint { kind: EntryKind::Route, id: "/c".into(), file: "app/c/page.tsx".into(), area: "c".into() },
                EntryPoint { kind: EntryKind::Route, id: "/d".into(), file: "app/d/page.tsx".into(), area: "d".into() },
            ],
            ..Default::default()
        };
        let map = FlowMap {
            features: vec![
                Feature { id: "fa".into(), entries: vec!["/a".into()], ..Default::default() },
                Feature {
                    id: "fb".into(),
                    paths: vec![FlowPath { steps: vec![Step { files: vec!["./app/b/page.tsx".into()], ..Default::default() }], ..Default::default() }],
                    ..Default::default()
                },
            ],
            ignored: vec![Ignored { entry: "/c".into(), reason: "debug".into() }],
            ..Default::default()
        };
        let left: Vec<&str> = uncovered(&map, &inv).into_iter().map(|e| e.id.as_str()).collect();
        assert_eq!(left, vec!["/d"]);
    }
}
