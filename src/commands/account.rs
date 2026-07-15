use std::io::IsTerminal;
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::account as domain;
use crate::auth::TokenResponse;
use crate::auth::authcode;
use crate::auth::m2m::{M2mConfig, build_assertion};
use crate::auth::tba;
use crate::client::NsClient;
use crate::config::{AccountEntry, AuthFlow, Config};
use crate::error::CliError;
use crate::secrets::{AccountSecrets, CachedToken, SecretStore, TbaSecrets};

pub fn add_m2m(
    config_path: &Path,
    store: &dyn SecretStore,
    alias: &str,
    account_id: &str,
    client_id: &str,
    cert_id: &str,
    key_path: &Path,
) -> Result<Value, CliError> {
    let private_key_pem = std::fs::read_to_string(key_path).map_err(|read_error| {
        CliError::Usage(format!(
            "cannot read key file {}: {read_error}",
            key_path.display()
        ))
    })?;
    validate_m2m_key(client_id, cert_id, &private_key_pem)?;

    let secrets = AccountSecrets::M2m {
        client_id: client_id.to_string(),
        cert_id: cert_id.to_string(),
        private_key_pem,
    };
    let serialized_secrets = serde_json::to_string(&secrets).expect("serializable");
    if cfg!(windows) && exceeds_windows_credential_limit(serialized_secrets.len()) {
        return Err(CliError::Usage(format!(
            "this M2M credential is {} bytes serialized, which exceeds Windows Credential \
             Manager's ~2560-byte (UTF-16) blob limit; use an EC P-256 key instead of RSA \
             (see README's M2M certificate section)",
            serialized_secrets.len()
        )));
    }

    // Write the config entry before touching the keychain: if the secrets write below fails,
    // the config still points at an alias with no stored credentials, which is a self-describing
    // and re-runnable state ("no credentials stored for '<alias>'; run account add"). The
    // reverse order can leave secrets under an alias the config never learns about.
    write_account_entry(config_path, alias, account_id, AuthFlow::M2m)?;
    store.set(alias, &secrets)?;
    // Re-registering an alias must not leave a stale cached bearer token from the previous
    // credentials being served until it expires or 401s.
    store.delete_token(alias)?;
    Ok(json!({"alias": alias, "accountId": account_id, "flow": "m2m"}))
}

/// Windows Credential Manager (the backend `keyring` uses on Windows) caps blobs at 2560 bytes
/// as UTF-16, i.e. 2 bytes per code unit for ASCII, so ~1280 ASCII chars. Serialized
/// `AccountSecrets` JSON is ASCII/UTF-8, so one byte is one UTF-16 code unit here; we use 1250
/// for margin. That catches RSA-4096 PEMs (~3.4KB serialized) well before they'd hit the real
/// ceiling, while EC P-256 PEMs stay clear. Pure and platform-independent so it's testable on
/// every target; callers gate the resulting error on `cfg!(windows)` since macOS Keychain and
/// Linux secret-service have no such limit.
fn exceeds_windows_credential_limit(serialized_len: usize) -> bool {
    const WINDOWS_CREDENTIAL_BLOB_LIMIT_BYTES: usize = 1250;
    serialized_len > WINDOWS_CREDENTIAL_BLOB_LIMIT_BYTES
}

pub async fn add_auth_code(
    config_path: &Path,
    store: Arc<dyn SecretStore>,
    alias: &str,
    account_id: &str,
    client_id: &str,
    port: u16,
    paste_mode: bool,
) -> Result<Value, CliError> {
    let http = reqwest::Client::new();
    let app_base = domain::app_base(account_id);
    let token_url = format!(
        "{}/services/rest/auth/oauth2/v1/token",
        domain::rest_base(account_id)
    );
    let token =
        authcode::run_login_flow(&http, &app_base, &token_url, client_id, port, paste_mode).await?;
    store_auth_code_account(
        config_path,
        store.as_ref(),
        alias,
        account_id,
        client_id,
        &token,
    )
}

