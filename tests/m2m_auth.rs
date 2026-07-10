use std::sync::Arc;

use netsuite_cli::auth::m2m::{M2mConfig, M2mProvider};
use netsuite_cli::auth::TokenProvider;
use netsuite_cli::secrets::MemoryStore;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn test_config(token_url: String) -> M2mConfig {
    M2mConfig {
        token_url,
        client_id: "cid".into(),
        cert_id: "kid".into(),
        private_key_pem: rcgen::KeyPair::generate().unwrap().serialize_pem(),
        scopes: vec!["rest_webservices".into()],
    }
}

#[tokio::test]
async fn exchanges_assertion_for_token_and_caches_it() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/services/rest/auth/oauth2/v1/token"))
        .and(body_string_contains("grant_type=client_credentials"))
        .and(body_string_contains("client_assertion_type=urn%3Aietf%3Aparams%3Aoauth%3Aclient-assertion-type%3Ajwt-bearer"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "ACCESS_1", "expires_in": 3600, "token_type": "Bearer"
        })))
        .expect(1) // second call must come from cache
        .mount(&server).await;

    let token_url = format!("{}/services/rest/auth/oauth2/v1/token", server.uri());
    let provider = M2mProvider::new(reqwest::Client::new(), "prod".into(),
        test_config(token_url), Arc::new(MemoryStore::default()));

    assert_eq!(provider.access_token().await.unwrap(), "ACCESS_1");
    assert_eq!(provider.access_token().await.unwrap(), "ACCESS_1");
}

#[tokio::test]
async fn token_endpoint_error_maps_to_auth_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/services/rest/auth/oauth2/v1/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": "invalid_grant"
        })))
        .mount(&server).await;

    let token_url = format!("{}/services/rest/auth/oauth2/v1/token", server.uri());
    let provider = M2mProvider::new(reqwest::Client::new(), "prod".into(),
        test_config(token_url), Arc::new(MemoryStore::default()));

    let error = provider.access_token().await.unwrap_err();
    assert!(matches!(error, netsuite_cli::error::CliError::Auth(_)));
    assert!(error.to_string().contains("invalid_grant"));
}
