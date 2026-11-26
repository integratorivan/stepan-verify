//! `cargo run -p stepan_verify_core --example arch -- <repo> <base>`: the computed structure of a change.
fn main() {
    let mut args = std::env::args().skip(1);
    let cwd = std::path::PathBuf::from(args.next().unwrap_or_else(|| ".".into()));
    let base = args.next().unwrap_or_else(|| "HEAD".into());
    let t = std::time::Instant::now();
    let noise = |p: &str| ["target/", "node_modules/", ".claude/", ".omc/", "dist/", ".next/"].iter().any(|d| p.starts_with(d) || p.contains(&format!("/{d}")));
    let Some(a) = stepan_verify_core::arch::compute(&cwd, &base, &noise) else { return eprintln!("not a git repo or bad base") };
    println!("base {} · {} modules · {} edges · {:?}", &a.base[..8], a.modules.len(), a.edges.len(), t.elapsed());
    for m in &a.modules {
        println!("  {:?} {:<10} {:<38} {} +{} -{}", m.status, m.package, m.label, m.id, m.plus, m.minus);
    }
    for e in a.edges.iter().filter(|e| e.status != stepan_verify_core::arch::Status::Same) {
        println!("  {:?} {} -> {}", e.status, e.from, e.to);
    }
    println!("  same edges: {}", a.edges.iter().filter(|e| e.status == stepan_verify_core::arch::Status::Same).count());
    for d in &a.deps {
        println!("  dep {} [{}] {} {:?} -> {:?}", d.manifest, d.section, d.name, d.before, d.after);
    }
    for l in &a.locks {
        println!("  lock {} +{:?} -{:?}", l.file, l.added, l.removed);
    }
}
