use std::sync::Arc;

use crate::account;
use crate::auth::TokenProvider;
use crate::auth::authcode::AuthCodeProvider;
use crate::auth::m2m::{M2mConfig, M2mProvider};
use crate::client::NsClient;
use crate::config::{AuthFlow, Config};
use crate::error::CliError;
use crate::secrets::{AccountSecrets, KeyringStore, SecretStore};

pub const M2M_DEFAULT_SCOPES: &[&str] = &["rest_webservices", "restlets"];

pub struct AccountContext {
    pub alias: String,
    pub account_id: String,
    pub client: NsClient,
    pub restlet_base: String,
    /// Entity/role internal ids recorded at auth-code login (see `config::AccountEntry`);
    /// `None` for M2M accounts and accounts added before they were captured. Carried here so
    /// `account cert upload` can default its mapping without re-reading the config file.
    pub entity_id: Option<String>,
    pub role_id: Option<String>,
}

pub fn context_for(alias_flag: Option<&str>) -> Result<AccountContext, CliError> {
    let config = Config::load(&crate::config::default_config_path())?;
    let env_alias = std::env::var("NETSUITE_ACCOUNT").ok();
    let store: Arc<dyn SecretStore> = Arc::new(KeyringStore);
    context_from(&config, alias_flag, env_alias.as_deref(), store)
}

/// Core of `context_for` with the config and secret store injected, so the alias resolution
/// and the copy of the entry's fields (including the entity/role ids `account cert upload`
/// relies on) can be tested without the real config file or OS keychain.
fn context_from(
    config: &Config,
    alias_flag: Option<&str>,
    env_alias: Option<&str>,
    store: Arc<dyn SecretStore>,
) -> Result<AccountContext, CliError> {
    let alias = config.resolve_alias(alias_flag, env_alias)?;
    let entry = &config.accounts[&alias];
    let provider = provider_for(&alias, entry.account_id.as_str(), entry.flow, store)?;
    let client = NsClient::new(
        reqwest::Client::new(),
        account::rest_base(&entry.account_id),
        provider,
    );
    Ok(AccountContext {
        alias,
        account_id: entry.account_id.clone(),
        client,
        restlet_base: account::restlet_base(&entry.account_id),
        entity_id: entry.entity_id.clone(),
        role_id: entry.role_id.clone(),
    })
}

fn provider_for(
    alias: &str,
    account_id: &str,
    flow: AuthFlow,
    store: Arc<dyn SecretStore>,
) -> Result<Arc<dyn TokenProvider>, CliError> {
    let secrets = store.get(alias)?.ok_or_else(|| {
        CliError::Auth(format!(
            "no credentials stored for '{alias}'; run `netsuite-cli account add`"
        ))
    })?;
    let token_url = format!(
        "{}/services/rest/auth/oauth2/v1/token",
        account::rest_base(account_id)
    );
    match (flow, secrets) {
        (
            AuthFlow::M2m,
            AccountSecrets::M2m {
                client_id,
                cert_id,
                private_key_pem,
            },
        ) => {
            let config = M2mConfig {
                token_url,
                client_id,
                cert_id,
                private_key_pem,
                scopes: M2M_DEFAULT_SCOPES
                    .iter()
                    .map(|scope| scope.to_string())
                    .collect(),
            };
            Ok(Arc::new(M2mProvider::new(
                reqwest::Client::new(),
                alias.to_string(),
                config,
                store,
            )))
        }
        (AuthFlow::AuthCode, AccountSecrets::AuthCode { client_id, .. }) => {
            Ok(Arc::new(AuthCodeProvider::new(
                reqwest::Client::new(),
                alias.to_string(),
                token_url,
                client_id,
                store,
            )))
        }
        _ => Err(CliError::Auth(format!(
            "stored credentials for '{alias}' do not match its configured flow"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AccountEntry;
    use crate::secrets::MemoryStore;

    /// The entity/role ids recorded on the config entry must reach `AccountContext`, since
    /// `account cert upload` defaults its NetSuite mapping from `context.entity_id`/`role_id`.
    /// A regression here (dropped copy, swapped fields) would silently send the wrong mapping.
    #[test]
    fn context_carries_entity_and_role_ids_from_the_config_entry() {
        let mut config = Config::default();
        config.accounts.insert(
            "bootstrap".into(),
            AccountEntry {
                account_id: "1234567_SB1".into(),
                flow: AuthFlow::AuthCode,
                entity_id: Some("9".into()),
                role_id: Some("3".into()),
            },
        );
        let store: Arc<dyn SecretStore> = Arc::new(MemoryStore::default());
        store
            .set(
                "bootstrap",
                &AccountSecrets::AuthCode {
                    client_id: "CID".into(),
                    refresh_token: Some("REFRESH".into()),
                },
            )
            .unwrap();

        let context = context_from(&config, Some("bootstrap"), None, store).unwrap();
        assert_eq!(context.entity_id.as_deref(), Some("9"));
        assert_eq!(context.role_id.as_deref(), Some("3"));
        assert_eq!(context.restlet_base, account::restlet_base("1234567_SB1"));
    }

    /// M2M accounts never record a mapping, so the context's ids stay `None` and
    /// `account cert upload` correctly falls back to requiring the `--entity`/`--role` flags.
    #[test]
    fn context_has_no_mapping_ids_for_m2m_accounts() {
        let mut config = Config::default();
        config.accounts.insert(
            "prod".into(),
            AccountEntry {
                account_id: "1234567".into(),
                flow: AuthFlow::M2m,
                entity_id: None,
                role_id: None,
            },
        );
        let store: Arc<dyn SecretStore> = Arc::new(MemoryStore::default());
        store
            .set(
                "prod",
                &AccountSecrets::M2m {
                    client_id: "CID".into(),
                    cert_id: "KID".into(),
                    private_key_pem: "PEM".into(),
                },
            )
            .unwrap();

        let context = context_from(&config, Some("prod"), None, store).unwrap();
        assert!(context.entity_id.is_none());
        assert!(context.role_id.is_none());
    }
}
