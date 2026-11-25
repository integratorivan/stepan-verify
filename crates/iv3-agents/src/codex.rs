//! Codex over `codex app-server`: newline-delimited JSON-RPC without the
//! `jsonrpc` field. Requests from the server (approvals) carry both `id` and
//! `method` and are answered with `{id, result}`.

use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Write},
    process::{ChildStdin, Command as Proc, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::Duration,
};

use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};

use crate::{
    Command, Mode, SessionSpec,
    claude::truncate,
    model::{AgentEvent, Approval, Attachment, Emitter, FileDiff, ImageStore, Item, ItemKind, ItemStatus},
};

const MAX_HISTORY_PAGES: usize = 50;

type Pending = Arc<Mutex<HashMap<u64, mpsc::Sender<Result<Value, String>>>>>;

struct Rpc {
    stdin: Mutex<ChildStdin>,
    next_id: AtomicU64,
    pending: Pending,
}

impl Rpc {
    fn write(&self, v: &Value) -> Result<()> {
        let mut line = serde_json::to_string(v)?;
        line.push('\n');
        let mut stdin = self.stdin.lock().unwrap();
        stdin.write_all(line.as_bytes()).context("codex stdin закрыт")?;
        stdin.flush()?;
        Ok(())
    }

    fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        self.pending.lock().unwrap().insert(id, tx);
        self.write(&json!({"id": id, "method": method, "params": params}))?;
        rx.recv_timeout(Duration::from_secs(120))
            .map_err(|_| anyhow!("{method}: нет ответа от codex"))?
            .map_err(|e| anyhow!("{method}: {e}"))
    }

    fn notify(&self, method: &str) -> Result<()> {
        self.write(&json!({"method": method}))
    }
}

#[derive(Default)]
struct Turn {
    id: Option<String>,
    /// approval id we expose → JSON-RPC id of the server request.
    approvals: HashMap<String, Value>,
}

pub(crate) fn run(spec: SessionSpec, rx: mpsc::Receiver<Command>, events: &Emitter) -> Result<()> {
    let store = ImageStore { dir: spec.image_cache.clone() };
    let mut cmd = Proc::new("codex");
    cmd.args(["app-server", "-c", "thread_unload_delay_secs=0"])
        .current_dir(&spec.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    crate::scrub_env(&mut cmd);
    crate::docs_env(&mut cmd, &spec);
    cmd.envs(spec.env.iter().map(|(k, v)| (k, v)));
    let mut child = cmd
        .spawn()
        .context("не удалось запустить `codex app-server`")?;
    let stdout = child.stdout.take().unwrap();
    let rpc = Arc::new(Rpc {
        stdin: Mutex::new(child.stdin.take().unwrap()),
        next_id: AtomicU64::new(1),
        pending: Arc::default(),
    });
    let turn = Arc::new(Mutex::new(Turn::default()));

    {
        let rpc = rpc.clone();
        let turn = turn.clone();
        let events = Emitter(events.0.clone());
        let store = store.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
                dispatch(v, &rpc, &turn, &store, &events);
            }
            // Process gone: fail everything still waiting.
            for (_, tx) in rpc.pending.lock().unwrap().drain() {
                let _ = tx.send(Err("codex завершился".into()));
            }
        });
    }

    let result = session(&spec, &rpc, &turn, &store, rx, events);
    let _ = child.kill();
    let _ = child.wait();
    result
}

