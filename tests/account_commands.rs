mod common;

use common::client_for;
use netsuite_cli::commands::account;
use netsuite_cli::config::{AuthFlow, Config};
use netsuite_cli::error::CliError;
use netsuite_cli::secrets::{AccountSecrets, MemoryStore, SecretStore};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn key_pem() -> String {
    rcgen::KeyPair::generate().unwrap().serialize_pem()
}

#[test]
fn add_m2m_stores_key_in_secret_store_and_config_entry() {
    let temp_dir = tempfile::tempdir().unwrap();
    let config_path = temp_dir.path().join("config.toml");
    let key_path = temp_dir.path().join("netsuite-m2m.pem");
    let pem = key_pem();
    std::fs::write(&key_path, &pem).unwrap();
    let store = MemoryStore::default();

    let result = account::add_m2m(
        &config_path,
        &store,
        "prod",
        "1234567",
        "CID",
        "KID",
        &key_path,
    )
    .unwrap();
    assert_eq!(
        result,
        serde_json::json!({"alias": "prod", "accountId": "1234567", "flow": "m2m"})
    );

    match store.get("prod").unwrap().expect("secrets stored") {
        AccountSecrets::M2m {
            client_id,
            cert_id,
            private_key_pem,
        } => {
            assert_eq!(client_id, "CID");
            assert_eq!(cert_id, "KID");
            assert_eq!(private_key_pem, pem);
        }
        other => panic!("wrong variant: {other:?}"),
    }

    let config = Config::load(&config_path).unwrap();
    assert_eq!(config.accounts["prod"].account_id, "1234567");
    assert!(matches!(config.accounts["prod"].flow, AuthFlow::M2m));
    // first added account becomes the default
    assert_eq!(config.default_account.as_deref(), Some("prod"));
}

#[test]
fn add_m2m_rejects_unparseable_key_without_storing_anything() {
    let temp_dir = tempfile::tempdir().unwrap();
    let config_path = temp_dir.path().join("config.toml");
    let key_path = temp_dir.path().join("bad.pem");
    std::fs::write(&key_path, "not a pem").unwrap();
    let store = MemoryStore::default();

    let add_result = account::add_m2m(
        &config_path,
        &store,
        "prod",
        "1234567",
        "CID",
        "KID",
        &key_path,
    );
    assert!(matches!(add_result, Err(CliError::Auth(_))));
    assert!(store.get("prod").unwrap().is_none());
    assert!(!config_path.exists());
}

