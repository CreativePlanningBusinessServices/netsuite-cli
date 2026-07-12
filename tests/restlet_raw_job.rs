mod common;

use common::client_for;
use netsuite_cli::commands::{job, raw, restlet};
use netsuite_cli::error::CliError;
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn restlet_call_get_sends_script_deploy_and_extra_params_with_bearer_to_restlet_domain() {
    let main_server = MockServer::start().await;
    let restlet_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/app/site/hosting/restlet.nl"))
        .and(query_param("script", "482"))
        .and(query_param("deploy", "1"))
        .and(query_param("customerId", "42"))
        .and(header("authorization", "Bearer TEST_TOKEN"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
        .mount(&restlet_server)
        .await;

    let result = restlet::call(
        &client_for(&main_server),
        &restlet_server.uri(),
        "482",
        "1",
        reqwest::Method::GET,
        &[("customerId".to_string(), "42".to_string())],
        None,
    )
    .await
    .unwrap();
    assert_eq!(result, serde_json::json!({"ok": true}));
}

#[tokio::test]
async fn restlet_call_post_sends_json_body_with_content_type_header() {
    let main_server = MockServer::start().await;
    let restlet_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/app/site/hosting/restlet.nl"))
        .and(query_param("script", "482"))
        .and(query_param("deploy", "1"))
        .and(header("content-type", "application/json"))
        .and(body_json(serde_json::json!({"foo": "bar"})))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"created": true})),
        )
        .mount(&restlet_server)
        .await;

    let result = restlet::call(
        &client_for(&main_server),
        &restlet_server.uri(),
        "482",
        "1",
        reqwest::Method::POST,
        &[],
        Some(serde_json::json!({"foo": "bar"})),
    )
    .await
    .unwrap();
    assert_eq!(result, serde_json::json!({"created": true}));
}

#[tokio::test]
async fn raw_run_forwards_method_query_headers_body_and_returns_response_json() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/services/rest/record/v1/customer/eid:ACME-1"))
        .and(query_param("replace", "addressBook"))
        .and(header("x-custom", "value1"))
        .and(body_json(serde_json::json!({"companyName": "Acme"})))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"updated": true})),
        )
        .mount(&server)
        .await;

    let result = raw::run(
        &client_for(&server),
        reqwest::Method::PUT,
        "/services/rest/record/v1/customer/eid:ACME-1",
        &[("replace".to_string(), "addressBook".to_string())],
        &[("x-custom".to_string(), "value1".to_string())],
        Some(serde_json::json!({"companyName": "Acme"})),
    )
    .await
    .unwrap();
    assert_eq!(result, serde_json::json!({"updated": true}));
}

#[tokio::test]
async fn raw_run_forwards_patch_method_and_body() {
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path("/services/rest/record/v1/customer/1234"))
        .and(body_json(serde_json::json!({"email": "new@acme.example"})))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let result = raw::run(
        &client_for(&server),
        reqwest::Method::PATCH,
        "/services/rest/record/v1/customer/1234",
        &[],
        &[],
        Some(serde_json::json!({"email": "new@acme.example"})),
    )
    .await
    .unwrap();
    assert_eq!(result, serde_json::json!({"status": 204, "location": null}));
}

#[tokio::test]
async fn raw_run_returns_status_and_location_when_body_is_not_json() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/services/rest/record/v1/customer"))
        .respond_with(
            ResponseTemplate::new(204)
                .insert_header("Location", "/services/rest/record/v1/customer/647"),
        )
        .mount(&server)
        .await;

    let result = raw::run(
        &client_for(&server),
        reqwest::Method::POST,
        "/services/rest/record/v1/customer",
        &[],
        &[],
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        result,
        serde_json::json!({
            "status": 204,
            "location": "/services/rest/record/v1/customer/647",
        })
    );
}

