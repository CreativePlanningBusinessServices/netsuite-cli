mod common;

use std::time::Duration;

use common::client_for;
use netsuite_cli::commands::describe::{self, MetadataFormat};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn list_types_returns_names_from_catalog() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/services/rest/record/v1/metadata-catalog"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "items": [{"name": "account", "links": []}, {"name": "customer", "links": []}]
        })))
        .mount(&server)
        .await;

    let types = describe::list_types(&client_for(&server)).await.unwrap();
    assert_eq!(
        types,
        serde_json::json!({"recordTypes": ["account", "customer"]})
    );
}

#[tokio::test]
async fn describe_sends_schema_accept_header_and_caches() {
    let server = MockServer::start().await;
    let cache_dir = tempfile::tempdir().unwrap();
    Mock::given(method("GET"))
        .and(path("/services/rest/record/v1/metadata-catalog/customer"))
        .and(header("Accept", "application/schema+json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "type": "object", "properties": {"companyName": {"type": "string"}}
        })))
        .expect(1) // second call must hit the cache
        .mount(&server)
        .await;

    let ns_client = client_for(&server);
    let ttl = Duration::from_secs(3600);
    let first = describe::describe_type(
        &ns_client,
        "customer",
        MetadataFormat::Schema,
        cache_dir.path(),
        false,
        ttl,
    )
    .await
    .unwrap();
    let second = describe::describe_type(
        &ns_client,
        "customer",
        MetadataFormat::Schema,
        cache_dir.path(),
        false,
        ttl,
    )
    .await
    .unwrap();
    assert_eq!(first, second);
    assert_eq!(first["properties"]["companyName"]["type"], "string");
}

#[tokio::test]
async fn refresh_flag_bypasses_cache() {
    let server = MockServer::start().await;
    let cache_dir = tempfile::tempdir().unwrap();
    Mock::given(method("GET"))
        .and(path("/services/rest/record/v1/metadata-catalog/customer"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"v": 1})))
        .expect(2)
        .mount(&server)
        .await;

    let ns_client = client_for(&server);
    let ttl = Duration::from_secs(3600);
    describe::describe_type(
        &ns_client,
        "customer",
        MetadataFormat::Schema,
        cache_dir.path(),
        false,
        ttl,
    )
    .await
    .unwrap();
    describe::describe_type(
        &ns_client,
        "customer",
        MetadataFormat::Schema,
        cache_dir.path(),
        true,
        ttl,
    )
    .await
    .unwrap();
}
