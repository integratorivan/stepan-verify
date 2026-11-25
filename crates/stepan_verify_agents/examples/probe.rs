//! cargo run -p stepan_verify_agents --example probe -- <claude|codex> <cwd> [native-id] [transcript] [message]
use stepan_verify_agents::{AgentEvent, Mode, Provider, SessionSpec};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let provider = if a[0] == "codex" { Provider::Codex } else { Provider::Claude };
    let opt = |i: usize| a.get(i).filter(|s| !s.is_empty()).cloned();
    let spec = SessionSpec {
        provider,
        cwd: a[1].clone().into(),
        native_id: opt(2),
        transcript: opt(3).map(Into::into),
        mode: if std::env::var("PROBE_FULL").is_ok() { Mode::FullAccess } else { Mode::AutoEdits },
        image_cache: Some(std::env::temp_dir().join("stepan_verify-probe-images")),
        env: Vec::new(),
        model: std::env::var("PROBE_MODEL").ok(),
        // PROBE_INSTRUCTIONS=… goes to the system prompt / developer instructions.
        instructions: std::env::var("PROBE_INSTRUCTIONS").ok(),
    };
    let message = opt(4);
    let (handle, rx) = stepan_verify_agents::open(spec);
    if let Some(m) = &message {
        // STEPAN_VERIFY_IMAGES=/a.png:/b.jpg attaches images to the message.
        let images = std::env::var("STEPAN_VERIFY_IMAGES").map(|v| v.split(':').map(Into::into).collect()).unwrap_or_default();
        handle.send_message(stepan_verify_agents::Outgoing { text: m.clone(), images });
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    while std::time::Instant::now() < deadline {
        let Ok(ev) = rx.recv_blocking() else { break };
        match &ev {
            AgentEvent::History(items) => {
                println!("HISTORY {} items, {} with diffs, {} with attachments ({} total)", items.len(),
                    items.iter().filter(|i| !i.diffs.is_empty()).count(),
                    items.iter().filter(|i| !i.attachments.is_empty()).count(),
                    items.iter().map(|i| i.attachments.len()).sum::<usize>());
                for i in items.iter().filter(|i| !i.attachments.is_empty()).take(4) {
                    println!("  {:?} {} -> {:?}", i.kind, i.title, i.attachments.iter().map(|a| (a.kind, a.label())).collect::<Vec<_>>());
                }
                if let Some(d) = items.iter().find(|i| !i.diffs.is_empty()) {
                    println!("  first edit: {} {:?}\n{}", d.title, d.diffs[0].stats(), d.diffs[0].patch.lines().take(8).collect::<Vec<_>>().join("\n"));
                }
                for it in items.iter().take(6) {
                    println!("  {:?} {:?} {} | {}", it.kind, it.status, it.title, it.body.chars().take(80).collect::<String>().replace('\n', " "));
                }
                if message.is_none() { break; }
            }
            AgentEvent::Append { text, .. } => print!("{text}"),
            AgentEvent::ApprovalRequested(ap) => {
                println!("\nAPPROVAL {} — {}", ap.title, ap.detail);
                handle.approve(ap.id.clone(), true);
            }
            AgentEvent::TurnCompleted { error } => { println!("\nTURN DONE {error:?}"); break; }
            AgentEvent::Upsert(it) => println!("\nUPSERT {:?} {:?} {} | {}", it.kind, it.status, it.title, it.body.chars().take(80).collect::<String>().replace('\n', " ")),
            other => println!("\n{other:?}"),
        }
    }
}
