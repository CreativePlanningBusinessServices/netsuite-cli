use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use netsuite_cli::auth::TokenProvider;
use netsuite_cli::client::NsClient;
use netsuite_cli::error::CliError;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[derive(Default)]
struct FakeTokens {
    invalidations: AtomicU32,
}

impl TokenProvider for FakeTokens {
    fn access_token<'life>(
        &'life self,
    ) -> Pin<Box<dyn Future<Output = Result<String, CliError>> + Send + 'life>> {
        let invalidation_count = self.invalidations.load(Ordering::SeqCst);
        Box::pin(async move { Ok(format!("TOKEN_{invalidation_count}")) })
    }
    fn invalidate(&self) {
        self.invalidations.fetch_add(1, Ordering::SeqCst);
    }
}

fn client(server: &MockServer) -> NsClient {
    client_with_tokens(server, Arc::new(FakeTokens::default()))
}

fn client_with_tokens(server: &MockServer, tokens: Arc<FakeTokens>) -> NsClient {
    NsClient::new(reqwest::Client::new(), server.uri(), tokens)
}

#[tokio::test]
async fn sends_bearer_and_parses_json_body() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/services/rest/record/v1/customer/42"))
        .and(header("Authorization", "Bearer TOKEN_0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": "42"})))
        .mount(&server)
        .await;

    let response = client(&server)
        .request(
            reqwest::Method::GET,
            "/services/rest/record/v1/customer/42",
            &[],
            &[],
            None,
        )
        .await
        .unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(response.body.unwrap()["id"], "42");
}

#[tokio::test]
async fn inserts_leading_slash_when_joining_relative_path_without_one() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/no-slash/path"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
        .mount(&server)
        .await;

    let response = client(&server)
        .request(reqwest::Method::GET, "no-slash/path", &[], &[], None)
        .await
        .unwrap();
    assert_eq!(response.status, 200);
}

#[tokio::test]
async fn captures_location_on_204_create() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/services/rest/record/v1/customer"))
        .respond_with(ResponseTemplate::new(204).insert_header(
            "Location",
            "https://x.suitetalk.api.netsuite.com/services/rest/record/v1/customer/647",
        ))
        .mount(&server)
        .await;

    let response = client(&server)
        .request(
            reqwest::Method::POST,
            "/services/rest/record/v1/customer",
            &[],
            &[],
            Some(&serde_json::json!({"companyName": "Acme"})),
        )
        .await
        .unwrap();
    assert_eq!(response.status, 204);
    assert!(response.body.is_none());
    assert!(response.location.unwrap().ends_with("/customer/647"));
}

#[tokio::test]
async fn retries_429_honoring_retry_after_then_succeeds() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/limited"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "1"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/limited"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
        .mount(&server)
        .await;

    let response = client(&server)
        .request(reqwest::Method::GET, "/limited", &[], &[], None)
        .await
        .unwrap();
    assert_eq!(response.status, 200);
}

#[tokio::test]
async fn maps_netsuite_error_envelope() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/bad"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "type": "https://www.rfc-editor.org/rfc/rfc9110.html#name-400-bad-request",
            "title": "Bad Request", "status": 400,
            "o:errorDetails": [{"detail": "Invalid field somefield.", "o:errorCode": "INVALID_CONTENT"}]
        })))
        .mount(&server)
        .await;

    match client(&server)
        .request(reqwest::Method::GET, "/bad", &[], &[], None)
        .await
        .unwrap_err()
    {
        CliError::Api {
            status,
            message,
            details,
        } => {
            assert_eq!(status, 400);
            assert!(message.contains("Invalid field somefield."));
            assert_eq!(details[0]["o:errorCode"], "INVALID_CONTENT");
        }
        other => panic!("expected Api error, got {other:?}"),
    }
}

#[tokio::test]
async fn retries_once_with_fresh_token_on_401() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/auth"))
        .and(header("Authorization", "Bearer TOKEN_0"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/auth"))
        .and(header("Authorization", "Bearer TOKEN_1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
        .mount(&server)
        .await;

    let tokens = Arc::new(FakeTokens::default());
    let response = client_with_tokens(&server, tokens.clone())
        .request(reqwest::Method::GET, "/auth", &[], &[], None)
        .await
        .unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(tokens.invalidations.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn gives_up_after_exhausting_all_attempts() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/always-limited"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "0"))
        .expect(4)
        .mount(&server)
        .await;

    let error = client(&server)
        .request(reqwest::Method::GET, "/always-limited", &[], &[], None)
        .await
        .unwrap_err();
    match error {
        CliError::Api { status, .. } => assert_eq!(status, 429),
        other => panic!("expected Api error, got {other:?}"),
    }
}