/// Mints a never-expiring SOAP (TBA) token via browser consent and stores it alongside the
/// consumer pair used to obtain it, so a later `soap_auth` re-run (or the SOAP client itself)
/// can find both without prompting again.
pub async fn soap_auth(
    config_path: &Path,
    store: Arc<dyn SecretStore>,
    alias: &str,
    port: u16,
    paste_mode: bool,
) -> Result<Value, CliError> {
    let config = Config::load(config_path)?;
    let entry = config.accounts.get(alias).ok_or_else(|| {
        CliError::Usage(format!(
            "unknown account alias '{alias}'; run `netsuite-cli account list`"
        ))
    })?;
    let (consumer_key, consumer_secret) =
        resolve_consumer_pair(store.as_ref(), alias, std::io::stdin().is_terminal())?;
    // Persist the consumer pair before opening the browser: if the user abandons or fails the
    // consent flow, a retry finds the pair already stored instead of losing a prompted secret
    // and being forced to re-enter it. If a token was already minted under this exact same
    // consumer pair, keep it — a failed re-auth attempt must not destroy a working SOAP token.
    // A token minted under a *different* pair is unusable regardless (TBA request signatures
    // are keyed by consumerSecret&tokenSecret), so it is correctly dropped in that case.
    let existing_tba = store.get_tba(alias)?;
    let (preserved_token_id, preserved_token_secret) = match &existing_tba {
        Some(tba) if tba.consumer_key == consumer_key && tba.consumer_secret == consumer_secret => {
            (tba.token_id.clone(), tba.token_secret.clone())
        }
        _ => (None, None),
    };
    store.set_tba(
        alias,
        &TbaSecrets {
            consumer_key: consumer_key.clone(),
            consumer_secret: consumer_secret.clone(),
            token_id: preserved_token_id,
            token_secret: preserved_token_secret,
        },
    )?;
    let http = reqwest::Client::new();
    let minted = tba::run_tba_flow(
        &http,
        &domain::restlet_base(&entry.account_id),
        &domain::app_base(&entry.account_id),
        &consumer_key,
        &consumer_secret,
        port,
        paste_mode,
    )
    .await?;
    store.set_tba(
        alias,
        &TbaSecrets {
            consumer_key,
            consumer_secret,
            token_id: Some(minted.token_id),
            token_secret: Some(minted.token_secret),
        },
    )?;
    Ok(json!({"alias": alias, "accountId": entry.account_id, "soapTokenStored": true}))
}

/// Resolution order: env vars (for CI/non-interactive use) → previously stored TBA consumer
/// pair for this alias → interactive prompt (hidden for the secret). The consumer secret must
/// never be accepted as a CLI flag — that would leak it into shell history and `ps` output.
/// `interactive` (whether stdin is a TTY) is a parameter rather than checked here so tests can
/// exercise the non-interactive error branch deterministically instead of hanging on the prompt
/// when run from a real terminal.
pub fn resolve_consumer_pair(
    store: &dyn SecretStore,
    alias: &str,
    interactive: bool,
) -> Result<(String, String), CliError> {
    if let (Ok(env_key), Ok(env_secret)) = (
        std::env::var("NETSUITE_CLI_TBA_CONSUMER_KEY"),
        std::env::var("NETSUITE_CLI_TBA_CONSUMER_SECRET"),
    ) {
        return Ok((env_key, env_secret));
    }
    if let Some(stored) = store.get_tba(alias)? {
        return Ok((stored.consumer_key, stored.consumer_secret));
    }
    if !interactive {
        return Err(CliError::Auth(format!(
            "no TBA consumer credentials for '{alias}': set NETSUITE_CLI_TBA_CONSUMER_KEY and \
             NETSUITE_CLI_TBA_CONSUMER_SECRET, or run `netsuite-cli account soap-auth {alias}` \
             in an interactive terminal"
        )));
    }
    eprint!("Integration record consumer key (client id): ");
    let mut consumer_key = String::new();
    std::io::stdin()
        .read_line(&mut consumer_key)
        .map_err(|read_error| {
            CliError::Auth(format!("failed to read consumer key: {read_error}"))
        })?;
    let consumer_secret =
        rpassword::prompt_password("Integration record consumer secret (hidden): ").map_err(
            |read_error| CliError::Auth(format!("failed to read consumer secret: {read_error}")),
        )?;
    Ok((
        consumer_key.trim().to_string(),
        consumer_secret.trim().to_string(),
    ))
}

