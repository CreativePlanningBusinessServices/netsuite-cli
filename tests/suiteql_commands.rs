mod common;

use common::client_for;
use netsuite_cli::commands::suiteql;
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn posts_query_with_transient_prefer_header() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/services/rest/query/v1/suiteql"))
        .and(header("Prefer", "transient"))
        .and(body_json(
            serde_json::json!({"q": "SELECT id FROM customer"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "count": 1, "hasMore": false, "offset": 0, "totalResults": 1,
            "items": [{"id": "1"}], "links": []
        })))
        .mount(&server)
        .await;

    let result = suiteql::run(
        &client_for(&server),
        "SELECT id FROM customer",
        None,
        None,
        false,
    )
    .await
    .unwrap();
    assert_eq!(result["items"][0]["id"], "1");
}

#[tokio::test]
async fn all_flag_pages_until_has_more_is_false() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/services/rest/query/v1/suiteql"))
        .and(query_param("offset", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "count": 2, "hasMore": true, "offset": 0, "totalResults": 3,
            "items": [{"id": "1"}, {"id": "2"}], "links": []
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/services/rest/query/v1/suiteql"))
        .and(query_param("offset", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "count": 1, "hasMore": false, "offset": 2, "totalResults": 3,
            "items": [{"id": "3"}], "links": []
        })))
        .mount(&server)
        .await;

    let result = suiteql::run(
        &client_for(&server),
        "SELECT id FROM customer",
        Some(2),
        None,
        true,
    )
    .await
    .unwrap();
    assert_eq!(result["items"].as_array().unwrap().len(), 3);
    assert_eq!(result["hasMore"], false);
}

#[tokio::test]
async fn all_flag_stops_when_page_contributes_no_items_but_claims_more() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/services/rest/query/v1/suiteql"))
        .and(query_param("offset", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "count": 0, "hasMore": true, "offset": 0, "totalResults": 3,
            "items": [], "links": []
        })))
        .expect(1)
        .mount(&server)
        .await;

    let result = suiteql::run(
        &client_for(&server),
        "SELECT id FROM customer",
        None,
        None,
        true,
    )
    .await
    .unwrap();
    assert_eq!(result["hasMore"], true);
    assert_eq!(result["items"].as_array().unwrap().len(), 0);
}
