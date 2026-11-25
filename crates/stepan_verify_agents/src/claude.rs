//! Claude Code over `claude -p --input-format stream-json --output-format stream-json`.
//!
//! The CLI is spawned lazily on the first message, so opening a session only
//! to read it costs nothing. Permission prompts arrive as
//! `control_request{subtype:"can_use_tool"}` because of
//! `--permission-prompt-tool stdio` and are answered with `control_response`.
//!
//! History (the session .jsonl) and the live stream share one [`Mapper`], so a
//! message looks the same whether it was loaded or streamed.

use std::{
    collections::HashMap,
    fs::File,
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Child, ChildStdin, Command as Proc, Stdio},
    sync::{Arc, Mutex, mpsc},
};

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::{
    Command, Mode, SessionSpec,
    model::{
        AgentEvent, Approval, Attachment, Emitter, FileDiff, ImageStore, Item, ItemKind, ItemStatus, unified,
    },
};

const MAX_TOOL_OUTPUT: usize = 6000;

struct Shared {
    session_id: Option<String>,
    /// request_id → original tool input, echoed back on allow.
    pending: HashMap<String, Value>,
}

struct Live {
    child: Child,
    stdin: ChildStdin,
}

pub(crate) fn run(spec: SessionSpec, rx: mpsc::Receiver<Command>, events: &Emitter) -> Result<()> {
    let store = ImageStore { dir: spec.image_cache.clone() };
    let history = match &spec.transcript {
        Some(path) => read_history(path, &store)?,
        None => Vec::new(),
    };
    events.emit(AgentEvent::History(history));
    let shared = Arc::new(Mutex::new(Shared { session_id: spec.native_id.clone(), pending: HashMap::new() }));
    let mut live: Option<Live> = None;
    let mut sent = 0usize;
    let events = Emitter(events.0.clone());

    while let Ok(cmd) = rx.recv() {
        // A crashed or finished CLI is respawned on the next message.
        if let Some(l) = &mut live {
            if l.child.try_wait().ok().flatten().is_some() {
                live = None;
            }
        }
        match cmd {
            Command::Send(message) => {
                if live.is_none() {
                    live = Some(spawn(&spec, &shared, &store, &events)?);
                }
                sent += 1;
                let (content, attachments) = user_content(&message);
                events.emit(AgentEvent::Upsert(
                    Item::message(format!("local-user-{sent}"), ItemKind::User, message.text.clone()).with_attachments(attachments),
                ));
                events.emit(AgentEvent::TurnStarted);
                write_line(
                    &mut live.as_mut().unwrap().stdin,
                    &json!({"type":"user","message":{"role":"user","content":content},"parent_tool_use_id":null}),
                )?;
            }
            Command::Interrupt => {
                if let Some(l) = &mut live {
                    write_line(
                        &mut l.stdin,
                        &json!({"type":"control_request","request_id":format!("stepan_verify-int-{sent}"),"request":{"subtype":"interrupt"}}),
                    )?;
                }
            }
            Command::Approve { id, allow } => {
                let input = shared.lock().unwrap().pending.remove(&id);
                if let (Some(l), Some(input)) = (&mut live, input) {
                    let response = if allow {
                        json!({"behavior":"allow","updatedInput":input})
                    } else {
                        json!({"behavior":"deny","message":"Пользователь отклонил это действие."})
                    };
                    write_line(
                        &mut l.stdin,
                        &json!({"type":"control_response","response":{"subtype":"success","request_id":id,"response":response}}),
                    )?;
                    events.emit(AgentEvent::ApprovalResolved(id));
                }
            }
            Command::Shutdown => break,
        }
    }
    if let Some(mut l) = live {
        let _ = l.child.kill();
        let _ = l.child.wait();
    }
    Ok(())
}

