use std::sync::Arc;

use crate::account;
use crate::auth::TokenProvider;
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
}

pub fn context_for(alias_flag: Option<&str>) -> Result<AccountContext, CliError> {
    let config = Config::load(&crate::config::default_config_path())?;
    let env_alias = std::env::var("NETSUITE_ACCOUNT").ok();
    let alias = config.resolve_alias(alias_flag, env_alias.as_deref())?;
    let entry = &config.accounts[&alias];
    let store: Arc<dyn SecretStore> = Arc::new(KeyringStore);
    let provider = provider_for(&alias, entry.account_id.as_str(), entry.flow, store.clone())?;
    let client = NsClient::new(
        reqwest::Client::new(),
        account::rest_base(&entry.account_id),
        provider,
    );
    Ok(AccountContext {
        alias,
        account_id: entry.account_id.clone(),
        client,
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
        (AuthFlow::AuthCode, AccountSecrets::AuthCode { .. }) => {
            Err(CliError::Auth("auth-code provider lands in Task 9".into())) // replaced in Task 9
        }
        _ => Err(CliError::Auth(format!(
            "stored credentials for '{alias}' do not match its configured flow"
        ))),
    }
}
