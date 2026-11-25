//! Append-only journal of board activity: `~/.iv3/state/events.jsonl`.
//! One JSON object per line; never rewritten, so humans and agents can tail it.

use std::{
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Write},
};

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::Home;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Event {
    pub at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// "claude", "codex", "me", "iv3"…
    pub actor: String,
    /// create, claim, log, complete, block, release, status…
    pub kind: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
}

pub fn append(home: &Home, event: &Event) -> Result<()> {
    fs::create_dir_all(home.state())?;
    let mut file = OpenOptions::new().create(true).append(true).open(home.state().join("events.jsonl"))?;
    let mut line = serde_json::to_string(event)?;
    line.push('\n');
    file.write_all(line.as_bytes())?;
    Ok(())
}

/// All events, or only those of one task, oldest first.
pub fn read(home: &Home, task: Option<&str>) -> Vec<Event> {
    let Ok(file) = fs::File::open(home.state().join("events.jsonl")) else { return Vec::new() };
    BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|l| serde_json::from_str::<Event>(&l).ok())
        .filter(|e| task.is_none_or(|t| e.task.as_deref() == Some(t)))
        .collect()
}