fn spawn(spec: &SessionSpec, shared: &Arc<Mutex<Shared>>, store: &ImageStore, events: &Emitter) -> Result<Live> {
    let mode = match spec.mode {
        Mode::Ask => "manual",
        Mode::AutoEdits => "acceptEdits",
        Mode::FullAccess => "bypassPermissions",
    };
    let mut cmd = Proc::new("claude");
    cmd.args(["-p", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose"])
        .args(["--include-partial-messages", "--permission-prompt-tool", "stdio", "--permission-mode", mode])
        .current_dir(&spec.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    crate::scrub_env(&mut cmd);
    crate::docs_env(&mut cmd, &spec);
    cmd.envs(spec.env.iter().map(|(k, v)| (k, v)));
    if let Some(model) = &spec.model {
        cmd.args(["--model", model]);
    }
    if let Some(text) = spec.instructions.as_deref().filter(|t| !t.trim().is_empty()) {
        cmd.args(["--append-system-prompt", text]);
    }
    // Memory is for every session; the board only for task sessions.
    cmd.args(["--allowedTools", "Bash(stepan_verify memory *)"]);
    if crate::board_home(spec).is_some() {
        cmd.arg("Bash(stepan_verify task *)");
    }
    if spec.mode == Mode::FullAccess {
        cmd.arg("--allow-dangerously-skip-permissions");
    }
    if let Some(id) = &shared.lock().unwrap().session_id {
        cmd.args(["--resume", id]);
    }
    let mut child = cmd.spawn().context("не удалось запустить `claude`")?;
    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let shared = shared.clone();
    let events = Emitter(events.0.clone());
    let store = store.clone();
    std::thread::spawn(move || {
        let mut stream = StreamState { mapper: Mapper::new(store), ..Default::default() };
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                stream.handle(&v, &shared, &events);
            }
        }
    });
    Ok(Live { child, stdin })
}

fn write_line(stdin: &mut ChildStdin, v: &Value) -> Result<()> {
    let mut line = serde_json::to_string(v)?;
    line.push('\n');
    stdin.write_all(line.as_bytes()).context("claude stdin закрыт")?;
    stdin.flush()?;
    Ok(())
}

/// Message content for stream-json input: plain text, or text + base64 images.
/// Unreadable images are skipped and reported in the text so the agent knows.
fn user_content(message: &crate::Outgoing) -> (Value, Vec<Attachment>) {
    use base64::Engine as _;
    if message.images.is_empty() {
        return (json!(message.text), Vec::new());
    }
    let mut parts = Vec::new();
    let mut attachments = Vec::new();
    let mut missing = Vec::new();
    for path in &message.images {
        match std::fs::read(path) {
            Ok(bytes) => {
                let media_type = match path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
                    Some("jpg" | "jpeg") => "image/jpeg",
                    Some("gif") => "image/gif",
                    Some("webp") => "image/webp",
                    _ => "image/png",
                };
                let data = base64::engine::general_purpose::STANDARD.encode(bytes);
                parts.push(json!({"type":"image","source":{"type":"base64","media_type":media_type,"data":data}}));
                attachments.push(Attachment::for_path(path.clone()));
            }
            Err(_) => missing.push(path.display().to_string()),
        }
    }
    let mut text = message.text.clone();
    if !missing.is_empty() {
        text.push_str(&format!("\n\n(Не удалось прочитать вложения: {})", missing.join(", ")));
    }
    if !text.trim().is_empty() {
        parts.insert(0, json!({"type":"text","text":text}));
    }
    (Value::Array(parts), attachments)
}

// ------------------------------------------------------------------- mapper

/// What a record changes in the transcript.
enum Op {
    Upsert(Item),
    /// Files/folders mentioned with `@` attach to the message they came with.
    AttachToLastUser(Vec<Attachment>),
}

/// Turns Claude records (assistant / user / attachment) into transcript ops.
#[derive(Default)]
struct Mapper {
    store: ImageStore,
    /// Text blocks seen per api message id, for stable `{msg}:t{n}` ids.
    texts: HashMap<String, usize>,
    /// tool_use id → the item as first shown, completed by its tool_result.
    tools: HashMap<String, Item>,
}

impl Mapper {
    fn new(store: ImageStore) -> Self {
        Self { store, ..Default::default() }
    }

    fn record(&mut self, v: &Value) -> Vec<Op> {
        let flag = |k: &str| v.get(k).and_then(Value::as_bool) == Some(true);
        if flag("isSidechain") || flag("isMeta") || v.get("parent_tool_use_id").is_some_and(|p| !p.is_null()) {
            return Vec::new(); // subagent chatter and injected context
        }
        match v.get("type").and_then(Value::as_str) {
            Some("assistant") => self.assistant(v),
            Some("user") => self.user(v),
            Some("attachment") => self.attachment(v.get("attachment").unwrap_or(&Value::Null)),
            _ => Vec::new(),
        }
    }

