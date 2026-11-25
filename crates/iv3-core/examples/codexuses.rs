fn main() {
    for a in std::env::args().skip(1) {
        let n = iv3_core::skills::codex_uses(std::path::Path::new(&a)).iter().filter(|u| u.skill == "unslop").count();
        if n != 1 { println!("{n} {a}"); }
    }
}