#[test]
fn add_m2m_on_existing_alias_overwrites_secrets_and_config() {
    let temp_dir = tempfile::tempdir().unwrap();
    let config_path = temp_dir.path().join("config.toml");
    let key_path = temp_dir.path().join("netsuite-m2m.pem");
    std::fs::write(&key_path, key_pem()).unwrap();
    let store = MemoryStore::default();

    account::add_m2m(
        &config_path,
        &store,
        "prod",
        "1234567",
        "CID_OLD",
        "KID_OLD",
        &key_path,
    )
    .unwrap();
    account::add_m2m(
        &config_path,
        &store,
        "prod",
        "7654321",
        "CID_NEW",
        "KID_NEW",
        &key_path,
    )
    .unwrap();

    let config = Config::load(&config_path).unwrap();
    assert_eq!(config.accounts.len(), 1);
    assert_eq!(config.accounts["prod"].account_id, "7654321");
    match store.get("prod").unwrap().unwrap() {
        AccountSecrets::M2m {
            client_id, cert_id, ..
        } => {
            assert_eq!(client_id, "CID_NEW");
            assert_eq!(cert_id, "KID_NEW");
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn second_added_account_does_not_become_default() {
    let temp_dir = tempfile::tempdir().unwrap();
    let config_path = temp_dir.path().join("config.toml");
    let key_path = temp_dir.path().join("netsuite-m2m.pem");
    std::fs::write(&key_path, key_pem()).unwrap();
    let store = MemoryStore::default();

    account::add_m2m(&config_path, &store, "prod", "1", "CID", "KID", &key_path).unwrap();
    account::add_m2m(&config_path, &store, "dev", "2", "CID", "KID", &key_path).unwrap();

    let config = Config::load(&config_path).unwrap();
    assert_eq!(config.default_account.as_deref(), Some("prod"));
}

#[test]
fn list_returns_accounts_without_secrets_and_marks_default() {
    let temp_dir = tempfile::tempdir().unwrap();
    let config_path = temp_dir.path().join("config.toml");
    let key_path = temp_dir.path().join("netsuite-m2m.pem");
    std::fs::write(&key_path, key_pem()).unwrap();
    let store = MemoryStore::default();
    account::add_m2m(
        &config_path,
        &store,
        "prod",
        "1234567",
        "CID",
        "KID",
        &key_path,
    )
    .unwrap();

    let listing = account::list(&config_path).unwrap();
    let accounts = listing["accounts"].as_array().unwrap();
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0]["alias"], "prod");
    assert_eq!(accounts[0]["accountId"], "1234567");
    assert_eq!(accounts[0]["flow"], "m2m");
    assert_eq!(accounts[0]["default"], true);
    assert!(accounts[0].get("clientId").is_none());
    assert!(accounts[0].get("privateKeyPem").is_none());
    assert!(!listing.to_string().contains("CID"));
}

#[test]
fn list_on_empty_config_returns_empty_array() {
    let temp_dir = tempfile::tempdir().unwrap();
    let config_path = temp_dir.path().join("nope.toml");
    let listing = account::list(&config_path).unwrap();
    assert_eq!(listing, serde_json::json!({"accounts": []}));
}

#[test]
fn remove_deletes_config_entry_and_secrets_and_clears_default() {
    let temp_dir = tempfile::tempdir().unwrap();
    let config_path = temp_dir.path().join("config.toml");
    let key_path = temp_dir.path().join("netsuite-m2m.pem");
    std::fs::write(&key_path, key_pem()).unwrap();
    let store = MemoryStore::default();
    account::add_m2m(
        &config_path,
        &store,
        "prod",
        "1234567",
        "CID",
        "KID",
        &key_path,
    )
    .unwrap();

    account::remove(&config_path, &store, "prod").unwrap();

    let config = Config::load(&config_path).unwrap();
    assert!(!config.accounts.contains_key("prod"));
    assert_eq!(config.default_account, None);
    assert!(store.get("prod").unwrap().is_none());
}

#[test]
fn remove_of_non_default_alias_leaves_default_untouched() {
    let temp_dir = tempfile::tempdir().unwrap();
    let config_path = temp_dir.path().join("config.toml");
    let key_path = temp_dir.path().join("netsuite-m2m.pem");
    std::fs::write(&key_path, key_pem()).unwrap();
    let store = MemoryStore::default();
    account::add_m2m(&config_path, &store, "prod", "1", "CID", "KID", &key_path).unwrap();
    account::add_m2m(&config_path, &store, "dev", "2", "CID", "KID", &key_path).unwrap();

    account::remove(&config_path, &store, "dev").unwrap();

    let config = Config::load(&config_path).unwrap();
    assert_eq!(config.default_account.as_deref(), Some("prod"));
}

#[test]
fn remove_unknown_alias_is_usage_error() {
    let temp_dir = tempfile::tempdir().unwrap();
    let config_path = temp_dir.path().join("config.toml");
    let store = MemoryStore::default();
    let remove_result = account::remove(&config_path, &store, "nope");
    assert!(matches!(remove_result, Err(CliError::Usage(_))));
}

/// Proves the config removal is saved before the keychain delete is attempted: if the config
/// write fails, the alias must still resolve and its secrets must be untouched, rather than
/// being deleted from the keychain under an alias config still thinks exists.
#[test]
#[cfg(unix)]
fn remove_leaves_alias_and_secrets_intact_when_config_save_fails() {
    use std::os::unix::fs::PermissionsExt;

    let temp_dir = tempfile::tempdir().unwrap();
    let config_path = temp_dir.path().join("config.toml");
    let key_path = temp_dir.path().join("netsuite-m2m.pem");
    std::fs::write(&key_path, key_pem()).unwrap();
    let store = MemoryStore::default();
    account::add_m2m(
        &config_path,
        &store,
        "prod",
        "1234567",
        "CID",
        "KID",
        &key_path,
    )
    .unwrap();

    // Make the config file read-only so `Config::save` fails inside `remove`, without touching
    // permissions on other files in the temp dir (e.g. the key file already read above).
    let original_permissions = std::fs::metadata(&config_path).unwrap().permissions();
    std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o444)).unwrap();

    let remove_result = account::remove(&config_path, &store, "prod");

    // Restore write permission before any assertion can panic and skip cleanup.
    std::fs::set_permissions(&config_path, original_permissions).unwrap();

    assert!(
        matches!(remove_result, Err(CliError::Usage(_))),
        "expected a config-save Usage error, got {remove_result:?}"
    );
    let config = Config::load(&config_path).unwrap();
    assert!(
        config.accounts.contains_key("prod"),
        "alias must still resolve after a failed config save"
    );
    assert!(
        store.get("prod").unwrap().is_some(),
        "secrets must be untouched when config save fails before the keychain delete"
    );
}

#[test]
fn set_default_validates_alias_exists() {
    let temp_dir = tempfile::tempdir().unwrap();
    let config_path = temp_dir.path().join("config.toml");
    let key_path = temp_dir.path().join("netsuite-m2m.pem");
    std::fs::write(&key_path, key_pem()).unwrap();
    let store = MemoryStore::default();
    account::add_m2m(&config_path, &store, "prod", "1", "CID", "KID", &key_path).unwrap();
    account::add_m2m(&config_path, &store, "dev", "2", "CID", "KID", &key_path).unwrap();

    account::set_default(&config_path, "dev").unwrap();
    let config = Config::load(&config_path).unwrap();
    assert_eq!(config.default_account.as_deref(), Some("dev"));

    let set_default_result = account::set_default(&config_path, "nope");
    assert!(matches!(set_default_result, Err(CliError::Usage(_))));
}

#[tokio::test]
async fn test_calls_metadata_catalog_with_customer_select_and_returns_ok() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/services/rest/record/v1/metadata-catalog"))
        .and(query_param("select", "customer"))
        .and(header("Accept", "application/json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "items": [{"name": "customer", "links": []}]
        })))
        .mount(&server)
        .await;

    let result = account::test(&client_for(&server), "prod").await.unwrap();
    assert_eq!(result, serde_json::json!({"alias": "prod", "ok": true}));
}

#[tokio::test]
async fn test_propagates_api_error_on_failure() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/services/rest/record/v1/metadata-catalog"))
        .respond_with(ResponseTemplate::new(400))
        .mount(&server)
        .await;

    let test_result = account::test(&client_for(&server), "prod").await;
    assert!(matches!(test_result, Err(CliError::Api { .. })));
}