#[tokio::test]
async fn job_submit_extracts_job_id_from_relative_location_and_sends_prefer_and_idempotency_headers()
 {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/services/rest/record/v1/customer"))
        .and(header("Prefer", "respond-async"))
        .and(header(
            "X-NetSuite-Idempotency-Key",
            "11111111-1111-1111-1111-111111111111",
        ))
        // A caller-supplied --header (here the batch collection content-type) is sent
        // alongside the forced Prefer header — this is what enables batch writes.
        .and(header(
            "Content-Type",
            "application/vnd.oracle.resource+json; type=collection",
        ))
        .and(body_json(serde_json::json!({"companyName": "Acme"})))
        .respond_with(
            ResponseTemplate::new(202)
                .insert_header("Location", "/services/rest/async/v1/job/9001"),
        )
        .mount(&server)
        .await;

    let result = job::submit(
        &client_for(&server),
        reqwest::Method::POST,
        "/services/rest/record/v1/customer",
        &[],
        &[(
            "Content-Type".to_string(),
            "application/vnd.oracle.resource+json; type=collection".to_string(),
        )],
        Some(serde_json::json!({"companyName": "Acme"})),
        Some("11111111-1111-1111-1111-111111111111".to_string()),
    )
    .await
    .unwrap();
    assert_eq!(
        result,
        serde_json::json!({
            "jobId": "9001",
            "location": "/services/rest/async/v1/job/9001",
            "status": 202,
        })
    );
}

#[tokio::test]
async fn job_submit_errors_when_location_header_missing() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/services/rest/record/v1/customer"))
        .respond_with(ResponseTemplate::new(202))
        .mount(&server)
        .await;

    let submit_result = job::submit(
        &client_for(&server),
        reqwest::Method::GET,
        "/services/rest/record/v1/customer",
        &[],
        &[],
        None,
        None,
    )
    .await;
    match submit_result {
        Err(CliError::Api { status, .. }) => assert_eq!(status, 202),
        other => panic!("expected CliError::Api, got {other:?}"),
    }
}

#[tokio::test]
async fn job_status_and_tasks_send_plain_gets() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/services/rest/async/v1/job/9001"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "completed": true, "id": "9001", "progress": 100
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/services/rest/async/v1/job/9001/task/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "items": [{"id": "9001.1"}]
        })))
        .mount(&server)
        .await;

    let ns_client = client_for(&server);
    let status_result = job::status(&ns_client, "9001").await.unwrap();
    assert_eq!(status_result["completed"], true);

    let tasks_result = job::tasks(&ns_client, "9001").await.unwrap();
    assert_eq!(tasks_result["items"][0]["id"], "9001.1");
}

#[tokio::test]
async fn job_result_with_explicit_task_fetches_directly() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/services/rest/async/v1/job/9001/task/9001.1/result"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": "647"})))
        .mount(&server)
        .await;

    let result = job::result(&client_for(&server), "9001", Some("9001.1".to_string()))
        .await
        .unwrap();
    assert_eq!(result["id"], "647");
}

#[tokio::test]
async fn job_result_without_task_auto_picks_single_task() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/services/rest/async/v1/job/9001/task/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "items": [{"id": "9001.1"}]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/services/rest/async/v1/job/9001/task/9001.1/result"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": "647"})))
        .mount(&server)
        .await;

    let result = job::result(&client_for(&server), "9001", None)
        .await
        .unwrap();
    assert_eq!(result["id"], "647");
}

#[tokio::test]
async fn job_result_without_task_errors_on_multiple_tasks() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/services/rest/async/v1/job/9001/task/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "items": [
                {"id": "9001.1"},
                {
                    "links": [{
                        "rel": "self",
                        "href": "/services/rest/async/v1/job/9001/task/9001.2"
                    }]
                }
            ]
        })))
        .mount(&server)
        .await;

    let result_error = job::result(&client_for(&server), "9001", None).await;
    match result_error {
        Err(CliError::Usage(message)) => {
            assert!(message.contains("9001.1"));
            assert!(message.contains("9001.2"));
            assert!(message.contains("--task"));
        }
        other => panic!("expected CliError::Usage, got {other:?}"),
    }
}

#[tokio::test]
async fn job_result_without_task_auto_picks_single_task_from_links_only_item() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/services/rest/async/v1/job/9001/task/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "items": [{
                "links": [{
                    "rel": "self",
                    "href": "/services/rest/async/v1/job/9001/task/9001.1"
                }]
            }]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/services/rest/async/v1/job/9001/task/9001.1/result"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": "647"})))
        .mount(&server)
        .await;

    let result = job::result(&client_for(&server), "9001", None)
        .await
        .unwrap();
    assert_eq!(result["id"], "647");
}

#[tokio::test]
async fn job_result_without_task_errors_on_zero_tasks() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/services/rest/async/v1/job/9001/task/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"items": []})))
        .mount(&server)
        .await;

    let result_error = job::result(&client_for(&server), "9001", None).await;
    assert!(matches!(result_error, Err(CliError::Usage(_))));
}
