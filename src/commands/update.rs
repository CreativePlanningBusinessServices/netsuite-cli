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
    Ok(json!({
        "current": current_version,
        "latest": latest_release.version,
        "updateAvailable": latest_release.version != current_version,
    }))
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
