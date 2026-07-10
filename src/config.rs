use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::CliError;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    pub default_account: Option<String>,
    #[serde(default)]
    pub accounts: BTreeMap<String, AccountEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AccountEntry {
    pub account_id: String,
    pub flow: AuthFlow,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthFlow {
    M2m,
    AuthCode,
}

impl Config {
    pub fn load(path: &Path) -> Result<Config, CliError> {
        if !path.exists() {
            return Ok(Config::default());
        }
        let raw = std::fs::read_to_string(path).map_err(|read_error| {
            CliError::Usage(format!(
                "cannot read config {}: {read_error}",
                path.display()
            ))
        })?;
        toml::from_str(&raw).map_err(|parse_error| {
            CliError::Usage(format!("invalid config {}: {parse_error}", path.display()))
        })
    }

    pub fn save(&self, path: &Path) -> Result<(), CliError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|io_error| {
                CliError::Usage(format!("cannot create {}: {io_error}", parent.display()))
            })?;
        }
        let raw = toml::to_string_pretty(self).expect("config serializable");
        std::fs::write(path, raw).map_err(|io_error| {
            CliError::Usage(format!("cannot write {}: {io_error}", path.display()))
        })
    }

    pub fn resolve_alias(&self, flag: Option<&str>, env: Option<&str>) -> Result<String, CliError> {
        let alias = flag
            .map(str::to_string)
            .or_else(|| env.map(str::to_string))
            .or_else(|| self.default_account.clone())
            .ok_or_else(|| CliError::Usage(
                "no account selected: pass --account, set NETSUITE_ACCOUNT, or run `netsuite-cli account set-default`".into()))?;
        if !self.accounts.contains_key(&alias) {
            return Err(CliError::Usage(format!(
                "unknown account alias '{alias}'; run `netsuite-cli account list`"
            )));
        }
        Ok(alias)
    }
}

pub fn default_config_path() -> PathBuf {
    directories::ProjectDirs::from("com", "CreativePlanning", "netsuite-cli")
        .expect("resolvable home directory")
        .config_dir()
        .join("config.toml")
}

pub fn default_cache_dir() -> PathBuf {
    directories::ProjectDirs::from("com", "CreativePlanning", "netsuite-cli")
        .expect("resolvable home directory")
        .cache_dir()
        .to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_config() -> Config {
        let mut config = Config::default();
        config.accounts.insert(
            "prod".into(),
            AccountEntry {
                account_id: "1234567".into(),
                flow: AuthFlow::M2m,
            },
        );
        config.accounts.insert(
            "sb1".into(),
            AccountEntry {
                account_id: "1234567_SB1".into(),
                flow: AuthFlow::AuthCode,
            },
        );
        config.default_account = Some("prod".into());
        config
    }

    #[test]
    fn config_round_trips_through_toml_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        sample_config().save(&path).unwrap();
        let loaded = Config::load(&path).unwrap();
        assert_eq!(loaded.default_account.as_deref(), Some("prod"));
        assert_eq!(loaded.accounts["sb1"].account_id, "1234567_SB1");
        assert!(matches!(loaded.accounts["sb1"].flow, AuthFlow::AuthCode));
    }

    #[test]
    fn missing_config_file_loads_as_default() {
        let dir = tempfile::tempdir().unwrap();
        let loaded = Config::load(&dir.path().join("nope.toml")).unwrap();
        assert!(loaded.accounts.is_empty());
    }

    #[test]
    fn alias_resolution_prefers_flag_then_env_then_default() {
        let config = sample_config();
        assert_eq!(
            config.resolve_alias(Some("sb1"), Some("ignored")).unwrap(),
            "sb1"
        );
        assert_eq!(config.resolve_alias(None, Some("sb1")).unwrap(), "sb1");
        assert_eq!(config.resolve_alias(None, None).unwrap(), "prod");
        assert!(matches!(
            config.resolve_alias(Some("nope"), None),
            Err(crate::error::CliError::Usage(_))
        ));
        let empty = Config::default();
        assert!(matches!(
            empty.resolve_alias(None, None),
            Err(crate::error::CliError::Usage(_))
        ));
    }
}
