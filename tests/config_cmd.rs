use netsuite_cli::commands::config_cmd;
use netsuite_cli::config::{AccountEntry, AuthFlow, Config};
use netsuite_cli::error::CliError;

fn config_with_alias(alias: &str) -> Config {
    let mut config = Config::default();
    config.accounts.insert(
        alias.to_string(),
        AccountEntry {
            account_id: "1234567".into(),
            flow: AuthFlow::M2m,
        },
    );
    config
}

#[test]
fn set_cache_ttl_hours_then_get_returns_it() {
    let temp_dir = tempfile::tempdir().unwrap();
    let config_path = temp_dir.path().join("config.toml");
    Config::default().save(&config_path).unwrap();

    let set_result = config_cmd::set(&config_path, "cache_ttl_hours", "48").unwrap();
    assert_eq!(set_result, serde_json::json!({"cache_ttl_hours": 48}));

    let get_result = config_cmd::get(&config_path, Some("cache_ttl_hours")).unwrap();
    assert_eq!(get_result, serde_json::json!({"cache_ttl_hours": 48}));
}

#[test]
fn get_cache_ttl_hours_defaults_to_24_when_unset() {
    let temp_dir = tempfile::tempdir().unwrap();
    let config_path = temp_dir.path().join("config.toml");
    Config::default().save(&config_path).unwrap();

    let get_result = config_cmd::get(&config_path, Some("cache_ttl_hours")).unwrap();
    assert_eq!(get_result, serde_json::json!({"cache_ttl_hours": 24}));
}

#[test]
fn get_with_no_key_returns_full_effective_config() {
    let temp_dir = tempfile::tempdir().unwrap();
    let config_path = temp_dir.path().join("config.toml");
    let mut config = config_with_alias("prod");
    config.default_account = Some("prod".into());
    config.save(&config_path).unwrap();

    let get_result = config_cmd::get(&config_path, None).unwrap();
    assert_eq!(
        get_result,
        serde_json::json!({"default_account": "prod", "cache_ttl_hours": 24})
    );
}

#[test]
fn get_unknown_key_is_usage_error_listing_valid_keys() {
    let temp_dir = tempfile::tempdir().unwrap();
    let config_path = temp_dir.path().join("config.toml");
    Config::default().save(&config_path).unwrap();

    let error = config_cmd::get(&config_path, Some("bogus")).unwrap_err();
    match error {
        CliError::Usage(message) => {
            assert!(message.contains("bogus"));
            assert!(message.contains("default_account"));
            assert!(message.contains("cache_ttl_hours"));
        }
        other => panic!("expected Usage error, got {other:?}"),
    }
}

#[test]
fn set_unknown_key_is_usage_error_listing_valid_keys() {
    let temp_dir = tempfile::tempdir().unwrap();
    let config_path = temp_dir.path().join("config.toml");
    Config::default().save(&config_path).unwrap();

    let error = config_cmd::set(&config_path, "bogus", "1").unwrap_err();
    match error {
        CliError::Usage(message) => {
            assert!(message.contains("bogus"));
            assert!(message.contains("default_account"));
            assert!(message.contains("cache_ttl_hours"));
        }
        other => panic!("expected Usage error, got {other:?}"),
    }
}

#[test]
fn set_default_account_validates_alias_exists() {
    let temp_dir = tempfile::tempdir().unwrap();
    let config_path = temp_dir.path().join("config.toml");
    config_with_alias("prod").save(&config_path).unwrap();

    let error = config_cmd::set(&config_path, "default_account", "nope").unwrap_err();
    assert!(matches!(error, CliError::Usage(_)));

    let set_result = config_cmd::set(&config_path, "default_account", "prod").unwrap();
    assert_eq!(set_result, serde_json::json!({"default_account": "prod"}));
    let loaded = Config::load(&config_path).unwrap();
    assert_eq!(loaded.default_account.as_deref(), Some("prod"));
}

#[test]
fn set_cache_ttl_hours_rejects_non_integer_value() {
    let temp_dir = tempfile::tempdir().unwrap();
    let config_path = temp_dir.path().join("config.toml");
    Config::default().save(&config_path).unwrap();

    let error = config_cmd::set(&config_path, "cache_ttl_hours", "not-a-number").unwrap_err();
    assert!(matches!(error, CliError::Usage(_)));
}
