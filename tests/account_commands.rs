mod common;

use common::client_for;
use netsuite_cli::commands::account;
use netsuite_cli::config::{AuthFlow, Config};
use netsuite_cli::error::CliError;
use netsuite_cli::secrets::{AccountSecrets, MemoryStore, SecretStore, TbaSecrets};
use std::sync::Arc;
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

/// Removes an env var on drop so a panicking assertion between set and remove cannot leak the
/// var into the rest of the (shared) test process.
struct EnvVarGuard {
    name: &'static str,
}

impl EnvVarGuard {
    fn set(name: &'static str, value: &str) -> Self {
        unsafe { std::env::set_var(name, value) };
        EnvVarGuard { name }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        unsafe { std::env::remove_var(self.name) };
    }
}

/// NOTE: env-var cases must run in one test (or use a mutex) — process env is global, and
/// `cargo test`'s parallel test threads would otherwise race each other's set_var/remove_var.
#[tokio::test]
async fn consumer_pair_resolution_prefers_env_then_store() {
    let store = MemoryStore::default();

    // no env, nothing stored, not interactive → Auth error naming the env vars
    let missing = account::resolve_consumer_pair(&store, "demo", false).unwrap_err();
    assert!(matches!(missing, CliError::Auth(_)));
    assert!(
        missing
            .to_string()
            .contains("NETSUITE_CLI_TBA_CONSUMER_KEY")
    );

    // stored pair wins when no env
    store
        .set_tba(
            "demo",
            &TbaSecrets {
                consumer_key: "storedkey".into(),
                consumer_secret: "storedsecret".into(),
                token_id: None,
                token_secret: None,
            },
        )
        .unwrap();
    assert_eq!(
        account::resolve_consumer_pair(&store, "demo", false).unwrap(),
        ("storedkey".to_string(), "storedsecret".to_string())
    );

    // env overrides stored — and file-sourced env vars often carry a trailing newline (e.g.
    // from `export FOO=$(cat secret.txt)`), which must be trimmed just like the prompt path
    // trims, or it would be persisted into the keyring and silently break HMAC signing.
    let _key_guard = EnvVarGuard::set("NETSUITE_CLI_TBA_CONSUMER_KEY", "envkey\n");
    let _secret_guard = EnvVarGuard::set("NETSUITE_CLI_TBA_CONSUMER_SECRET", "envsecret\n");
    assert_eq!(
        account::resolve_consumer_pair(&store, "demo", false).unwrap(),
        ("envkey".to_string(), "envsecret".to_string())
    );

    // While the env guards are still in scope, prove that `soap_auth` persists the
    // env-sourced consumer pair *before* it opens the browser consent flow — not only after
    // the flow succeeds. Account id "0000000" resolves to a restlets.api.netsuite.com
    // subdomain that does not exist, so the request-token POST fails fast on DNS resolution
    // without needing a live NetSuite account or a browser.
    let temp_dir = tempfile::tempdir().unwrap();
    let config_path = temp_dir.path().join("config.toml");
    let mut config = Config::default();
    config.accounts.insert(
        "demo".to_string(),
        netsuite_cli::config::AccountEntry {
            account_id: "0000000".to_string(),
            flow: AuthFlow::M2m,
        },
    );
    config.save(&config_path).unwrap();
    let store: Arc<dyn SecretStore> = Arc::new(store);

    let soap_auth_result = account::soap_auth(&config_path, store.clone(), "demo", 8899, false)
        .await
        .unwrap_err();
    assert!(
        matches!(
            soap_auth_result,
            CliError::Network(_) | CliError::Auth(_) | CliError::Api { .. }
        ),
        "expected the unroutable request-token POST to fail, got {soap_auth_result:?}"
    );

    let persisted = store
        .get_tba("demo")
        .unwrap()
        .expect("consumer pair must be persisted even though the browser consent flow failed");
    assert_eq!(persisted.consumer_key, "envkey");
    assert_eq!(persisted.consumer_secret, "envsecret");
    assert_eq!(
        persisted.token_id, None,
        "no token has been minted yet, so token_id must stay unset"
    );
    assert_eq!(persisted.token_secret, None);

    // Register two more aliases against the same unroutable account id, still under the
    // "envkey"/"envsecret" env guards, to prove the pre-flow persist preserves a previously
    // minted token when the consumer pair is unchanged, and correctly drops it when the pair
    // changed (a token minted under a different consumerSecret is unusable anyway — TBA
    // request signatures are keyed by consumerSecret&tokenSecret).
    config.accounts.insert(
        "demo-same-pair".to_string(),
        netsuite_cli::config::AccountEntry {
            account_id: "0000000".to_string(),
            flow: AuthFlow::M2m,
        },
    );
    config.accounts.insert(
        "demo-diff-pair".to_string(),
        netsuite_cli::config::AccountEntry {
            account_id: "0000000".to_string(),
            flow: AuthFlow::M2m,
        },
    );
    config.save(&config_path).unwrap();

    // Scenario 1: stored pair already matches the resolved (env) pair, and a token was already
    // minted under it. A failed re-auth attempt must not clobber that working token.
    store
        .set_tba(
            "demo-same-pair",
            &TbaSecrets {
                consumer_key: "envkey".into(),
                consumer_secret: "envsecret".into(),
                token_id: Some("existing-token-id".into()),
                token_secret: Some("existing-token-secret".into()),
            },
        )
        .unwrap();

    account::soap_auth(&config_path, store.clone(), "demo-same-pair", 8899, false)
        .await
        .unwrap_err();

    let same_pair_persisted = store
        .get_tba("demo-same-pair")
        .unwrap()
        .expect("consumer pair must remain persisted");
    assert_eq!(same_pair_persisted.consumer_key, "envkey");
    assert_eq!(same_pair_persisted.consumer_secret, "envsecret");
    assert_eq!(
        same_pair_persisted.token_id,
        Some("existing-token-id".to_string()),
        "an unchanged consumer pair must preserve the previously minted token when the retry fails"
    );
    assert_eq!(
        same_pair_persisted.token_secret,
        Some("existing-token-secret".to_string())
    );

    // Scenario 2: stored pair differs from the resolved (env) pair, and a token was minted
    // under that old pair. The env pair wins resolution, and since the pair changed, the old
    // token must be dropped (it cannot be used to sign requests under the new consumer secret).
    store
        .set_tba(
            "demo-diff-pair",
            &TbaSecrets {
                consumer_key: "oldkey".into(),
                consumer_secret: "oldsecret".into(),
                token_id: Some("stale-token-id".into()),
                token_secret: Some("stale-token-secret".into()),
            },
        )
        .unwrap();

    account::soap_auth(&config_path, store.clone(), "demo-diff-pair", 8899, false)
        .await
        .unwrap_err();

    let diff_pair_persisted = store
        .get_tba("demo-diff-pair")
        .unwrap()
        .expect("consumer pair must be persisted");
    assert_eq!(diff_pair_persisted.consumer_key, "envkey");
    assert_eq!(diff_pair_persisted.consumer_secret, "envsecret");
    assert_eq!(
        diff_pair_persisted.token_id, None,
        "a changed consumer pair must drop the stale token minted under the old pair"
    );
    assert_eq!(diff_pair_persisted.token_secret, None);
}

#[tokio::test]
async fn soap_auth_rejects_unknown_alias() {
    let temp_dir = tempfile::tempdir().unwrap();
    let config_path = temp_dir.path().join("config.toml");
    Config::default().save(&config_path).unwrap();
    let store: Arc<dyn SecretStore> = Arc::new(MemoryStore::default());

    let error = account::soap_auth(&config_path, store, "ghost", 8899, false)
        .await
        .unwrap_err();

    match error {
        CliError::Usage(message) => assert!(message.contains("ghost")),
        other => panic!("expected Usage error, got {other:?}"),
    }
}
