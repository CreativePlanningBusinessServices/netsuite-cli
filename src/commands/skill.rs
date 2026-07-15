use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::error::CliError;

pub const EMBEDDED_SKILL: &str = include_str!("../../skills/netsuite-cli/SKILL.md");

const SKILL_FILE: &str = "SKILL.md";

#[derive(Debug)]
pub enum SkillTarget {
    Write(PathBuf),
    Skip(&'static str),
}

pub fn install(dir_override: Option<&Path>) -> Result<Value, CliError> {
    let config_base = config_base_dir();
    let base_exists = config_base.as_deref().is_some_and(Path::exists);
    let target = resolve_target(dir_override, config_base.as_deref(), base_exists);

    let path = match target {
        SkillTarget::Skip(reason) => {
            if reason == "no Claude config dir" {
                eprintln!(
                    "skill not installed ({reason}); place it yourself with \
                     `netsuite-cli skill install --dir <your skills dir>`"
                );
            }
            return Ok(skip_json(reason));
        }
        SkillTarget::Write(path) => path,
    };

    // Re-check symlink + content here (resolve_target is pure and cannot touch disk).
    if is_symlink(&path) || path.parent().is_some_and(is_symlink) {
        return Ok(skip_json("symlink — skill tracks its source repo via git"));
    }
    if std::fs::read_to_string(&path).is_ok_and(|existing| existing == EMBEDDED_SKILL) {
        return Ok(installed_json(&path, false, Some("already current")));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|io_error| {
            CliError::Usage(format!("cannot create {}: {io_error}", parent.display()))
        })?;
    }
    std::fs::write(&path, EMBEDDED_SKILL).map_err(|io_error| {
        CliError::Usage(format!("cannot write {}: {io_error}", path.display()))
    })?;
    Ok(installed_json(&path, true, None))
}

pub fn resolve_target(
    dir_override: Option<&Path>,
    config_base: Option<&Path>,
    base_exists: bool,
) -> SkillTarget {
    if let Some(dir) = dir_override {
        let path = if dir.file_name().is_some_and(|name| name == SKILL_FILE) {
            dir.to_path_buf()
        } else {
            dir.join(SKILL_FILE)
        };
        return SkillTarget::Write(path);
    }
    match config_base {
        Some(base) if base_exists => {
            SkillTarget::Write(base.join("skills").join("netsuite-cli").join(SKILL_FILE))
        }
        _ => SkillTarget::Skip("no Claude config dir"),
    }
}

fn config_base_dir() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("CLAUDE_CONFIG_DIR")
        && !explicit.is_empty()
    {
        return Some(PathBuf::from(explicit));
    }
    directories::UserDirs::new().map(|dirs| dirs.home_dir().join(".claude"))
}

fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink())
}

fn skip_json(reason: &str) -> Value {
    json!({"skill": "netsuite-cli", "installed": false, "reason": reason})
}

fn installed_json(path: &Path, installed: bool, reason: Option<&str>) -> Value {
    let mut result = json!({
        "skill": "netsuite-cli", "path": path.display().to_string(), "installed": installed,
    });
    if let Some(reason) = reason {
        result["reason"] = json!(reason);
    }
    result
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;

    #[test]
    fn embedded_skill_carries_the_real_frontmatter() {
        assert!(EMBEDDED_SKILL.starts_with("---\nname: netsuite-cli"));
        assert!(EMBEDDED_SKILL.len() > 1000);
    }

    #[test]
    fn resolve_target_skips_when_no_override_and_base_missing() {
        let target = resolve_target(None, Some(Path::new("/no/such/base")), false);
        assert!(matches!(target, SkillTarget::Skip("no Claude config dir")));
    }

    #[test]
    fn resolve_target_builds_the_conventional_path_under_an_existing_base() {
        let base = Path::new("/home/x/.claude");
        match resolve_target(None, Some(base), true) {
            SkillTarget::Write(path) => {
                assert_eq!(
                    path,
                    PathBuf::from("/home/x/.claude/skills/netsuite-cli/SKILL.md")
                )
            }
            other => panic!("expected Write, got {other:?}"),
        }
    }

    #[test]
    fn dir_override_targets_skill_md_and_ignores_base_existence() {
        match resolve_target(Some(Path::new("/tmp/dest")), None, false) {
            SkillTarget::Write(path) => assert_eq!(path, PathBuf::from("/tmp/dest/SKILL.md")),
            other => panic!("expected Write, got {other:?}"),
        }
    }

    #[test]
    fn dir_override_that_already_names_skill_md_is_used_verbatim() {
        match resolve_target(Some(Path::new("/tmp/dest/SKILL.md")), None, false) {
            SkillTarget::Write(path) => assert_eq!(path, PathBuf::from("/tmp/dest/SKILL.md")),
            other => panic!("expected Write, got {other:?}"),
        }
    }
}
