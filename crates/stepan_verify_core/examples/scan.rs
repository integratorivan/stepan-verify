use std::time::{Duration, Instant};

fn main() {
    let t = Instant::now();
    let all = stepan_verify_core::sessions::scan_all(Duration::from_secs(7 * 86400));
    for s in all.iter().take(15) {
        println!("{:6} {}  {:<20} {}", s.provider.label(), s.updated_at.format("%m-%d %H:%M"), s.project(), s.title);
    }
    println!("{} sessions in {:?}", all.len(), t.elapsed());
}