fn session(
    spec: &SessionSpec,
    rpc: &Rpc,
    turn: &Mutex<Turn>,
    store: &ImageStore,
    rx: mpsc::Receiver<Command>,
    events: &Emitter,
) -> Result<()> {
    rpc.request(
        "initialize",
        json!({"clientInfo": {"name": "iv3", "title": "iv3", "version": env!("CARGO_PKG_VERSION")},
               "capabilities": {"experimentalApi": true}}),
    )?;
    rpc.notify("initialized")?;

    let thread_id = match &spec.native_id {
        Some(id) => {
            rpc.request("thread/resume", json!({"threadId": id, "excludeTurns": true, "developerInstructions": spec.instructions}))?;
            events.emit(AgentEvent::History(load_history(rpc, id, store)?));
            id.clone()
        }
        None => {
            let started =
                rpc.request("thread/start", json!({"cwd": spec.cwd, "model": spec.model, "developerInstructions": spec.instructions}))?;
            let id = started.pointer("/thread/id").and_then(Value::as_str).context("thread/start без id")?;
            events.emit(AgentEvent::History(Vec::new()));
            id.to_owned()
        }
    };
    events.emit(AgentEvent::SessionId(thread_id.clone()));

    let (approval, sandbox) = match spec.mode {
        Mode::Ask => (json!("untrusted"), json!({"type": "readOnly", "networkAccess": false})),
        Mode::AutoEdits => (
            json!("on-request"),
            json!({"type": "workspaceWrite", "writableRoots": crate::board_home(spec).into_iter().chain([crate::docs_dir()]).collect::<Vec<_>>(), "networkAccess": false,
                   "excludeTmpdirEnvVar": false, "excludeSlashTmp": false}),
        ),
        Mode::FullAccess => (json!("never"), json!({"type": "dangerFullAccess"})),
    };

    while let Ok(cmd) = rx.recv() {
        match cmd {
            Command::Send(message) => {
                let mut input = Vec::new();
                if !message.text.is_empty() {
                    input.push(json!({"type": "text", "text": message.text, "text_elements": []}));
                }
                input.extend(message.images.iter().map(|p| json!({"type": "localImage", "path": p})));
                let params = json!({
                    "threadId": thread_id,
                    "input": input,
                    "cwd": spec.cwd,
                    "approvalPolicy": approval,
                    "sandboxPolicy": sandbox,
                });
                // turn/start answers right away; progress arrives as notifications.
                if let Err(err) = rpc.request("turn/start", params) {
                    events.emit(AgentEvent::TurnCompleted { error: Some(format!("{err:#}")) });
                }
            }
            Command::Interrupt => {
                let turn_id = turn.lock().unwrap().id.clone();
                if let Some(turn_id) = turn_id {
                    rpc.request("turn/interrupt", json!({"threadId": thread_id, "turnId": turn_id}))?;
                }
            }
            Command::Approve { id, allow } => {
                let rpc_id = turn.lock().unwrap().approvals.remove(&id);
                if let Some(rpc_id) = rpc_id {
                    let decision = if allow { "accept" } else { "decline" };
                    rpc.write(&json!({"id": rpc_id, "result": {"decision": decision}}))?;
                    events.emit(AgentEvent::ApprovalResolved(id));
                }
            }
            Command::Shutdown => break,
        }
    }
    Ok(())
}

fn load_history(rpc: &Rpc, thread_id: &str, store: &ImageStore) -> Result<Vec<Item>> {
    let mut items = Vec::new();
    let mut cursor = Value::Null;
    for _ in 0..MAX_HISTORY_PAGES {
        let page = rpc.request("thread/items/list", json!({"threadId": thread_id, "limit": 200, "cursor": cursor}))?;
        for entry in page.get("data").and_then(Value::as_array).into_iter().flatten() {
            if let Some(item) = entry.get("item").and_then(|i| map_item(i, store)) {
                items.push(item);
            }
        }
        cursor = page.get("nextCursor").cloned().unwrap_or(Value::Null);
        if cursor.is_null() {
            return Ok(items);
        }
    }
    items.push(Item::message("history-cut", ItemKind::Notice, "История обрезана: показаны первые 10 000 записей."));
    Ok(items)
}

