mod common;

use common::client_for;
use netsuite_cli::commands::system;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn server_time_returns_body_passthrough() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/services/rest/system/v1/serverTime"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "serverTime": "2026-07-14T16:21:00.000Z"
        })))
        .mount(&server)
        .await;

    let server_time = system::server_time(&client_for(&server)).await.unwrap();
    assert_eq!(server_time["serverTime"], "2026-07-14T16:21:00.000Z");
}

#[tokio::test]
async fn governance_limits_returns_body_passthrough() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/services/rest/system/v1/governanceLimits"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "type": "accountLimit",
            "accountConcurrencyLimit": 25,
            "accountUnallocatedConcurrencyLimit": 10
        })))
        .mount(&server)
        .await;

    let limits = system::governance_limits(&client_for(&server))
        .await
        .unwrap();
    assert_eq!(limits["accountConcurrencyLimit"], 25);
}