    fn assistant(&mut self, v: &Value) -> Vec<Op> {
        let msg = v.pointer("/message/id").and_then(Value::as_str).unwrap_or_default().to_owned();
        let uuid = str_field(v, "uuid");
        let mut ops = Vec::new();
        for (i, block) in v.pointer("/message/content").and_then(Value::as_array).into_iter().flatten().enumerate() {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    let text = str_field(block, "text");
                    let n = self.texts.entry(msg.clone()).or_default();
                    let id = format!("{msg}:t{n}");
                    *n += 1;
                    if !text.trim().is_empty() {
                        ops.push(Op::Upsert(Item::message(id, ItemKind::Assistant, text)));
                    }
                }
                Some("thinking") => {
                    let text = str_field(block, "thinking");
                    if !text.trim().is_empty() {
                        ops.push(Op::Upsert(Item::message(format!("{uuid}:th{i}"), ItemKind::Reasoning, text)));
                    }
                }
                Some("tool_use") => {
                    let id = str_field(block, "id");
                    let name = str_field(block, "name");
                    let input = block.get("input").unwrap_or(&Value::Null);
                    if let Some(plan) = plan_body(&name, input) {
                        ops.push(Op::Upsert(Item::message(format!("plan:{id}"), ItemKind::Plan, plan)));
                        continue;
                    }
                    let item = Item::tool(id.clone(), tool_title(&name, input), tool_input_body(&name, input), ItemStatus::Running)
                        .with_diffs(tool_diffs(&name, input))
                        .with_attachments(tool_input_attachments(&name, input));
                    self.tools.insert(id, item.clone());
                    ops.push(Op::Upsert(item));
                }
                _ => {}
            }
        }
        ops
    }

    fn user(&mut self, v: &Value) -> Vec<Op> {
        let uuid = str_field(v, "uuid");
        let mut ops = Vec::new();
        let mut text = Vec::new();
        let mut images = Vec::new();
        match v.pointer("/message/content") {
            Some(Value::String(s)) => text.push(s.clone()),
            Some(Value::Array(parts)) => {
                for part in parts {
                    match part.get("type").and_then(Value::as_str) {
                        Some("text") => text.push(str_field(part, "text")),
                        Some("image") => images.extend(self.image(part)),
                        Some("tool_result") => ops.extend(self.tool_result(part).map(Op::Upsert)),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        // Drop injected wrappers (command echoes, caveats, reminders), keep what you typed.
        let text: Vec<String> = text
            .into_iter()
            .map(|t| t.trim().to_owned())
            .filter(|t| !t.is_empty() && !t.starts_with('<') && !t.starts_with("[Request interrupted"))
            .collect();
        if !text.is_empty() || !images.is_empty() {
            ops.push(Op::Upsert(Item::message(uuid, ItemKind::User, text.join("\n\n")).with_attachments(images)));
        }
        ops
    }

    /// The result completes the tool item it answers; results of tools we
    /// did not show (plans, calls before a transcript tail) are dropped.
    fn tool_result(&mut self, part: &Value) -> Option<Item> {
        let id = str_field(part, "tool_use_id");
        let failed = part.get("is_error").and_then(Value::as_bool) == Some(true);
        let mut item = self.tools.get(&id).cloned()?;
        item.status = if failed { ItemStatus::Failed } else { ItemStatus::Done };
        let (output, images) = self.result_content(part.get("content"));
        // Keep the input (e.g. a subagent's prompt) above the output.
        item.body = match (item.body.is_empty(), output.is_empty()) {
            (true, _) => output,
            (false, true) => item.body,
            (false, false) => format!("{}\n\n{}", item.body, output),
        };
        item.body = truncate(&item.body, MAX_TOOL_OUTPUT);
        item.attachments.extend(images);
        if failed {
            item.diffs.clear();
        }
        Some(item)
    }

    fn result_content(&self, content: Option<&Value>) -> (String, Vec<Attachment>) {
        match content {
            Some(Value::String(s)) => (s.clone(), Vec::new()),
            Some(Value::Array(parts)) => {
                let mut text = Vec::new();
                let mut images = Vec::new();
                for p in parts {
                    match p.get("type").and_then(Value::as_str) {
                        Some("image") => images.extend(self.image(p)),
                        _ => text.extend(p.get("text").and_then(Value::as_str).map(str::to_owned)),
                    }
                }
                (text.join("\n"), images)
            }
            _ => (String::new(), Vec::new()),
        }
    }

    fn image(&self, part: &Value) -> Option<Attachment> {
        let source = part.get("source")?;
        match source.get("type").and_then(Value::as_str) {
            Some("base64") => self.store.store_base64(
                source.get("media_type").and_then(Value::as_str).unwrap_or("image/png"),
                source.get("data").and_then(Value::as_str)?,
            ),
            Some("file") | Some("path") => Some(Attachment::for_path(source.get("path").and_then(Value::as_str)?)),
            _ => None,
        }
    }

    /// Transcript-only records: mentioned files, mid-turn messages, hook errors.
    fn attachment(&mut self, a: &Value) -> Vec<Op> {
        let s = |k: &str| a.get(k).and_then(Value::as_str);
        match s("type") {
            Some("file") | Some("compact_file_reference") => {
                s("filename").map(|f| vec![Op::AttachToLastUser(vec![Attachment::for_path(f)])]).unwrap_or_default()
            }
            Some("directory") => s("path").map(|p| vec![Op::AttachToLastUser(vec![Attachment::folder(p)])]).unwrap_or_default(),
            Some("queued_command") => {
                let human = a.pointer("/origin/kind").and_then(Value::as_str).is_none_or(|k| k == "human");
                match (s("prompt"), s("source_uuid")) {
                    (Some(prompt), Some(id)) if human && !prompt.starts_with('<') => {
                        vec![Op::Upsert(Item::message(id, ItemKind::User, prompt))]
                    }
                    _ => Vec::new(),
                }
            }
            Some("hook_blocking_error") => {
                let hook = s("hookName").unwrap_or("hook");
                let err = a.pointer("/blockingError/blockingError").and_then(Value::as_str).unwrap_or_default();
                let id = format!("hook:{}:{hook}", s("toolUseID").unwrap_or_default());
                vec![Op::Upsert(Item::message(id, ItemKind::Notice, format!("Хук {hook} остановил действие: {err}")))]
            }
            _ => Vec::new(),
        }
    }
}

/// Live stdout: partial-message streaming on top of the shared mapper.
#[derive(Default)]
struct StreamState {
    mapper: Mapper,
    /// (api message id, content block index) → item id of a streaming text block.
    streaming: HashMap<(String, u64), String>,
    streamed_texts: HashMap<String, usize>,
}

impl StreamState {
    fn handle(&mut self, v: &Value, shared: &Arc<Mutex<Shared>>, events: &Emitter) {
        match v.get("type").and_then(Value::as_str) {
            Some("system") if str_field(v, "subtype") == "init" => {
                let id = str_field(v, "session_id");
                shared.lock().unwrap().session_id = Some(id.clone());
                events.emit(AgentEvent::SessionId(id));
            }
            Some("system") if str_field(v, "subtype") == "permission_denied" => {
                events.emit(AgentEvent::Upsert(Item::message(str_field(v, "uuid"), ItemKind::Notice, str_field(v, "message"))));
            }
            Some("stream_event") => self.stream_event(v, events),
            Some("assistant") | Some("user") => {
                for op in self.mapper.record(v) {
                    if let Op::Upsert(item) = op {
                        events.emit(AgentEvent::Upsert(item));
                    }
                }
            }
            Some("control_request") => {
                let req = v.get("request").cloned().unwrap_or_default();
                if str_field(&req, "subtype") != "can_use_tool" {
                    return;
                }
                let id = str_field(v, "request_id");
                let input = req.get("input").cloned().unwrap_or_default();
                let tool = str_field(&req, "tool_name");
                shared.lock().unwrap().pending.insert(id.clone(), input.clone());
                events.emit(AgentEvent::ApprovalRequested(Approval {
                    id,
                    title: tool_title(&tool, &input),
                    detail: str_field(&req, "description"),
                }));
            }
            Some("result") => {
                let failed = v.get("is_error").and_then(Value::as_bool) == Some(true);
                let error = failed.then(|| str_field(v, "result")).filter(|e| !e.is_empty());
                events.emit(AgentEvent::TurnCompleted { error });
            }
            _ => {}
        }
    }

    fn stream_event(&mut self, v: &Value, events: &Emitter) {
        if v.get("parent_tool_use_id").is_some_and(|p| !p.is_null()) {
            return;
        }
        let Some(ev) = v.get("event") else { return };
        let msg = str_field(v, "api_message_id");
        let index = ev.get("index").and_then(Value::as_u64).unwrap_or(0);
        match ev.get("type").and_then(Value::as_str) {
            Some("content_block_start") if ev.pointer("/content_block/type").and_then(Value::as_str) == Some("text") => {
                let n = self.streamed_texts.entry(msg.clone()).or_default();
                let id = format!("{msg}:t{n}");
                *n += 1;
                self.streaming.insert((msg, index), id.clone());
                let mut item = Item::message(id, ItemKind::Assistant, "");
                item.status = ItemStatus::Running;
                events.emit(AgentEvent::Upsert(item));
            }
            Some("content_block_delta") => {
                if let (Some(id), Some(text)) =
                    (self.streaming.get(&(msg, index)), ev.pointer("/delta/text").and_then(Value::as_str))
                {
                    events.emit(AgentEvent::Append { id: id.clone(), text: text.to_owned() });
                }
            }
            _ => {}
        }
    }
}

/// Rebuild the visible transcript from a Claude session file.
pub(crate) fn read_history(path: &Path, store: &ImageStore) -> Result<Vec<Item>> {
    let reader = BufReader::new(File::open(path).with_context(|| format!("{}", path.display()))?);
    let mut items = items_from_lines(reader.lines().map_while(Result::ok), store);
    // Tools whose result never landed (interrupted turn) are not running anymore.
    for item in &mut items {
        if item.status == ItemStatus::Running {
            item.status = ItemStatus::Done;
        }
    }
    Ok(items)
}

/// The end of a live session file, for a dashboard card: tools without a
/// result yet stay Running.
pub fn peek(path: &Path) -> Vec<Item> {
    items_from_lines(crate::peek::tail_lines(path, crate::peek::TAIL_BYTES).into_iter(), &ImageStore::default())
}

fn items_from_lines(lines: impl Iterator<Item = String>, store: &ImageStore) -> Vec<Item> {
    let mut mapper = Mapper::new(store.clone());
    let mut items: Vec<Item> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();

    for line in lines {
        // Cheap prefilter: hooks and other bookkeeping dominate these files.
        let wanted = line.contains("\"type\":\"user\"")
            || line.contains("\"type\":\"assistant\"")
            || line.contains("\"type\":\"queued_command\"")
            || line.contains("\"type\":\"file\"")
            || line.contains("\"type\":\"directory\"")
            || line.contains("\"type\":\"compact_file_reference\"")
            || line.contains("\"type\":\"hook_blocking_error\"");
        if !wanted {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
        for op in mapper.record(&v) {
            match op {
                Op::Upsert(item) => match index.get(&item.id) {
                    Some(&ix) => items[ix] = item,
                    None => {
                        index.insert(item.id.clone(), items.len());
                        items.push(item);
                    }
                },
                Op::AttachToLastUser(attachments) => match items.last_mut() {
                    Some(last) if last.kind == ItemKind::User => {
                        for a in attachments {
                            if !last.attachments.contains(&a) {
                                last.attachments.push(a);
                            }
                        }
                    }
                    _ => {}
                },
            }
        }
    }
    items
}

// ------------------------------------------------------------------ helpers

fn str_field(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or_default().to_owned()
}

/// Short human title for a tool call.
pub(crate) fn tool_title(name: &str, input: &Value) -> String {
    let field = |k: &str| input.get(k).and_then(Value::as_str);
    let detail = match name {
        "Bash" => field("command").map(|c| format!("$ {c}")),
        "Read" | "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => field("file_path").map(|p| format!("{name} {p}")),
        "Grep" | "Glob" => field("pattern").map(|p| format!("{name} {p}")),
        "WebFetch" => field("url").map(|u| format!("{name} {u}")),
        "WebSearch" => field("query").map(|q| format!("{name} {q}")),
        "Task" | "Agent" => field("description").map(|d| format!("Сабагент: {d}")),
        "Skill" => field("skill").map(|d| format!("Skill {d}")),
        "SendUserFile" => Some("Отправил файлы".to_owned()),
        "TodoWrite" => Some("План".to_owned()),
        _ => mcp_title(name, input),
    };
    let title = detail.unwrap_or_else(|| {
        let args = input.to_string();
        if args == "{}" || args == "null" { name.to_owned() } else { format!("{name} {args}") }
    });
    truncate(title.lines().next().unwrap_or_default(), 160)
}

/// `mcp__Claude_Browser__find {"query":"x"}` → `Browser · find — x`.
fn mcp_title(name: &str, input: &Value) -> Option<String> {
    let rest = name.strip_prefix("mcp__")?;
    let (server, tool) = rest.split_once("__")?;
    let server = server.rsplit('_').next().filter(|s| !s.is_empty()).unwrap_or(server);
    // The first short string argument usually says what the call is about.
    let hint = input.as_object().and_then(|o| {
        ["query", "url", "action", "command", "description", "name", "path", "text"]
            .iter()
            .find_map(|k| o.get(*k).and_then(Value::as_str))
            .or_else(|| o.values().find_map(Value::as_str))
    });
    Some(match hint {
        Some(h) => format!("{server} · {tool} — {h}"),
        None => format!("{server} · {tool}"),
    })
}

/// Checklist tools: Claude's TodoWrite and the desktop app's update_plan.
fn plan_body(name: &str, input: &Value) -> Option<String> {
    let entries = match name {
        "TodoWrite" => input.get("todos"),
        n if n.ends_with("update_plan") => input.get("items").or_else(|| input.get("plan")),
        _ => None,
    }?
    .as_array()?;
    let lines: Vec<String> = entries
        .iter()
        .map(|t| {
            let text = ["content", "step", "title"].iter().find_map(|k| t.get(*k).and_then(Value::as_str)).unwrap_or_default();
            format!("{} {text}", plan_mark(t.get("status").and_then(Value::as_str).unwrap_or_default()))
        })
        .collect();
    (!lines.is_empty()).then(|| lines.join("\n"))
}

pub(crate) fn plan_mark(status: &str) -> &'static str {
    match status {
        "completed" | "done" => "✓",
        "in_progress" | "inProgress" | "active" => "▸",
        _ => "○",
    }
}

/// Input worth showing above the result: subagent prompts, todo lists.
fn tool_input_body(name: &str, input: &Value) -> String {
    match name {
        "Task" | "Agent" => input.get("prompt").and_then(Value::as_str).map(|p| format!("Задание:\n{p}")).unwrap_or_default(),
        "TodoWrite" => input
            .get("todos")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|t| {
                let mark = match t.get("status").and_then(Value::as_str) {
                    Some("completed") => "✓",
                    Some("in_progress") => "▸",
                    _ => "○",
                };
                format!("{mark} {}", t.get("content").and_then(Value::as_str).unwrap_or_default())
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Files the agent hands you (SendUserFile) become attachments right away.
fn tool_input_attachments(name: &str, input: &Value) -> Vec<Attachment> {
    match name {
        "SendUserFile" => input
            .get("files")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(Attachment::for_path)
            .collect(),
        _ => Vec::new(),
    }
}

/// Diffs for Claude's file-editing tools, built from the tool input.
pub(crate) fn tool_diffs(name: &str, input: &Value) -> Vec<FileDiff> {
    let field = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();
    let path = field(input, "file_path");
    let patch = match name {
        "Edit" => unified(&field(input, "old_string"), &field(input, "new_string")),
        "MultiEdit" => input
            .get("edits")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|e| unified(&field(e, "old_string"), &field(e, "new_string")))
            .collect(),
        "Write" => unified("", &field(input, "content")),
        _ => return Vec::new(),
    };
    if path.is_empty() || patch.is_empty() {
        return Vec::new();
    }
    vec![FileDiff { path, patch }]
}

pub(crate) fn truncate(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_becomes_unified_diff() {
        let input = json!({"file_path": "/p/a.js", "old_string": "a\nmul\nc\n", "new_string": "a\nmultiply\nc\n"});
        let diffs = tool_diffs("Edit", &input);
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].path, "/p/a.js");
        assert!(diffs[0].patch.contains("-mul\n+multiply"), "{}", diffs[0].patch);
        assert_eq!(diffs[0].stats(), (1, 1));
    }

    #[test]
    fn write_is_all_additions_and_bash_has_no_diff() {
        let diffs = tool_diffs("Write", &json!({"file_path": "/p/n.md", "content": "x\ny\n"}));
        assert_eq!(diffs[0].stats(), (2, 0));
        assert!(tool_diffs("Bash", &json!({"command": "ls"})).is_empty());
    }

    fn temp_store() -> (ImageStore, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("stepan_verify-test-{}", std::process::id()));
        (ImageStore { dir: Some(dir.clone()) }, dir)
    }

    // 1×1 transparent PNG.
    const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";

    #[test]
    fn user_images_and_tool_screenshots_become_attachments() {
        let (store, dir) = temp_store();
        let mut m = Mapper::new(store);
        let user = json!({"type":"user","uuid":"u1","message":{"content":[
            {"type":"text","text":"вот макет"},
            {"type":"image","source":{"type":"base64","media_type":"image/png","data":PNG}}]}});
        let Op::Upsert(item) = m.record(&user).pop().unwrap() else { panic!() };
        assert_eq!(item.body, "вот макет");
        assert_eq!(item.attachments.len(), 1);
        assert!(item.attachments[0].path.exists());

        m.record(&json!({"type":"assistant","uuid":"a1","message":{"id":"m1","content":[
            {"type":"tool_use","id":"t1","name":"mcp__sim__screenshot","input":{}}]}}));
        let result = json!({"type":"user","uuid":"u2","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":[
            {"type":"text","text":"done"},
            {"type":"image","source":{"type":"base64","media_type":"image/png","data":PNG}}]}]}});
        let Op::Upsert(tool) = m.record(&result).pop().unwrap() else { panic!() };
        assert_eq!(tool.kind, ItemKind::Tool);
        assert_eq!(tool.body, "done");
        assert_eq!(tool.attachments.len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn outgoing_images_become_base64_blocks() {
        use base64::Engine as _;
        let dir = std::env::temp_dir().join(format!("stepan_verify-out-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("shot.png");
        std::fs::write(&png, base64::engine::general_purpose::STANDARD.decode(PNG).unwrap()).unwrap();
        let (content, attachments) =
            user_content(&crate::Outgoing { text: "смотри".into(), images: vec![png.clone(), dir.join("nope.png")] });
        let parts = content.as_array().unwrap();
        assert_eq!(parts[0]["type"], "text");
        assert!(parts[0]["text"].as_str().unwrap().contains("nope.png"));
        assert_eq!(parts[1]["source"]["media_type"], "image/png");
        assert_eq!(parts[1]["source"]["data"], PNG);
        assert_eq!(attachments, vec![Attachment::for_path(png)]);
        assert_eq!(user_content(&crate::Outgoing { text: "hi".into(), images: vec![] }).0, json!("hi"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn mcp_titles_are_readable() {
        assert_eq!(tool_title("mcp__Claude_Browser__find", &json!({"query":"Claim bonus","tabId":"seed"})), "Browser · find — Claim bonus");
        assert_eq!(tool_title("mcp__posthog__exec", &json!({})), "posthog · exec");
    }

    #[test]
    fn mentioned_files_attach_to_the_user_message() {
        let (store, _) = temp_store();
        let mut m = Mapper::new(store);
        let ops = m.record(&json!({"type":"attachment","attachment":{"type":"file","filename":"/p/src/App.tsx"}}));
        assert!(matches!(&ops[..], [Op::AttachToLastUser(a)] if a[0].label() == "App.tsx"));
        let ops = m.record(&json!({"type":"attachment","attachment":{"type":"queued_command","prompt":"и ещё","source_uuid":"q1","origin":{"kind":"human"}}}));
        assert!(matches!(&ops[..], [Op::Upsert(i)] if i.kind == ItemKind::User && i.body == "и ещё"));
    }
}
