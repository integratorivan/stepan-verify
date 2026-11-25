//! Time the indexed session scan: `cargo run --release -p stepan_verify_core --example index -- <db>`.
use std::time::{Duration, Instant};

fn main() {
    let db = std::env::args().nth(1).expect("path to a db");
    let store = stepan_verify_core::Store::open_at(db.as_ref()).unwrap();
    let age = Duration::from_secs(14 * 86_400);
    for pass in 1..=3 {
        let t = Instant::now();
        let n = stepan_verify_core::sessions::scan_indexed(&store, age).len();
        println!("scan {pass}: {n} sessions in {:?}", t.elapsed());
    }
    let t = Instant::now();
    let n = stepan_verify_core::sessions::cached(&store, age).len();
    println!("cached: {n} sessions in {:?}", t.elapsed());
    let t = Instant::now();
    let old = stepan_verify_core::sessions::scan_all(age);
    println!("old full scan: {} sessions in {:?}", old.len(), t.elapsed());
    let key = |s: &stepan_verify_core::SessionSummary| (s.id.clone(), s.title.clone(), s.cwd.clone(), s.file.clone());
    let mut a: Vec<_> = old.iter().map(key).collect();
    let mut b: Vec<_> = stepan_verify_core::sessions::cached(&store, age).iter().map(key).collect();
    a.sort();
    b.sort();
    let diff: Vec<_> = a.iter().filter(|x| !b.contains(x)).collect();
    println!("differ from the old scan: {}", diff.len());
    for d in diff.iter().take(5) {
        println!("  {d:?}");
    }
}