fn dispatch(v: Value, rpc: &Rpc, turn: &Mutex<Turn>, store: &ImageStore, events: &Emitter) {
    let method = v.get("method").and_then(Value::as_str).map(str::to_owned);
    let params = v.get("params").cloned().unwrap_or_default();
    let p = |k: &str| params.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();

    match (v.get("id"), method.as_deref()) {
        // Response to one of our requests.
        (Some(id), None) => {
            let Some(id) = id.as_u64() else { return };
            if let Some(tx) = rpc.pending.lock().unwrap().remove(&id) {
                let result = match v.get("error") {
                    Some(err) => Err(err.get("message").and_then(Value::as_str).unwrap_or("ошибка").to_owned()),
                    None => Ok(v.get("result").cloned().unwrap_or_default()),
                };
                let _ = tx.send(result);
            }
        }
        // Request from the server: approvals.
        (Some(rpc_id), Some(method)) => {
            let (title, detail) = match method {
                "item/commandExecution/requestApproval" | "execCommandApproval" => {
                    (format!("$ {}", strip_shell(&p("command"))), p("reason"))
                }
                "item/fileChange/requestApproval" | "applyPatchApproval" => ("Изменить файлы".to_owned(), p("reason")),
                "item/permissions/requestApproval" => ("Расширить права".to_owned(), p("reason")),
                _ => {
                    // Unsupported request: reject it so the turn does not hang.
                    let _ = rpc.write(&json!({"id": rpc_id, "error": {"code": -32601, "message": "not supported by iv3"}}));
                    return;
                }
            };
            let id = format!("codex-{rpc_id}");
            turn.lock().unwrap().approvals.insert(id.clone(), rpc_id.clone());
            events.emit(AgentEvent::ApprovalRequested(Approval { id, title, detail }));
        }
        // Notification.
        (None, Some(method)) => match method {
            "turn/started" => {
                turn.lock().unwrap().id = params.pointer("/turn/id").and_then(Value::as_str).map(str::to_owned);
                events.emit(AgentEvent::TurnStarted);
            }
            "turn/completed" => {
                turn.lock().unwrap().id = None;
                let error = params.pointer("/turn/error/message").and_then(Value::as_str).map(str::to_owned);
                events.emit(AgentEvent::TurnCompleted { error });
            }
            "item/started" | "item/completed" => {
                if let Some(item) = params.get("item").and_then(|i| map_item(i, store)) {
                    events.emit(AgentEvent::Upsert(item));
                }
            }
            "turn/plan/updated" => {
                let steps: Vec<String> = params
                    .get("plan")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .map(|t| {
                        let status = t.get("status").and_then(Value::as_str).unwrap_or_default();
                        let step = t.get("step").and_then(Value::as_str).unwrap_or_default();
                        format!("{} {step}", crate::claude::plan_mark(status))
                    })
                    .collect();
                if !steps.is_empty() {
                    let id = format!("plan:{}", p("turnId"));
                    events.emit(AgentEvent::Upsert(Item::message(id, ItemKind::Plan, steps.join("\n"))));
                }
            }
            "item/agentMessage/delta" | "item/commandExecution/outputDelta" | "item/reasoning/summaryTextDelta" => {
                events.emit(AgentEvent::Append { id: p("itemId"), text: p("delta") });
            }
            "error" => {
                let msg = params.pointer("/error/message").and_then(Value::as_str).unwrap_or("ошибка codex");
                events.emit(AgentEvent::Error(msg.to_owned()));
            }
            _ => {}
        },
        _ => {}
    }
}