pub fn list(config_path: &Path) -> Result<Value, CliError> {
    let config = Config::load(config_path)?;
    let accounts: Vec<Value> = config
        .accounts
        .iter()
        .map(|(alias, entry)| {
            json!({
                "alias": alias,
                "accountId": entry.account_id,
                "flow": serde_json::to_value(entry.flow).expect("flow is serializable"),
                "default": config.default_account.as_deref() == Some(alias.as_str()),
            })
        })
        .collect();
    Ok(json!({"accounts": accounts}))
}

pub fn remove(config_path: &Path, store: &dyn SecretStore, alias: &str) -> Result<Value, CliError> {
    let mut config = Config::load(config_path)?;
    if config.accounts.remove(alias).is_none() {
        return Err(CliError::Usage(format!(
            "unknown account alias '{alias}'; run `netsuite-cli account list`"
        )));
    }
    if config.default_account.as_deref() == Some(alias) {
        config.default_account = None;
    }
    // Save the config removal before deleting keychain secrets: if the secrets delete below
    // fails after this, the alias is already gone from config (invisible to the CLI, and a
    // future `account add` for the same alias just overwrites the orphaned keychain entry).
    // The reverse order can leave the alias in config with no secrets behind it, which then
    // surfaces as a confusing "no credentials stored" error on unrelated commands later.
    config.save(config_path)?;
    store.delete(alias)?;
    Ok(json!({"removed": alias}))
}

pub fn set_default(config_path: &Path, alias: &str) -> Result<Value, CliError> {
    let mut config = Config::load(config_path)?;
    if !config.accounts.contains_key(alias) {
        return Err(CliError::Usage(format!(
            "unknown account alias '{alias}'; run `netsuite-cli account list`"
        )));
    }
    config.default_account = Some(alias.to_string());
    config.save(config_path)?;
    Ok(json!({"default": alias}))
}

pub async fn test(client: &NsClient, alias: &str) -> Result<Value, CliError> {
    client
        .request(
            reqwest::Method::GET,
            "/services/rest/record/v1/metadata-catalog",
            &[("select", "customer".to_string())],
            &[("Accept", "application/json")],
            None,
        )
        .await?;
    Ok(json!({"alias": alias, "ok": true}))
}

/// Rejects a bad key at add time by attempting to sign a throwaway assertion with it,
/// rather than waiting for the first real token request to fail.
fn validate_m2m_key(client_id: &str, cert_id: &str, private_key_pem: &str) -> Result<(), CliError> {
    let throwaway_config = M2mConfig {
        token_url: "https://validate.invalid/services/rest/auth/oauth2/v1/token".into(),
        client_id: client_id.to_string(),
        cert_id: cert_id.to_string(),
        private_key_pem: private_key_pem.to_string(),
        scopes: vec!["rest_webservices".into()],
    };
    build_assertion(&throwaway_config, 0).map(|_assertion| ())
}

fn write_account_entry(
    config_path: &Path,
    alias: &str,
    account_id: &str,
    flow: AuthFlow,
) -> Result<(), CliError> {
    let mut config = Config::load(config_path)?;
    config.accounts.insert(
        alias.to_string(),
        AccountEntry {
            account_id: account_id.to_string(),
            flow,
        },
    );
    if config.default_account.is_none() {
        config.default_account = Some(alias.to_string());
    }
    config.save(config_path)
}

