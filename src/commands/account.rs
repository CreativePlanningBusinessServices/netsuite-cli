use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::account as domain;
use crate::auth::TokenResponse;
use crate::auth::authcode;
use crate::auth::m2m::{M2mConfig, build_assertion};
use crate::client::NsClient;
use crate::config::{AccountEntry, AuthFlow, Config};
use crate::error::CliError;
use crate::secrets::{AccountSecrets, CachedToken, SecretStore};

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

    store.set(
        alias,
        &AccountSecrets::M2m {
            client_id: client_id.to_string(),
            cert_id: cert_id.to_string(),
            private_key_pem,
        },
    )?;
    write_account_entry(config_path, alias, account_id, AuthFlow::M2m)?;
    Ok(json!({"alias": alias, "accountId": account_id, "flow": "m2m"}))
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
    store.delete(alias)?;
    if config.default_account.as_deref() == Some(alias) {
        config.default_account = None;
    }
    config.save(config_path)?;
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
    store.set(
        alias,
        &AccountSecrets::AuthCode {
            client_id: client_id.to_string(),
            refresh_token: token.refresh_token.clone(),
        },
    )?;
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
    write_account_entry(config_path, alias, account_id, AuthFlow::AuthCode)?;
    Ok(json!({"alias": alias, "accountId": account_id, "flow": "auth-code"}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::MemoryStore;

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
}
