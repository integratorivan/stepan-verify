//! `/describe-pr`: HumanLayer's PR skill, shipped with Stepan. The agent
//! writes a short reviewer-oriented PR body (show-me style change outline),
//! then watches CI and fixes what its PR broke, stopping before merge
//! conflicts and after three failed fixes. Linked for Claude and Codex like
//! `show-me`.

use crate::{
    flowsync::{self, SkillReport},
    Home,
};

pub const SKILL_NAME: &str = "describe-pr";

pub fn skill_text() -> &'static str {
    include_str!("describe_pr_skill.md")
}

/// Write the skill and link it for Claude and Codex; idempotent, never
/// replaces a describe-pr the user installed themselves.
pub fn install_skill(home: &Home, targets: &[std::path::PathBuf]) -> SkillReport {
    flowsync::install_bundled(home, SKILL_NAME, skill_text(), targets)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_is_explicit_only_and_keeps_the_ci_loop() {
        let text = skill_text();
        assert!(text.starts_with("---\nname: describe-pr\n"));
        assert!(text.contains("disable-model-invocation: true"));
        assert!(text.contains("gh pr checks {number} --watch --fail-fast"));
        assert!(text.contains("Stop and wait for approval"));
    }
}