/// Pure config/secret-store persistence for an already-obtained auth-code token. Split out
/// from `add_auth_code` so it can be unit tested without running the real browser login flow.
fn store_auth_code_account(
    config_path: &Path,
    store: &dyn SecretStore,
    alias: &str,
    account_id: &str,
    client_id: &str,
    token: &TokenResponse,
) -> Result<Value, CliError> {
    // Write the config entry before touching the keychain: if the secrets write below fails,
    // the config still points at an alias with no stored credentials, which is a self-describing
    // and re-runnable state ("no credentials stored for '<alias>'; run account add"). The
    // reverse order can leave secrets under an alias the config never learns about.
    write_account_entry(config_path, alias, account_id, AuthFlow::AuthCode)?;
    store.set(
        alias,
        &AccountSecrets::AuthCode {
            client_id: client_id.to_string(),
            refresh_token: token.refresh_token.clone(),
        },
    )?;
    // Re-registering an alias must not leave a stale cached bearer token from the previous
    // credentials around; the fresh token is set right below, but clear first for consistency
    // with add_m2m and in case set_token below is ever skipped or fails.
    store.delete_token(alias)?;
    let now_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    store.set_token(
        alias,
        &CachedToken {
            access_token: token.access_token.clone(),
            expires_at_epoch: now_epoch + token.expires_in,
        },
    )?;
    Ok(json!({"alias": alias, "accountId": account_id, "flow": "auth-code"}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::MemoryStore;

    /// Always fails `set`, to prove the config entry is written before the keychain write is
    /// attempted — so a keychain failure leaves a self-describing, re-runnable state instead of
    /// orphaned secrets under an alias the config never learns about.
    #[derive(Default)]
    struct FailingSecretStore;

    impl SecretStore for FailingSecretStore {
        fn get(&self, _alias: &str) -> Result<Option<AccountSecrets>, CliError> {
            Ok(None)
        }
        fn set(&self, _alias: &str, _secrets: &AccountSecrets) -> Result<(), CliError> {
            Err(CliError::Auth("keychain unavailable".into()))
        }
        fn delete(&self, _alias: &str) -> Result<(), CliError> {
            Ok(())
        }
        fn get_token(&self, _alias: &str) -> Result<Option<CachedToken>, CliError> {
            Ok(None)
        }
        fn set_token(&self, _alias: &str, _token: &CachedToken) -> Result<(), CliError> {
            Ok(())
        }
        fn delete_token(&self, _alias: &str) -> Result<(), CliError> {
            Ok(())
        }
        fn get_tba(&self, _alias: &str) -> Result<Option<crate::secrets::TbaSecrets>, CliError> {
            Ok(None)
        }
        fn set_tba(
            &self,
            _alias: &str,
            _secrets: &crate::secrets::TbaSecrets,
        ) -> Result<(), CliError> {
            Ok(())
        }
        fn delete_tba(&self, _alias: &str) -> Result<(), CliError> {
            Ok(())
        }
    }

    #[test]
    fn add_m2m_writes_config_entry_before_keychain_secrets() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("config.toml");
        let key_path = temp_dir.path().join("key.pem");
        let key_pem = rcgen::KeyPair::generate().unwrap().serialize_pem();
        std::fs::write(&key_path, &key_pem).unwrap();
        let store = FailingSecretStore;

        let error = add_m2m(
            &config_path,
            &store,
            "prod",
            "1234567",
            "CID",
            "KID",
            &key_path,
        )
        .unwrap_err();

        assert!(matches!(error, CliError::Auth(_)));
        let config = Config::load(&config_path).unwrap();
        assert_eq!(
            config.accounts["prod"].account_id, "1234567",
            "config entry must be written even though the secrets write failed"
        );
    }

    #[test]
    fn store_auth_code_account_writes_config_entry_before_keychain_secrets() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("config.toml");
        let store = FailingSecretStore;
        let token = TokenResponse {
            access_token: "ACCESS1".into(),
            expires_in: 3600,
            refresh_token: Some("REFRESH1".into()),
        };

        let error =
            store_auth_code_account(&config_path, &store, "dev", "1234567_SB1", "CID", &token)
                .unwrap_err();

        assert!(matches!(error, CliError::Auth(_)));
        let config = Config::load(&config_path).unwrap();
        assert_eq!(
            config.accounts["dev"].account_id, "1234567_SB1",
            "config entry must be written even though the secrets write failed"
        );
    }

    #[test]
    fn store_auth_code_account_persists_secrets_token_and_config() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("config.toml");
        let store = MemoryStore::default();
        let token = TokenResponse {
            access_token: "ACCESS1".into(),
            expires_in: 3600,
            refresh_token: Some("REFRESH1".into()),
        };

        let result =
            store_auth_code_account(&config_path, &store, "dev", "1234567_SB1", "CID", &token)
                .unwrap();
        assert_eq!(
            result,
            json!({"alias": "dev", "accountId": "1234567_SB1", "flow": "auth-code"})
        );

        match store.get("dev").unwrap().expect("secrets stored") {
            AccountSecrets::AuthCode {
                client_id,
                refresh_token,
            } => {
                assert_eq!(client_id, "CID");
                assert_eq!(refresh_token.as_deref(), Some("REFRESH1"));
            }
            other => panic!("wrong variant: {other:?}"),
        }
        assert_eq!(
            store.get_token("dev").unwrap().unwrap().access_token,
            "ACCESS1"
        );

        let config = Config::load(&config_path).unwrap();
        assert_eq!(config.accounts["dev"].account_id, "1234567_SB1");
        assert!(matches!(config.accounts["dev"].flow, AuthFlow::AuthCode));
        assert_eq!(config.default_account.as_deref(), Some("dev"));
    }

    #[test]
    fn store_auth_code_account_does_not_override_existing_default() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("config.toml");
        let store = MemoryStore::default();
        write_account_entry(&config_path, "prod", "1234567", AuthFlow::M2m).unwrap();

        let token = TokenResponse {
            access_token: "ACCESS2".into(),
            expires_in: 3600,
            refresh_token: Some("REFRESH2".into()),
        };
        store_auth_code_account(&config_path, &store, "dev", "1234567_SB1", "CID", &token).unwrap();

        let config = Config::load(&config_path).unwrap();
        assert_eq!(config.default_account.as_deref(), Some("prod"));
    }

    #[test]
    fn exceeds_windows_credential_limit_thresholds_at_1250_bytes() {
        assert!(!exceeds_windows_credential_limit(1250));
        assert!(exceeds_windows_credential_limit(1251));
        assert!(!exceeds_windows_credential_limit(0));
    }

    /// A real (throwaway, never used against any NetSuite account) RSA-4096 PKCS8 key so
    /// `validate_m2m_key`'s signing check passes and the test reaches the size pre-check.
    /// Serializes to well over the 2500-byte threshold once wrapped in AccountSecrets JSON.
    const OVERSIZED_RSA_4096_PEM: &str = "-----BEGIN PRIVATE KEY-----\n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\ngznu7ktsVpeg+gKQidVqlgpLTXeaA4E=\n-----END PRIVATE KEY-----\n";

    #[test]
    #[cfg(windows)]
    fn add_m2m_rejects_oversized_rsa_key_on_windows() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("config.toml");
        let key_path = temp_dir.path().join("key.pem");
        std::fs::write(&key_path, OVERSIZED_RSA_4096_PEM).unwrap();
        let store = MemoryStore::default();

        let error = add_m2m(
            &config_path,
            &store,
            "prod",
            "1234567",
            "CID",
            "KID",
            &key_path,
        )
        .unwrap_err();

        match error {
            CliError::Usage(message) => {
                assert!(message.contains("Windows Credential Manager"));
                assert!(message.contains("EC P-256"));
            }
            other => panic!("expected Usage error, got {other:?}"),
        }
        assert!(
            store.get("prod").unwrap().is_none(),
            "oversized secrets must not be persisted"
        );
    }

    #[test]
    fn add_m2m_clears_stale_cached_token_on_reregister() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("config.toml");
        let key_path = temp_dir.path().join("key.pem");
        let key_pem = rcgen::KeyPair::generate().unwrap().serialize_pem();
        std::fs::write(&key_path, &key_pem).unwrap();
        let store = MemoryStore::default();
        store
            .set_token(
                "prod",
                &CachedToken {
                    access_token: "STALE".into(),
                    expires_at_epoch: u64::MAX,
                },
            )
            .unwrap();

        add_m2m(
            &config_path,
            &store,
            "prod",
            "1234567",
            "CID",
            "KID",
            &key_path,
        )
        .unwrap();

        assert!(
            store.get_token("prod").unwrap().is_none(),
            "stale cached token must be cleared when re-registering an alias"
        );
    }

    #[test]
    #[cfg(not(windows))]
    fn add_m2m_allows_oversized_rsa_key_off_windows() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("config.toml");
        let key_path = temp_dir.path().join("key.pem");
        std::fs::write(&key_path, OVERSIZED_RSA_4096_PEM).unwrap();
        let store = MemoryStore::default();

        add_m2m(
            &config_path,
            &store,
            "prod",
            "1234567",
            "CID",
            "KID",
            &key_path,
        )
        .unwrap();

        assert!(store.get("prod").unwrap().is_some());
    }
}
