use std::sync::Arc;

use netsuite_cli::auth::TokenProvider;
use netsuite_cli::auth::authcode::{AuthCodeProvider, exchange_code};
use netsuite_cli::secrets::{AccountSecrets, MemoryStore, SecretStore};
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

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
