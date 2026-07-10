use serde_json::{Value, json};

use crate::error::CliError;

pub const REPO_OWNER: &str = "CreativePlanningBusinessServices";
pub const REPO_NAME: &str = "netsuite-cli";

/// self_update's backend is blocking (synchronous HTTP + file I/O), so it runs on the
/// blocking thread pool rather than tying up the async runtime.
pub async fn run(check_only: bool) -> Result<Value, CliError> {
    tokio::task::spawn_blocking(move || {
        if check_only {
            check_for_update()
        } else {
            install_update()
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

fn install_update() -> Result<Value, CliError> {
    let updater = build_updater()?;
    let status = updater
        .update()
        .map_err(|update_error| CliError::Network(update_error.to_string()))?;
    Ok(json!({
        "updated": status.updated(),
        "version": status.version(),
    }))
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
}
