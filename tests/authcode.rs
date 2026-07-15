use std::sync::Arc;

use netsuite_cli::auth::TokenProvider;
use netsuite_cli::auth::authcode::{AuthCodeProvider, exchange_code};
use netsuite_cli::error::CliError;
use netsuite_cli::secrets::{AccountSecrets, CachedToken, MemoryStore, SecretStore};
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Wraps a `MemoryStore` but fails only `set` (the rotated-refresh-token write), so tests can
/// prove what happens when persisting the rotated refresh token fails after a successful
/// refresh — the interesting case, since `refresh` itself already succeeded and NetSuite has
/// already invalidated the old refresh token server-side by that point.
struct FailingOnSetStore {
    inner: MemoryStore,
}

impl SecretStore for FailingOnSetStore {
    fn get(&self, alias: &str) -> Result<Option<AccountSecrets>, CliError> {
        self.inner.get(alias)
    }
    fn set(&self, _alias: &str, _secrets: &AccountSecrets) -> Result<(), CliError> {
        Err(CliError::Auth("keychain unavailable".into()))
    }
    fn delete(&self, alias: &str) -> Result<(), CliError> {
        self.inner.delete(alias)
    }
    fn get_token(&self, alias: &str) -> Result<Option<CachedToken>, CliError> {
        self.inner.get_token(alias)
    }
    fn set_token(&self, alias: &str, token: &CachedToken) -> Result<(), CliError> {
        self.inner.set_token(alias, token)
    }
    fn delete_token(&self, alias: &str) -> Result<(), CliError> {
        self.inner.delete_token(alias)
    }
    fn get_tba(&self, alias: &str) -> Result<Option<netsuite_cli::secrets::TbaSecrets>, CliError> {
        self.inner.get_tba(alias)
    }
    fn set_tba(
        &self,
        alias: &str,
        secrets: &netsuite_cli::secrets::TbaSecrets,
    ) -> Result<(), CliError> {
        self.inner.set_tba(alias, secrets)
    }
    fn delete_tba(&self, alias: &str) -> Result<(), CliError> {
        self.inner.delete_tba(alias)
    }
}

#[tokio::test]
async fn exchange_sends_pkce_verifier_and_public_client_id() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains("code=CODE1"))
        .and(body_string_contains("code_verifier=VERIFIER"))
        .and(body_string_contains("client_id=cid"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "AT1", "refresh_token": "RT1", "expires_in": 3600, "token_type": "bearer"
        })))
        .mount(&server)
        .await;

    let token = exchange_code(
        &reqwest::Client::new(),
        &format!("{}/token", server.uri()),
        "cid",
        "CODE1",
        "https://localhost:8899/callback",
        "VERIFIER",
    )
    .await
    .unwrap();
    assert_eq!(token.access_token, "AT1");
    assert_eq!(token.refresh_token.as_deref(), Some("RT1"));
}

#[tokio::test]
async fn provider_refreshes_and_persists_rotated_refresh_token() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("refresh_token=RT_OLD"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "AT2", "refresh_token": "RT_NEW", "expires_in": 3600, "token_type": "bearer"
        })))
        .mount(&server)
        .await;

    let store: Arc<dyn SecretStore> = Arc::new(MemoryStore::default());
    store
        .set(
            "dev",
            &AccountSecrets::AuthCode {
                client_id: "cid".into(),
                refresh_token: Some("RT_OLD".into()),
            },
        )
        .unwrap();

    let provider = AuthCodeProvider::new(
        reqwest::Client::new(),
        "dev".into(),
        format!("{}/token", server.uri()),
        "cid".into(),
        store.clone(),
    );
    assert_eq!(provider.access_token().await.unwrap(), "AT2");

    match store.get("dev").unwrap().unwrap() {
        AccountSecrets::AuthCode { refresh_token, .. } => {
            assert_eq!(refresh_token.as_deref(), Some("RT_NEW")) // rotation persisted
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

#[tokio::test]
async fn refresh_without_rotated_token_still_succeeds_and_leaves_old_refresh_stored() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("refresh_token=RT_OLD"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "AT2", "expires_in": 3600, "token_type": "bearer"
        })))
        .mount(&server)
        .await;

    let store: Arc<dyn SecretStore> = Arc::new(MemoryStore::default());
    store
        .set(
            "dev",
            &AccountSecrets::AuthCode {
                client_id: "cid".into(),
                refresh_token: Some("RT_OLD".into()),
            },
        )
        .unwrap();

    let provider = AuthCodeProvider::new(
        reqwest::Client::new(),
        "dev".into(),
        format!("{}/token", server.uri()),
        "cid".into(),
        store.clone(),
    );

    // The just-obtained access token is good, so this command must still succeed even though
    // the server didn't rotate the refresh token.
    assert_eq!(provider.access_token().await.unwrap(), "AT2");

    // Nothing to persist in place of the (now server-invalidated) old refresh token, so it's
    // left as-is rather than being silently cleared.
    match store.get("dev").unwrap().unwrap() {
        AccountSecrets::AuthCode { refresh_token, .. } => {
            assert_eq!(refresh_token.as_deref(), Some("RT_OLD"))
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

#[tokio::test]
async fn expired_refresh_token_yields_actionable_auth_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(
            ResponseTemplate::new(400).set_body_json(serde_json::json!({"error": "invalid_grant"})),
        )
        .mount(&server)
        .await;

    let store: Arc<dyn SecretStore> = Arc::new(MemoryStore::default());
    store
        .set(
            "dev",
            &AccountSecrets::AuthCode {
                client_id: "cid".into(),
                refresh_token: Some("RT_DEAD".into()),
            },
        )
        .unwrap();

    let provider = AuthCodeProvider::new(
        reqwest::Client::new(),
        "dev".into(),
        format!("{}/token", server.uri()),
        "cid".into(),
        store,
    );
    let error = provider.access_token().await.unwrap_err();
    assert!(
        error.to_string().contains("account add"),
        "should tell the user to re-authenticate: {error}"
    );
}

#[tokio::test]
async fn failed_rotated_refresh_persist_yields_explicit_reauth_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("refresh_token=RT_OLD"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "AT2", "refresh_token": "RT_NEW", "expires_in": 3600, "token_type": "bearer"
        })))
        .mount(&server)
        .await;

    // Seed the initial refresh token through a plain MemoryStore (FailingOnSetStore fails all
    // `set` calls, including this one), then wrap it so only the rotation write fails.
    let seeded_store = MemoryStore::default();
    seeded_store
        .set(
            "dev",
            &AccountSecrets::AuthCode {
                client_id: "cid".into(),
                refresh_token: Some("RT_OLD".into()),
            },
        )
        .unwrap();
    let store: Arc<dyn SecretStore> = Arc::new(FailingOnSetStore {
        inner: seeded_store,
    });

    let provider = AuthCodeProvider::new(
        reqwest::Client::new(),
        "dev".into(),
        format!("{}/token", server.uri()),
        "cid".into(),
        store,
    );

    let error = provider.access_token().await.unwrap_err();
    assert!(matches!(error, CliError::Auth(_)));
    let message = error.to_string();
    assert!(
        message.contains("rotated the refresh token") && message.contains("keychain"),
        "error should explain the rotated token could not be saved: {message}"
    );
    assert!(
        message.contains("account add") && message.contains("re-authenticated"),
        "error should tell the user the account is broken and how to fix it: {message}"
    );
}
