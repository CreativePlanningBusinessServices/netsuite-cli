use std::path::Path;

use serde_json::{Value, json};

use crate::error::CliError;

pub const REPO_OWNER: &str = "CreativePlanningBusinessServices";
pub const REPO_NAME: &str = "netsuite-cli";

/// self_update's backend is blocking (synchronous HTTP + file I/O), so it runs on the
/// blocking thread pool rather than tying up the async runtime.
pub async fn run(check_only: bool, skip_skill: bool) -> Result<Value, CliError> {
    tokio::task::spawn_blocking(move || {
        if check_only {
            check_for_update()
        } else {
            install_update(skip_skill)
        }
    })
    .await
    .map_err(|join_error| CliError::Network(format!("update task panicked: {join_error}")))?
}

fn check_for_update() -> Result<Value, CliError> {
    let current_version = env!("CARGO_PKG_VERSION");
    let updater = build_updater()?;
    let latest_release = updater
        .get_latest_release()
        .map_err(|update_error| CliError::Network(update_error.to_string()))?;
    let update_available = update_available(current_version, &latest_release.version)?;
    Ok(json!({
        "current": current_version,
        "latest": latest_release.version,
        "updateAvailable": update_available,
    }))
}

/// GitHub release tags are commonly spelled with a leading `v` (`v0.2.0`) while
/// `CARGO_PKG_VERSION` never has one, so plain string inequality misfires on semver-equivalent
/// spellings. Strip the prefix and compare with real semver ordering instead.
fn update_available(current: &str, latest: &str) -> Result<bool, CliError> {
    let latest_stripped = latest.strip_prefix(['v', 'V']).unwrap_or(latest);
    self_update::version::bump_is_greater(current, latest_stripped).map_err(|version_error| {
        CliError::Network(format!(
            "cannot compare versions '{current}' and '{latest}': {version_error}"
        ))
    })
}

fn install_update(skip_skill: bool) -> Result<Value, CliError> {
    // Resolve the running binary's path before the swap: self_update overwrites the file at
    // this path in place, so the path captured here is where the new binary lands. Resolving
    // current_exe() AFTER the swap is fragile on inode-based platforms — e.g. Linux's
    // /proc/self/exe can resolve to a "(deleted)" path once the original inode is replaced.
    let exe_path = std::env::current_exe();
    let updater = build_updater()?;
    let status = updater
        .update()
        .map_err(|update_error| CliError::Network(update_error.to_string()))?;
    let mut result = json!({ "updated": status.updated(), "version": status.version() });
    if !skip_skill && status.updated() {
        result["skill"] = match exe_path {
            Ok(exe) => refresh_skill_via_new_binary(&exe),
            Err(error) => skill_refresh_error(&format!("cannot locate updated binary: {error}")),
        };
    }
    Ok(result)
}

/// The running process is the OLD binary and still holds the OLD embedded skill, so it cannot
/// write the new one directly. self_update has already replaced the file at `exe`; run THAT
/// (now-new) binary's `skill install` so the fresh embedded skill lands. Never fatal — a failure
/// here leaves the binary updated and only the skill stale.
fn refresh_skill_via_new_binary(exe: &Path) -> Value {
    let output = std::process::Command::new(exe)
        .args(["skill", "install"])
        .output();
    match output {
        Ok(done) if done.status.success() => {
            // Surface the child's stderr notes (symlink/no-config-dir tips) to the user.
            if !done.stderr.is_empty() {
                eprint!("{}", String::from_utf8_lossy(&done.stderr));
            }
            serde_json::from_slice(&done.stdout)
                .map(drop_skill_name)
                .unwrap_or_else(|_| skill_refresh_error("skill install produced no JSON"))
        }
        Ok(done) => skill_refresh_error(&format!(
            "skill install exited {}: {}",
            done.status,
            String::from_utf8_lossy(&done.stderr).trim()
        )),
        Err(error) => skill_refresh_error(&format!("could not run skill install: {error}")),
    }
}

/// The child's JSON carries its own `"skill":"netsuite-cli"` self-identifier, needed when `skill
/// install` runs standalone. Here it's redundant: this value is folded under the outer
/// `result["skill"]` key, which already names the skill.
fn drop_skill_name(mut value: Value) -> Value {
    if let Some(map) = value.as_object_mut() {
        map.remove("skill");
    }
    value
}

fn skill_refresh_error(message: &str) -> Value {
    eprintln!("binary updated; skill refresh skipped — {message}");
    json!({"installed": false, "reason": message})
}

fn build_updater() -> Result<Box<dyn self_update::update::ReleaseUpdate>, CliError> {
    let mut updater = self_update::backends::github::Update::configure();
    updater
        .repo_owner(REPO_OWNER)
        .repo_name(REPO_NAME)
        .bin_name("netsuite-cli")
        .show_download_progress(false)
        .current_version(env!("CARGO_PKG_VERSION"));
    // The repo is private, so listing/downloading releases needs a token
    // (a GitHub PAT with `repo` scope, or `gh auth token`).
    if let Ok(github_token) = std::env::var("GITHUB_TOKEN") {
        updater.auth_token(&github_token);
    }
    updater
        .build()
        .map_err(|update_error| CliError::Network(update_error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_available_treats_v_prefixed_equal_versions_as_no_update() {
        assert!(!update_available("0.1.0", "v0.1.0").unwrap());
    }

    #[test]
    fn update_available_detects_a_newer_release() {
        assert!(update_available("0.1.0", "0.2.0").unwrap());
    }

    #[test]
    fn update_available_is_false_when_current_is_newer_than_latest() {
        assert!(!update_available("0.2.0", "0.1.0").unwrap());
    }

    #[test]
    fn drop_skill_name_removes_the_redundant_self_identifier() {
        let child_stdout = json!({"skill": "netsuite-cli", "installed": true, "path": "/x"});
        assert_eq!(
            drop_skill_name(child_stdout),
            json!({"installed": true, "path": "/x"})
        );
    }

    #[test]
    fn skill_refresh_error_omits_the_skill_name_field() {
        assert_eq!(
            skill_refresh_error("boom"),
            json!({"installed": false, "reason": "boom"})
        );
    }
}
