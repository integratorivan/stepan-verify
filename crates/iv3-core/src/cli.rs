//! `iv3 task …`: the board from the command line, for agents and scripts.
//!
//! Agents launched by iv3 get `IV3_AGENT` and `IV3_TASK` in their
//! environment, so `--by` and the task id can be omitted there.

use std::{path::PathBuf, time::Duration};

use anyhow::{Context, Result, bail};

use crate::{Board, Home, NewTask, Task, TaskStatus};

pub const USAGE: &str = "\
iv3 task — доска задач iv3 (~/.iv3/tasks)

  iv3 task list [--ready] [--status STATUS] [--json]
  iv3 task show [ID]
  iv3 task create TITLE [--body TEXT] [--cwd DIR] [--label k=v]... [--depends ID]... [--attach PATH]... [--inbox]
  iv3 task claim [ID | --ready [--label k=v]] [--by NAME] [--lease 1h]
  iv3 task renew [ID] [--by NAME] [--lease 1h]
  iv3 task log [ID] TEXT [--by NAME]
  iv3 task complete [ID] SUMMARY [--by NAME]     → на ревью человеку
  iv3 task block [ID] QUESTION [--by NAME]       → нужен ответ человека
  iv3 task release [ID] [--by NAME]              → вернуть в очередь
  iv3 task status ID inbox|todo|in_progress|blocked|review|done

