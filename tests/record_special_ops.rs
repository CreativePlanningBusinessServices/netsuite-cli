mod common;

use common::client_for;
use netsuite_cli::commands::record;
use netsuite_cli::error::CliError;
use wiremock::matchers::{body_json, header, method, path, query_param};
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

#[tokio::test]
async fn transform_returns_id_from_location_header() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(
            "/services/rest/record/v1/salesOrder/7/!transform/invoice",
        ))
        .and(body_json(serde_json::json!({})))
        .respond_with(
            ResponseTemplate::new(204)
                .insert_header("Location", "https://x/services/rest/record/v1/invoice/91"),
        )
        .mount(&server)
        .await;

    let transformed = record::transform(
        &client_for(&server),
        "salesOrder",
        "7",
        "invoice",
        None,
        false,
        None,
        false,
    )
    .await
    .unwrap();
    assert_eq!(
        transformed,
        serde_json::json!({
            "id": "91", "location": "https://x/services/rest/record/v1/invoice/91"
        })
    );
}

#[tokio::test]
async fn transform_errors_when_location_header_missing() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(
            "/services/rest/record/v1/salesOrder/7/!transform/invoice",
        ))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let transform_result = record::transform(
        &client_for(&server),
        "salesOrder",
        "7",
        "invoice",
        None,
        false,
        None,
        false,
    )
    .await;

    match transform_result {
        Err(CliError::Api {
            status, message, ..
        }) => {
            assert_eq!(status, 204);
            assert!(
                message.contains("no Location header was returned"),
                "unexpected message: {message}"
            );
        }
        other => panic!("expected CliError::Api, got {other:?}"),
    }
}

#[tokio::test]
async fn transform_form_sends_create_form_accept_and_passes_body_through() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(
            "/services/rest/record/v1/salesOrder/7/!transform/itemFulfillment",
        ))
        .and(header(
            "Accept",
            "application/vnd.oracle.resource+json; type=create-form",
        ))
        .and(query_param("fields", "item"))
        .and(query_param("expandSubResources", "true"))
        .and(body_json(serde_json::json!({})))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "item": {"items": [{"quantity": 3}]}
        })))
        .mount(&server)
        .await;

    let preview = record::transform(
        &client_for(&server),
        "salesOrder",
        "7",
        "itemFulfillment",
        None,
        true,
        Some("item".into()),
        true,
    )
    .await
    .unwrap();
    assert_eq!(preview["item"]["items"][0]["quantity"], 3);
}

#[tokio::test]
async fn create_form_posts_accept_header_and_default_empty_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/services/rest/record/v1/salesOrder"))
        .and(header(
            "Accept",
            "application/vnd.oracle.resource+json; type=create-form",
        ))
        .and(body_json(serde_json::json!({})))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "currency": {"id": "1", "refName": "USD"}
        })))
        .mount(&server)
        .await;

    let form = record::create_form(&client_for(&server), "salesOrder", None, None, false)
        .await
        .unwrap();
    assert_eq!(form["currency"]["refName"], "USD");
}

#[tokio::test]
async fn edit_form_patches_with_accept_header_and_body() {
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path("/services/rest/record/v1/salesOrder/12"))
        .and(header(
            "Accept",
            "application/vnd.oracle.resource+json; type=edit-form",
        ))
        .and(query_param("fields", "memo,total"))
        .and(body_json(serde_json::json!({"memo": "rush"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "memo": "rush", "total": 99.5
        })))
        .mount(&server)
        .await;

    let form = record::edit_form(
        &client_for(&server),
        "salesOrder",
        "12",
        Some(serde_json::json!({"memo": "rush"})),
        Some("memo,total".into()),
        false,
    )
    .await
    .unwrap();
    assert_eq!(form["total"], 99.5);
}

#[tokio::test]
async fn select_options_without_id_posts_with_filters() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/services/rest/record/v1/customer"))
        .and(header(
            "Accept",
            "application/vnd.oracle.resource+json; type=select-options",
        ))
        .and(query_param("fields", "entitystatus,location"))
        .and(query_param("q", "entitystatus START_WITH LEAD-"))
        .and(query_param("limit", "50"))
        .and(query_param("offset", "0"))
        .and(body_json(serde_json::json!({})))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "entitystatus": {
                "_selectOptions": {
                    "items": [{"id": 13, "refName": "CUSTOMER-Closed Won"}],
                    "count": 1, "offset": 0, "hasMore": false, "totalResults": 1
                }
            }
        })))
        .mount(&server)
        .await;

    let options = record::select_options(
        &client_for(&server),
        "customer",
        None,
        "entitystatus,location",
        Some("entitystatus START_WITH LEAD-".into()),
        Some(50),
        Some(0),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        options["entitystatus"]["_selectOptions"]["items"][0]["id"],
        13
    );
}

#[tokio::test]
async fn select_options_with_id_patches_with_dependency_body() {
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path("/services/rest/record/v1/salesOrder/12"))
        .and(header(
            "Accept",
            "application/vnd.oracle.resource+json; type=select-options",
        ))
        .and(query_param("fields", "item"))
        .and(body_json(serde_json::json!({"subsidiary": {"id": 1}})))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "item": {"_selectOptions": {"items": []}}
        })))
        .mount(&server)
        .await;

    let options = record::select_options(
        &client_for(&server),
        "salesOrder",
        Some("12"),
        "item",
        None,
        None,
        None,
        Some(serde_json::json!({"subsidiary": {"id": 1}})),
    )
    .await
    .unwrap();
    assert!(options["item"]["_selectOptions"]["items"].is_array());
}
