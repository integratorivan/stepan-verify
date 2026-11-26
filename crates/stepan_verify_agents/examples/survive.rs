//! Manual check that an agent outlives its window (needs `claude` and a built
//! `stepan_verify` as `STEPAN_VERIFY_HOST_EXE`):
//!
//!   survive start <cwd>              start a session, quit mid-step, print its id
//!   survive attach <cwd> <id> <file> come back and wait for the turn to end
//!
//! `SURVIVE_PROVIDER=codex` runs Codex instead (the file argument is unused).

use std::{path::PathBuf, time::Duration};

use stepan_verify_agents::{AgentEvent, Mode, Provider, SessionSpec};

fn spec(cwd: &str, id: Option<String>, transcript: Option<PathBuf>) -> SessionSpec {
    let codex = std::env::var("SURVIVE_PROVIDER").as_deref() == Ok("codex");
    SessionSpec {
        provider: if codex { Provider::Codex } else { Provider::Claude },
        native_id: id,
        transcript,
        cwd: PathBuf::from(cwd),
        mode: Mode::FullAccess,
        image_cache: None,
        env: Vec::new(),
        model: (!codex).then(|| "haiku".into()),
        instructions: None,
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("start") => {
            let (handle, rx) = stepan_verify_agents::open(spec(&args[1], None, None));
            handle.send("Run exactly this bash command and nothing else: `sleep 25 && echo stepan-survived > survived.txt`. Then answer with one word: готово.");
            let mut id = None;
            while let Ok(event) = rx.recv_blocking() {
                match event {
                    AgentEvent::SessionId(s) => id = Some(s),
                    AgentEvent::Upsert(item) if item.title.contains("sleep") => {
                        println!("id {}", id.clone().unwrap_or_default());
                        println!("step started: {}; the window quits now", item.title);
                        // As abrupt as an update relaunch: no clean shutdown.
                        std::process::exit(0);
                    }
                    AgentEvent::Error(e) => {
                        eprintln!("error: {e}");
                        std::process::exit(1);
                    }
                    _ => {}
                }
            }
        }
        Some("attach") => {
            let (handle, rx) = stepan_verify_agents::open(spec(&args[1], Some(args[2].clone()), Some(PathBuf::from(&args[3]))));
            let deadline = std::time::Instant::now() + Duration::from_secs(120);
            while std::time::Instant::now() < deadline {
                let Ok(event) = rx.recv_blocking() else { break };
                match event {
                    AgentEvent::TurnStarted => println!("attached mid-turn"),
                    AgentEvent::Interrupted { step } => println!("INTERRUPTED at {step}"),
                    AgentEvent::Upsert(item) if item.kind == stepan_verify_agents::ItemKind::Assistant && !item.body.is_empty() => {
                        println!("assistant: {}", item.body.chars().take(80).collect::<String>())
                    }
                    AgentEvent::TurnCompleted { error } => {
                        println!("turn completed, error: {error:?}");
                        break;
                    }
                    AgentEvent::Error(e) => println!("error: {e}"),
                    _ => {}
                }
            }
            drop(handle);
        }
        _ => eprintln!("survive start <cwd> | survive attach <cwd> <id> <transcript>"),
    }
}
