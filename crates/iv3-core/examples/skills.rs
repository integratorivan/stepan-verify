//! Debug: `cargo run -p iv3-core --example skills` — scan skills and usage.
use std::collections::BTreeMap;
fn main() {
    let home = iv3_core::Home::locate();
    let t = std::time::Instant::now();
    let r = iv3_core::skills::scan(&home);
    println!("scan {:?}: {} skills, {} uses, {} problems", t.elapsed(), r.skills.len(), r.uses.len(), r.problems.len());
    let since = chrono::Utc::now() - chrono::Duration::days(14);
    let mut by: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for u in r.uses.iter().filter(|u| u.at > since) {
        let e = by.entry(u.skill.clone()).or_default();
        match u.agent { iv3_core::Provider::Claude => e.0 += 1, iv3_core::Provider::Codex => e.1 += 1 }
    }
    let mut v: Vec<_> = by.into_iter().collect();
    v.sort_by_key(|(_, (c, x))| std::cmp::Reverse(c + x));
    for (k, (c, x)) in v.iter().take(15) { println!("{k:30} claude={c} codex={x}"); }
    let vis = |f: fn(&iv3_core::skills::Skill) -> bool| r.skills.iter().filter(|s| f(s)).count();
    println!("claude sees {} codex sees {} plugin {}", vis(|s| s.claude), vis(|s| s.codex), vis(|s| s.plugin));
    for p in r.problems.iter().take(12) { println!("{:?} {} — {}", p.severity, p.skill, p.title); }
}