fn short_path(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// `/bin/zsh -lc 'cmd'` → `cmd`.
fn strip_shell(cmd: &str) -> String {
    for prefix in ["/bin/zsh -lc ", "/bin/bash -lc ", "bash -lc ", "zsh -lc "] {
        if let Some(rest) = cmd.strip_prefix(prefix) {
            return rest.trim_matches('\'').trim_matches('"').to_owned();
        }
    }
    cmd.to_owned()
}

/// Codex ThreadItem → transcript item. Unknown kinds are dropped.
fn map_item(item: &Value, store: &ImageStore) -> Option<Item> {
    let s = |k: &str| item.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();
    let id = s("id");
    let status = match item.get("status").and_then(Value::as_str) {
        Some("inProgress") | Some("pending") => ItemStatus::Running,
        Some("failed") | Some("declined") => ItemStatus::Failed,
        _ => ItemStatus::Done,
    };
    let it = match item.get("type")?.as_str()? {
        "userMessage" => {
            let mut text = Vec::new();
            let mut attachments = Vec::new();
            for c in item.get("content")?.as_array()? {
                let field = |k: &str| c.get(k).and_then(Value::as_str);
                match field("type") {
                    Some("text") => text.extend(field("text")),
                    Some("image") => attachments.extend(field("url").and_then(|u| store.store_data_url(u))),
                    Some("localImage") => attachments.extend(field("path").map(Attachment::for_path)),
                    Some("mention") => attachments.extend(field("path").map(Attachment::for_path)),
                    Some("skill") => text.extend(field("name").map(|_| "")),
                    _ => {}
                }
            }
            let text = text.join("\n");
            if (text.trim().is_empty() || text.starts_with('<')) && attachments.is_empty() {
                return None;
            }
            let text = if text.starts_with('<') { String::new() } else { text };
            Item::message(id, ItemKind::User, text).with_attachments(attachments)
        }
        "agentMessage" => Item::message(id, ItemKind::Assistant, s("text")),
        "reasoning" => {
            let summary: Vec<&str> =
                item.get("summary")?.as_array()?.iter().filter_map(Value::as_str).collect();
            if summary.is_empty() {
                return None;
            }
            Item::message(id, ItemKind::Reasoning, summary.join("\n\n"))
        }
        "plan" => Item::message(id, ItemKind::Notice, s("text")),
        "commandExecution" => Item::tool(
            id,
            format!("$ {}", strip_shell(&s("command"))),
            truncate(&s("aggregatedOutput"), 6000),
            status,
        ),
        "fileChange" => {
            let diffs: Vec<FileDiff> = item
                .get("changes")?
                .as_array()?
                .iter()
                .filter_map(|c| {
                    Some(FileDiff { path: c.get("path")?.as_str()?.to_owned(), patch: c.get("diff")?.as_str()?.to_owned() })
                })
                .collect();
            let files: Vec<&str> = diffs.iter().map(|d| short_path(&d.path)).collect();
            Item::tool(id, format!("Правки: {}", files.join(", ")), String::new(), status).with_diffs(diffs)
        }
        "mcpToolCall" => {
            // MCP results are content blocks: text, and images as base64.
            let mut text = Vec::new();
            let mut images = Vec::new();
            for c in item.pointer("/result/content").and_then(Value::as_array).into_iter().flatten() {
                match c.get("type").and_then(Value::as_str) {
                    Some("image") => images.extend(
                        c.get("data")
                            .and_then(Value::as_str)
                            .and_then(|d| store.store_base64(c.get("mimeType").and_then(Value::as_str).unwrap_or("image/png"), d)),
                    ),
                    _ => text.extend(c.get("text").and_then(Value::as_str).map(str::to_owned)),
                }
            }
            let body = match item.get("error").filter(|e| !e.is_null()) {
                Some(err) => err.get("message").and_then(Value::as_str).map(str::to_owned).unwrap_or_else(|| err.to_string()),
                None => text.join("\n"),
            };
            Item::tool(id, format!("{}.{}", s("server"), s("tool")), truncate(&body, 3000), status).with_attachments(images)
        }
        "imageView" => Item::tool(id, format!("Смотрит {}", short_path(&s("path"))), String::new(), ItemStatus::Done)
            .with_attachments(vec![Attachment::for_path(s("path"))]),
        "imageGeneration" => {
            let prompt = item.get("revisedPrompt").and_then(Value::as_str).unwrap_or("картинка");
            let saved = item.get("savedPath").and_then(Value::as_str);
            let status = if item.get("failure").is_some_and(|f| !f.is_null()) { ItemStatus::Failed } else { status };
            Item::tool(id, format!("Генерация: {prompt}"), String::new(), status)
                .with_attachments(saved.map(|p| vec![Attachment::for_path(p)]).unwrap_or_default())
        }
        "dynamicToolCall" => Item::tool(id, s("tool"), String::new(), status),
        "webSearch" => Item::tool(id, format!("Поиск: {}", s("query")), String::new(), status),
        "contextCompaction" => Item::message(id, ItemKind::Notice, "Контекст сжат"),
        _ => return None,
    };
    Some(it)
}
