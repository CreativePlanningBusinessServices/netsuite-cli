use std::path::Path;

use serde_json::{Value, json};

use crate::config::Config;
use crate::error::CliError;

const VALID_KEYS: [&str; 2] = ["default_account", "cache_ttl_hours"];
const DEFAULT_CACHE_TTL_HOURS: u64 = 24;

pub fn get(config_path: &Path, key: Option<&str>) -> Result<Value, CliError> {
    let config = Config::load(config_path)?;
    match key {
        None => Ok(json!({
            "default_account": config.default_account,
            "cache_ttl_hours": effective_cache_ttl_hours(&config),
        })),
        Some("default_account") => Ok(json!({"default_account": config.default_account})),
        Some("cache_ttl_hours") => {
            Ok(json!({"cache_ttl_hours": effective_cache_ttl_hours(&config)}))
        }
        Some(unknown_key) => Err(unknown_key_error(unknown_key)),
    }
}

pub fn set(config_path: &Path, key: &str, value: &str) -> Result<Value, CliError> {
    let mut config = Config::load(config_path)?;
    let result = match key {
        "default_account" => {
            if !config.accounts.contains_key(value) {
                return Err(CliError::Usage(format!(
                    "unknown account alias '{value}'; run `netsuite-cli account list`"
                )));
            }
            config.default_account = Some(value.to_string());
            json!({"default_account": value})
        }
        "cache_ttl_hours" => {
            let hours: u64 = value.parse().map_err(|_| {
                CliError::Usage(format!(
                    "cache_ttl_hours must be a non-negative integer, got '{value}'"
                ))
            })?;
            config.cache_ttl_hours = Some(hours);
            json!({"cache_ttl_hours": hours})
        }
        unknown_key => return Err(unknown_key_error(unknown_key)),
    };
    config.save(config_path)?;
    Ok(result)
}

fn effective_cache_ttl_hours(config: &Config) -> u64 {
    config.cache_ttl_hours.unwrap_or(DEFAULT_CACHE_TTL_HOURS)
}

fn unknown_key_error(key: &str) -> CliError {
    CliError::Usage(format!(
        "unknown config key '{key}'; valid keys are: {}",
        VALID_KEYS.join(", ")
    ))
}
