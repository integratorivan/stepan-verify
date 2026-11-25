//! Read-only glance at sessions running outside stepan_verify (a terminal, Codex
//! Desktop, t3code): parse only the end of their transcript file.

use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

use serde_json::Value;

use crate::{
    claude::plan_mark,
    model::{Item, ItemKind, ItemStatus},
};

/// Enough for the current turn of a busy session, cheap to re-read every few seconds.
pub(crate) const TAIL_BYTES: u64 = 512 * 1024;

/// Complete lines from the last `bytes` of a file.
pub(crate) fn tail_lines(path: &Path, bytes: u64) -> Vec<String> {
    let Ok(mut file) = File::open(path) else { return Vec::new() };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(bytes);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut buf = Vec::new();
    if file.read_to_end(&mut buf).is_err() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    if start > 0 && !lines.is_empty() {
        lines.remove(0); // cut mid-line
    }
    lines
}

/// What an external session is doing right now.
pub struct Peek {
    pub items: Vec<Item>,
    /// Known for Codex (task_started / task_complete); None for Claude.
    pub running: Option<bool>,
}

pub fn peek_claude(path: &Path) -> Peek {
    Peek { items: crate::claude::peek(path), running: None }
}

/// Codex rollout: `event_msg` records carry item_started / item_completed
/// with PascalCase item types, plus task_started / task_complete.
pub fn peek_codex(path: &Path) -> Peek {
    let mut items: Vec<Item> = Vec::new();
    let mut running = None;
    for line in tail_lines(path, TAIL_BYTES) {
        if !line.contains("\"event_msg\"") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
        let Some(p) = v.get("payload") else { continue };
        match p.get("type").and_then(Value::as_str) {
            Some("task_started") => running = Some(true),
            Some("task_complete") | Some("turn_aborted") => running = Some(false),
            Some(kind @ ("item_started" | "item_completed")) => {
                let done = kind == "item_completed";
                if let Some(item) = p.get("item").and_then(|i| codex_item(i, done)) {
                    match items.iter().position(|x| x.id == item.id) {
                        Some(ix) => items[ix] = item,
                        None => items.push(item),
                    }
                }
            }
            _ => {}
        }
    }
    Peek { items, running }
}

fn codex_item(item: &Value, done: bool) -> Option<Item> {
    let s = |k: &str| item.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();
    let id = s("id");
    let status = if done { ItemStatus::Done } else { ItemStatus::Running };
    let texts = |k: &str| -> String {
        item.get(k)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|c| c.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n")
    };
    Some(match item.get("type")?.as_str()? {
        "UserMessage" => {
            let text = texts("content");
            if text.trim().is_empty() || text.starts_with('<') {
                return None;
            }
            Item::message(id, ItemKind::User, text)
        }
        "AgentMessage" => Item::message(id, ItemKind::Assistant, texts("content")),
        "CommandExecution" => {
            let cmd = item.get("command").and_then(Value::as_array).and_then(|c| c.last()).and_then(Value::as_str).unwrap_or_default();
            Item::tool(id, format!("$ {cmd}"), "", status)
        }
        "McpToolCall" => Item::tool(id, format!("{} · {}", s("server"), s("tool")), "", status),
        "FileChange" | "PatchApply" => Item::tool(id, "Правит файлы", "", status),
        "WebSearch" => Item::tool(id, format!("Поиск: {}", s("query")), "", status),
        "Plan" | "TodoList" => {
            let steps: Vec<String> = item
                .get("plan")
                .or_else(|| item.get("items"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|t| {
                    let text = ["step", "text", "content"].iter().find_map(|k| t.get(*k).and_then(Value::as_str)).unwrap_or_default();
                    format!("{} {text}", plan_mark(t.get("status").and_then(Value::as_str).unwrap_or_default()))
                })
                .collect();
            Item::message(id, ItemKind::Plan, steps.join("\n"))
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_rollout_tail() {
        let dir = std::env::temp_dir().join(format!("stepan_verify-peek-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rollout.jsonl");
        let lines = [
            r#"{"type":"event_msg","payload":{"type":"task_started"}}"#,
            r#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"UserMessage","id":"u","content":[{"type":"text","text":"почини тесты"}]}}}"#,
            r#"{"type":"event_msg","payload":{"type":"item_started","item":{"type":"CommandExecution","id":"c1","command":["/bin/zsh","-lc","npm test"]}}}"#,
        ];
        std::fs::write(&path, lines.join("\n")).unwrap();
        let p = peek_codex(&path);
        assert_eq!(p.running, Some(true));
        assert_eq!(p.items.len(), 2);
        assert_eq!(p.items[1].title, "$ npm test");
        assert_eq!(p.items[1].status, ItemStatus::Running);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn tail_drops_the_partial_first_line() {
        let dir = std::env::temp_dir().join(format!("stepan_verify-tail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("f");
        std::fs::write(&path, "aaaa\nbbbb\ncccc\n").unwrap();
        assert_eq!(tail_lines(&path, 7), vec!["cccc".to_owned()]);
        let _ = std::fs::remove_dir_all(dir);
    }
}
