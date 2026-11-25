//! `/show-me`: HumanLayer's visual-explanation skill, shipped with Stepan.
//! Every Claude / Codex session gets it (linked like `stepan-verify-flows`);
//! the files it produces (`/tmp/show-me/show-me-*.html|png|svg`) are attached to
//! the transcript and the HTML opens in Stepan's preview panel.

use std::path::PathBuf;

use crate::{
    flowsync::{self, SkillReport},
    Home,
};

pub const SKILL_NAME: &str = "show-me";

/// The skill text: upstream show-me plus how Stepan shows its files.
pub fn skill_text() -> &'static str {
    include_str!("show_me_skill.md")
}

/// Our copy: `<home>/skills/show-me/`.
pub fn skill_dir(home: &Home) -> PathBuf {
    home.skills().join(SKILL_NAME)
}

/// Write the skill and link it for Claude and Codex; idempotent, never
/// replaces a show-me the user installed themselves.
pub fn install_skill(home: &Home, targets: &[PathBuf]) -> SkillReport {
    flowsync::install_bundled(home, SKILL_NAME, skill_text(), targets)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn installs_next_to_a_foreign_show_me_without_touching_it() {
        let root = std::env::temp_dir().join(format!("sv-showme-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let home = Home { root: root.join("home") };
        let (claude, agents) = (root.join("claude"), root.join("agents"));
        fs::create_dir_all(agents.join(SKILL_NAME)).unwrap();
        fs::write(agents.join(SKILL_NAME).join("SKILL.md"), "mine").unwrap();

        let r = install_skill(&home, &[claude.clone(), agents.clone()]);
        assert_eq!(r.linked, vec![claude.join(SKILL_NAME)]);
        assert_eq!(r.warnings.len(), 1, "{:?}", r.warnings);
        assert_eq!(fs::read_to_string(claude.join(SKILL_NAME).join("SKILL.md")).unwrap(), skill_text());
        assert_eq!(fs::read_to_string(agents.join(SKILL_NAME).join("SKILL.md")).unwrap(), "mine");
        assert!(skill_text().starts_with("---\nname: show-me\n"));
        assert!(skill_text().contains("STEPAN_VERIFY_DOCS"));
        let _ = fs::remove_dir_all(&root);
    }
}
