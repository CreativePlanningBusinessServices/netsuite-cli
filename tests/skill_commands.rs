use netsuite_cli::commands::skill::{self, EMBEDDED_SKILL};

#[test]
fn install_writes_embedded_skill_to_dir_override() {
    let dir = tempfile::tempdir().unwrap();
    let result = skill::install(Some(dir.path())).unwrap();
    assert_eq!(result["installed"], true);
    let written = std::fs::read_to_string(dir.path().join("SKILL.md")).unwrap();
    assert_eq!(written, EMBEDDED_SKILL);
    assert_eq!(
        result["path"],
        dir.path().join("SKILL.md").display().to_string()
    );
}

#[test]
fn second_install_reports_already_current() {
    let dir = tempfile::tempdir().unwrap();
    skill::install(Some(dir.path())).unwrap();
    let again = skill::install(Some(dir.path())).unwrap();
    assert_eq!(again["installed"], false);
    assert_eq!(again["reason"], "already current");
}

#[test]
fn install_skips_a_symlinked_target_file() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real-SKILL.md");
    std::fs::write(&real, "stale").unwrap();
    let link = dir.path().join("SKILL.md");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&real, &link).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_file(&real, &link).unwrap();
    let result = skill::install(Some(dir.path())).unwrap();
    assert_eq!(result["installed"], false);
    assert!(result["reason"].as_str().unwrap().contains("symlink"));
    // the symlinked file must be untouched
    assert_eq!(std::fs::read_to_string(&real).unwrap(), "stale");
}

#[test]
fn changed_content_is_overwritten_on_reinstall() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("SKILL.md"), "outdated").unwrap();
    let result = skill::install(Some(dir.path())).unwrap();
    assert_eq!(result["installed"], true);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("SKILL.md")).unwrap(),
        EMBEDDED_SKILL
    );
}
