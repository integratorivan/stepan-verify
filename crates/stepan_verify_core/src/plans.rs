//! Plans of plan-first tasks: `~/.stepan_verify/state/plans/<task>/`.
//!
//! The planning agent writes the plan as a report of the target state (the
//! same schema as the «Изменения» map, plus `stages` and `questions`) into
//! `draft/`, then runs `stepan_verify task plan-ready`, which moves the draft
//! into `runs/<n>/` — the same runs layout the changes map uses, so every
//! rewrite after your remarks is its own iteration.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::Home;

/// A vertical slice of the plan: after it you can run or see something.
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct Stage {
    pub title: String,
    /// What you can run or see once the stage is done.
    pub result: String,
    /// Map node ids the stage touches.
    pub nodes: Vec<String>,
    /// How to verify the stage.
    pub check: String,
}

/// A decision the plan needs from you, with the agent's own pick.
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(from = "QuestionRepr")]
pub struct Question {
    pub id: String,
    pub question: String,
    pub options: Vec<String>,
    pub recommendation: String,
    pub why: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum QuestionRepr {
    Text(String),
    Obj {
        #[serde(default)]
        id: String,
        #[serde(default, alias = "title", alias = "text")]
        question: String,
        #[serde(default)]
        options: Vec<String>,
        #[serde(default, alias = "recommend", alias = "default")]
        recommendation: String,
        #[serde(default, alias = "reason")]
        why: String,
    },
}

impl From<QuestionRepr> for Question {
    fn from(r: QuestionRepr) -> Self {
        match r {
            QuestionRepr::Text(question) => Question { question, ..Default::default() },
            QuestionRepr::Obj { id, question, options, recommendation, why } => Question { id, question, options, recommendation, why },
        }
    }
}

/// The parts of a plan report the board itself cares about.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Outline {
    pub title: String,
    pub stages: Vec<Stage>,
    pub questions: Vec<Question>,
}

/// `state/plans/<task>`.
pub fn dir(home: &Home, task: &str) -> PathBuf {
    home.state().join("plans").join(task)
}

/// Where the agent writes the next iteration.
pub fn draft(home: &Home, task: &str) -> PathBuf {
    dir(home, task).join("draft")
}

pub fn run_dir(home: &Home, task: &str, n: usize) -> PathBuf {
    dir(home, task).join("runs").join(n.to_string())
}

pub fn report_path(home: &Home, task: &str, n: usize) -> PathBuf {
    run_dir(home, task, n).join("report.json")
}

/// Iteration numbers on disk, oldest first.
pub fn runs(home: &Home, task: &str) -> Vec<usize> {
    let mut out: Vec<usize> = fs::read_dir(dir(home, task).join("runs"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse().ok())
        .collect();
    out.sort_unstable();
    out
}

pub fn latest(home: &Home, task: &str) -> Option<usize> {
    runs(home, task).last().copied()
}

/// Read a plan report's stages and questions.
pub fn outline(path: &Path) -> Result<Outline> {
    let text = fs::read_to_string(path).with_context(|| format!("нет {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("{} не читается", path.display()))
}

/// Check the draft and turn it into the next iteration `runs/<n>`. Returns `n`.
pub fn publish(home: &Home, task: &str, by: &str) -> Result<usize> {
    let draft = draft(home, task);
    let report = draft.join("report.json");
    let text = fs::read_to_string(&report)
        .with_context(|| format!("нет плана: запиши его в {} и запусти снова", report.display()))?;
    let value: serde_json::Value = serde_json::from_str(&text).with_context(|| format!("{} — не JSON", report.display()))?;
    let outline: Outline = serde_json::from_value(value.clone()).context("stages/questions не по схеме")?;
    let code = value.get("code");
    let nodes = ["added", "changed", "removed"]
        .iter()
        .filter_map(|k| code.and_then(|c| c.get(k)).and_then(|l| l.as_array()))
        .map(Vec::len)
        .sum::<usize>();
    if nodes == 0 {
        bail!("в плане нет узлов: заполни code.added / changed / removed — что появится, изменится и уберётся");
    }
    if outline.stages.is_empty() {
        bail!("в плане нет этапов: заполни stages — вертикальные срезы с результатом и проверкой");
    }
    let n = latest(home, task).map_or(1, |n| n + 1);
    let target = run_dir(home, task, n);
    fs::create_dir_all(target.parent().expect("runs dir"))?;
    fs::rename(&draft, &target).with_context(|| format!("не удалось перенести {} в {}", draft.display(), target.display()))?;
    let meta = serde_json::json!({ "at": chrono::Utc::now(), "reviewer": by });
    fs::write(target.join("meta.json"), serde_json::to_string_pretty(&meta)?)?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home(tag: &str) -> Home {
        let root = std::env::temp_dir().join(format!("stepan_verify-plans-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        Home { root }
    }

    #[test]
    fn stages_and_questions_parse() {
        let o: Outline = serde_json::from_str(
            r#"{"title": "t", "stages": [{"title": "Модель", "result": "задача хранит план", "nodes": ["meta"], "check": "cargo test"}],
                "questions": ["Хранить в YAML?", {"id": "q2", "question": "Отдельная вкладка?", "options": ["да", "нет"], "recommendation": "нет", "why": "меньше UI"}]}"#,
        )
        .unwrap();
        assert_eq!(o.stages[0].nodes, ["meta"]);
        assert_eq!(o.questions[0].question, "Хранить в YAML?");
        assert_eq!(o.questions[1].recommendation, "нет");
        assert_eq!(o.questions[1].options.len(), 2);
    }

    #[test]
    fn publish_numbers_iterations_and_checks_the_draft() {
        let home = home("publish");
        assert!(publish(&home, "t1", "claude").is_err(), "no draft");
        let draft = draft(&home, "t1");
        fs::create_dir_all(&draft).unwrap();
        fs::write(draft.join("report.json"), r#"{"title": "x", "code": {"added": []}, "stages": [{"title": "s"}]}"#).unwrap();
        assert!(publish(&home, "t1", "claude").unwrap_err().to_string().contains("нет узлов"));
        fs::write(draft.join("report.json"), r#"{"title": "x", "code": {"added": [{"id": "a"}]}}"#).unwrap();
        assert!(publish(&home, "t1", "claude").unwrap_err().to_string().contains("нет этапов"));
        let good = r#"{"title": "x", "code": {"added": [{"id": "a"}]}, "stages": [{"title": "s", "nodes": ["a"]}]}"#;
        fs::write(draft.join("report.json"), good).unwrap();
        fs::write(draft.join("explainer.html"), "<p>ui</p>").unwrap();
        assert_eq!(publish(&home, "t1", "claude").unwrap(), 1);
        assert!(!draft.exists());
        assert!(run_dir(&home, "t1", 1).join("explainer.html").exists());
        assert!(fs::read_to_string(run_dir(&home, "t1", 1).join("meta.json")).unwrap().contains("claude"));
        fs::create_dir_all(&draft).unwrap();
        fs::write(draft.join("report.json"), good).unwrap();
        assert_eq!(publish(&home, "t1", "claude").unwrap(), 2);
        assert_eq!(runs(&home, "t1"), [1, 2]);
        assert_eq!(outline(&report_path(&home, "t1", 2)).unwrap().stages.len(), 1);
        let _ = fs::remove_dir_all(&home.root);
    }
}
