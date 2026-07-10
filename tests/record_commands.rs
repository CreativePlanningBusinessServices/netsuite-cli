mod common;

use common::client_for;
use netsuite_cli::commands::record;
use wiremock::matchers::{body_json, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn get_passes_fields_and_expand_params() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/services/rest/record/v1/customer/42"))
        .and(query_param("fields", "companyName,email"))
        .and(query_param("expandSubResources", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": "42"})))
        .mount(&server)
        .await;

    let record_value = record::get(
        &client_for(&server),
        "customer",
        "42",
        Some("companyName,email".into()),
        true,
    )
    .await
    .unwrap();
    assert_eq!(record_value["id"], "42");
}

#[tokio::test]
async fn create_returns_id_from_location_header() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/services/rest/record/v1/customer"))
        .and(body_json(serde_json::json!({"companyName": "Acme"})))
        .respond_with(
            ResponseTemplate::new(204)
                .insert_header("Location", "https://x/services/rest/record/v1/customer/647"),
        )
        .mount(&server)
        .await;

    let created = record::create(
        &client_for(&server),
        "customer",
        serde_json::json!({"companyName": "Acme"}),
    )
    .await
    .unwrap();
    assert_eq!(
        created,
        serde_json::json!({
            "id": "647", "location": "https://x/services/rest/record/v1/customer/647"
        })
    );
}

#[tokio::test]
async fn list_all_follows_pagination_and_merges_items() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/services/rest/record/v1/customer"))
        .and(query_param("offset", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "count": 2, "hasMore": true, "offset": 0, "totalResults": 3,
            "items": [{"id": "1"}, {"id": "2"}], "links": []
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/services/rest/record/v1/customer"))
        .and(query_param("offset", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "count": 1, "hasMore": false, "offset": 2, "totalResults": 3,
            "items": [{"id": "3"}], "links": []
        })))
        .mount(&server)
        .await;

    let listing = record::list(&client_for(&server), "customer", None, Some(2), None, true)
        .await
        .unwrap();
    assert_eq!(listing["totalResults"], 3);
    assert_eq!(listing["items"].as_array().unwrap().len(), 3);
    assert_eq!(listing["count"], 3);
    assert_eq!(listing["hasMore"], false);
}

#[tokio::test]
async fn upsert_uses_eid_path_and_update_sends_replace_param() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/services/rest/record/v1/customer/eid:ACME-1"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/services/rest/record/v1/salesOrder/7"))
        .and(query_param("replace", "item"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let ns_client = client_for(&server);
    let upserted = record::upsert(
        &ns_client,
        "customer",
        "ACME-1",
        serde_json::json!({"companyName": "Acme"}),
    )
    .await
    .unwrap();
    assert_eq!(upserted["externalId"], "ACME-1");
    let updated = record::update(
        &ns_client,
        "salesOrder",
        "7",
        serde_json::json!({"item": {"items": []}}),
        Some("item".into()),
    )
    .await
    .unwrap();
    assert_eq!(updated["updated"], true);
}
