//! What an agent session gets at start: `cargo run -p stepan_verify_core --example brief -- <project> [claude|codex]`
fn main() {
    let mut args = std::env::args().skip(1);
    let project = args.next().filter(|p| !p.is_empty());
    let author = args.next().unwrap_or_else(|| "claude".into());
    print!("{}", stepan_verify_core::docs::agent_brief(&stepan_verify_core::Home::locate(), project.as_deref(), &author, None));
}
