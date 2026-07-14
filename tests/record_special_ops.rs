mod common;

use common::client_for;
use netsuite_cli::commands::record;
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn attach_posts_role_body_and_reports_linkage() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(
            "/services/rest/record/v1/customer/660/!attach/contact/106",
        ))
        .and(body_json(serde_json::json!({"role": {"id": "-5"}})))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let attached = record::attach(
        &client_for(&server),
        "customer",
        "660",
        "contact",
        "106",
        Some("-5".into()),
    )
    .await
    .unwrap();
    assert_eq!(
        attached,
        serde_json::json!({
            "attached": true, "type": "customer", "id": "660",
            "attachedType": "contact", "attachedId": "106"
        })
    );
}

#[tokio::test]
async fn attach_without_role_sends_empty_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(
            "/services/rest/record/v1/customer/660/!attach/contact/106",
        ))
        .and(body_json(serde_json::json!({})))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let attached = record::attach(
        &client_for(&server),
        "customer",
        "660",
        "contact",
        "106",
        None,
    )
    .await
    .unwrap();
    assert_eq!(attached["attached"], true);
}

#[tokio::test]
async fn detach_posts_empty_body_and_reports_linkage() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(
            "/services/rest/record/v1/opportunity/379/!detach/file/398",
        ))
        .and(body_json(serde_json::json!({})))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let detached = record::detach(&client_for(&server), "opportunity", "379", "file", "398")
        .await
        .unwrap();
    assert_eq!(
        detached,
        serde_json::json!({
            "detached": true, "type": "opportunity", "id": "379",
            "detachedType": "file", "detachedId": "398"
        })
    );
}