ID по умолчанию берётся из $IV3_TASK, NAME — из $IV3_AGENT (иначе \"me\").
";

struct Args {
    positional: Vec<String>,
    flags: Vec<(String, Option<String>)>,
}

impl Args {
    fn parse(raw: &[String]) -> Self {
        const VALUED: [&str; 9] = ["--status", "--body", "--cwd", "--label", "--depends", "--attach", "--by", "--lease", "--json-unused"];
        let mut positional = Vec::new();
        let mut flags = Vec::new();
        let mut it = raw.iter();
        while let Some(a) = it.next() {
            if a.starts_with("--") {
                let value = VALUED.contains(&a.as_str()).then(|| it.next().cloned()).flatten();
                flags.push((a.clone(), value));
            } else {
                positional.push(a.clone());
            }
        }
        Self { positional, flags }
    }
    fn has(&self, flag: &str) -> bool {
        self.flags.iter().any(|(f, _)| f == flag)
    }
    fn value(&self, flag: &str) -> Option<String> {
        self.flags.iter().find(|(f, _)| f == flag).and_then(|(_, v)| v.clone())
    }
    fn values(&self, flag: &str) -> Vec<String> {
        self.flags.iter().filter(|(f, _)| f == flag).filter_map(|(_, v)| v.clone()).collect()
    }
}

/// Runs `iv3 task <args>`; returns the text to print.
pub fn run(home: &Home, raw: &[String]) -> Result<String> {
    home.ensure()?;
    let board = Board::new(home.clone());
    let Some((cmd, rest)) = raw.split_first() else { return Ok(USAGE.to_owned()) };
    let args = Args::parse(rest);
    let by = args.value("--by").or_else(|| std::env::var("IV3_AGENT").ok()).unwrap_or_else(|| "me".into());
    let lease = args.value("--lease").map(|l| parse_duration(&l)).transpose()?.unwrap_or(Duration::from_secs(3600));
    // `[ID] TEXT`: with one positional it is the text, the id comes from $IV3_TASK.
    let id_and_text = || -> Result<(String, String)> {
        match args.positional.as_slice() {
            [id, text, ..] => Ok((id.clone(), text.clone())),
            [text] => Ok((env_task()?, text.clone())),
            [] => bail!("нужен текст"),
        }
    };
    let id_only = || -> Result<String> { args.positional.first().cloned().map_or_else(env_task, Ok) };

    let task = match cmd.as_str() {
        "list" | "ls" => {
            let all = board.tasks();
            let status = args.value("--status").map(|s| TaskStatus::parse(&s).context("неизвестный статус")).transpose()?;
            let shown: Vec<&Task> = all
                .iter()
                .filter(|t| !args.has("--ready") || t.is_ready(&all))
                .filter(|t| status.is_none_or(|s| t.meta.status == s))
                .collect();
            if args.has("--json") {
                let rows: Vec<_> = shown
                    .iter()
                    .map(|t| serde_json::json!({"id": t.id, "title": t.meta.title, "status": t.meta.status, "labels": t.meta.labels, "claimed_by": t.meta.claimed_by, "cwd": t.meta.cwd, "attachments": t.meta.attachments}))
                    .collect();
                return Ok(serde_json::to_string_pretty(&rows)?);
            }
            return Ok(shown.iter().map(|t| line(t, &all)).collect::<Vec<_>>().join("\n"));
        }
        "show" => {
            let task = board.get(&id_only()?)?;
            let journal: Vec<String> = board
                .events(Some(&task.id))
                .iter()
                .map(|e| format!("  {} {} {}{}", e.at.format("%m-%d %H:%M"), e.actor, e.kind, if e.text.is_empty() { String::new() } else { format!(": {}", e.text) }))
                .collect();
            let files: String =
                task.meta.attachments.iter().map(|p| format!("  {}\n", p.display())).collect();
            let files = if files.is_empty() { String::new() } else { format!("\nВложения:\n{files}") };
            return Ok(format!("{}\n\n{}\n{files}Журнал:\n{}", line(&task, &board.tasks()), task.body.trim(), journal.join("\n")));
        }
        "create" | "add" => {
            let title = args.positional.first().context("нужен заголовок")?.clone();
            let cwd = args.value("--cwd").map(PathBuf::from).map(|p| p.canonicalize().unwrap_or(p));
            board.create(
                NewTask {
                    title,
                    body: args.value("--body").unwrap_or_default(),
                    cwd,
                    labels: args.values("--label"),
                    depends_on: args.values("--depends"),
                    attachments: args
                        .values("--attach")
                        .into_iter()
                        .map(PathBuf::from)
                        .map(|p| p.canonicalize().unwrap_or(p))
                        .collect(),
                    status: if args.has("--inbox") { TaskStatus::Inbox } else { TaskStatus::Todo },
                },
                &by,
            )?
        }
        "claim" => {
            if args.has("--ready") {
                match board.claim_ready(&by, args.value("--label").as_deref(), lease)? {
                    Some(t) => t,
                    None => return Ok("готовых задач нет".into()),
                }
            } else {
                board.claim(&id_only()?, &by, lease)?
            }
        }
        "renew" => board.renew(&id_only()?, &by, lease)?,
        "log" => {
            let (id, text) = id_and_text()?;
            board.log(&id, &by, &text)?
        }
        "complete" | "done" => {
            let (id, text) = id_and_text()?;
            board.complete(&id, &by, &text)?
        }
        "block" => {
            let (id, text) = id_and_text()?;
            board.block(&id, &by, &text)?
        }
        "release" => board.release(&id_only()?, &by, "")?,
        "status" => {
            let [id, status, ..] = args.positional.as_slice() else { bail!("iv3 task status ID STATUS") };
            board.set_status(id, &by, TaskStatus::parse(status).context("неизвестный статус")?)?
        }
        "help" | "--help" | "-h" => return Ok(USAGE.to_owned()),
        other => bail!("неизвестная команда `{other}`\n\n{USAGE}"),
    };
    Ok(line(&task, &board.tasks()))
}

fn env_task() -> Result<String> {
    std::env::var("IV3_TASK").context("укажи ID задачи (или запусти из сессии iv3, где задан $IV3_TASK)")
}

fn line(t: &Task, all: &[Task]) -> String {
    let mut parts = vec![format!("{}  [{}]  {}", t.id, t.meta.status.label(), t.meta.title)];
    if let Some(p) = t.project_name() {
        parts.push(format!("проект: {p}"));
    }
    if let Some(by) = &t.meta.claimed_by {
        parts.push(format!("взял: {by}"));
    }
    let deps = t.open_dependencies(all);
    if !deps.is_empty() {
        parts.push(format!("ждёт: {}", deps.join(", ")));
    }
    if !t.meta.labels.is_empty() {
        parts.push(t.meta.labels.join(" "));
    }
    parts.join("  ·  ")
}

/// "90s", "30m", "2h", "1d".
pub fn parse_duration(s: &str) -> Result<Duration> {
    let (num, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len()));
    let n: u64 = num.parse().with_context(|| format!("не понял длительность `{s}`"))?;
    let secs = match unit {
        "" | "s" => n,
        "m" => n * 60,
        "h" => n * 3600,
        "d" => n * 86400,
        _ => bail!("не понял длительность `{s}`"),
    };
    Ok(Duration::from_secs(secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("90s").unwrap().as_secs(), 90);
        assert_eq!(parse_duration("30m").unwrap().as_secs(), 1800);
        assert_eq!(parse_duration("2h").unwrap().as_secs(), 7200);
        assert!(parse_duration("soon").is_err());
    }

    #[test]
    fn cli_roundtrip() {
        let root = std::env::temp_dir().join(format!("iv3-cli-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = Home { root: root.clone() };
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let out = run(&home, &s(&["create", "Починить логин", "--label", "kind=bug"])).unwrap();
        let id = out.split_whitespace().next().unwrap().to_owned();
        assert!(run(&home, &s(&["list", "--ready"])).unwrap().contains("Починить логин"));
        assert!(run(&home, &s(&["claim", "--ready", "--by", "claude"])).unwrap().contains("взял: claude"));
        assert!(run(&home, &s(&["log", &id, "нашёл причину", "--by", "claude"])).is_ok());
        assert!(run(&home, &s(&["complete", &id, "PR #7", "--by", "claude"])).unwrap().contains("[Ревью]"));
        let show = run(&home, &s(&["show", &id])).unwrap();
        assert!(show.contains("claude log: нашёл причину"), "{show}");

        let shot = root.join("shot.png");
        std::fs::write(&shot, b"png").unwrap();
        let shot = shot.to_string_lossy().into_owned();
        let out = run(&home, &s(&["create", "Кнопка уехала", "--attach", &shot, "--attach", &shot, "--inbox"])).unwrap();
        let id = out.split_whitespace().next().unwrap().to_owned();
        let show = run(&home, &s(&["show", &id])).unwrap();
        assert!(show.contains(&format!("{id}.files/shot.png")) && show.contains(&format!("{id}.files/shot-2.png")), "{show}");
        assert!(run(&home, &s(&["create", "x", "--attach", "/nope/missing.png"])).is_err());
        let _ = std::fs::remove_dir_all(root);
    }
}
