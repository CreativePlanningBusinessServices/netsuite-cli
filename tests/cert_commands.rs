mod common;

use common::client_for;
use netsuite_cli::commands::cert;
use netsuite_cli::error::CliError;
use serde_json::json;
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CLIENT_ID: &str = "751a2830e09c6f47b00e486ec192934cd0efad12fdba5f4703841bd2b67d5357";

fn certificates_path() -> String {
    format!("/services/rest/auth/oauth2/v1/clients/{CLIENT_ID}/certificates")
}

#[tokio::test]
async fn list_wraps_netsuite_certificate_array() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(certificates_path()))
        .and(header("authorization", "Bearer TEST_TOKEN"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"certificate_id": "CERT1", "algorithm": "EC", "revoked": false}
        ])))
        .expect(1)
        .mount(&server)
        .await;

    let client = client_for(&server);
    let result = cert::list(&client, &server.uri(), CLIENT_ID).await.unwrap();
    assert_eq!(result["certificates"][0]["certificate_id"], "CERT1");
}

#[tokio::test]
async fn upload_posts_pem_with_entity_and_role_and_returns_certificate_id() {
    let temp_dir = tempfile::tempdir().unwrap();
    let cert_path = temp_dir.path().join("cert.pem");
    let pem = "-----BEGIN CERTIFICATE-----\nMIIC\n-----END CERTIFICATE-----\n";
    std::fs::write(&cert_path, pem).unwrap();

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(certificates_path()))
        .and(body_partial_json(json!({
            "fileContent": pem,
            "entity": -5,
            "role": 1000,
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "certificate_id": "NEWCERTID",
            "algorithm": "EC",
            "valid_until": "2028-07-17",
            "invalidated": false
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = client_for(&server);
    let result = cert::upload(&client, &server.uri(), CLIENT_ID, &cert_path, "-5", "1000")
        .await
        .unwrap();
    assert_eq!(result["certificateId"], "NEWCERTID");
    assert_eq!(result["details"]["valid_until"], "2028-07-17");
}

#[tokio::test]
async fn upload_refuses_a_private_key_file_without_calling_netsuite() {
    let temp_dir = tempfile::tempdir().unwrap();
    let key_path = temp_dir.path().join("key.pem");
    std::fs::write(
        &key_path,
        "-----BEGIN PRIVATE KEY-----\nMIG\n-----END PRIVATE KEY-----\n",
    )
    .unwrap();

    let server = MockServer::start().await; // no mocks: any request would 404 and fail differently
    let client = client_for(&server);
    let error = cert::upload(&client, &server.uri(), CLIENT_ID, &key_path, "9", "3")
        .await
        .unwrap_err();
    match error {
        CliError::Usage(message) => assert!(message.contains("never upload the key")),
        other => panic!("expected Usage error, got {other:?}"),
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn upload_rejects_a_file_that_is_not_a_certificate() {
    let temp_dir = tempfile::tempdir().unwrap();
    let not_pem = temp_dir.path().join("notes.txt");
    std::fs::write(&not_pem, "hello").unwrap();

    let server = MockServer::start().await;
    let client = client_for(&server);
    let error = cert::upload(&client, &server.uri(), CLIENT_ID, &not_pem, "9", "3")
        .await
        .unwrap_err();
    assert!(matches!(error, CliError::Usage(_)));
}

#[tokio::test]
async fn revoke_posts_to_the_revoke_url_and_synthesizes_json() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!("{}/CERT1/revoke", certificates_path())))
        .respond_with(ResponseTemplate::new(200).set_body_string("Successfully revoked"))
        .expect(1)
        .mount(&server)
        .await;

    let client = client_for(&server);
    let result = cert::revoke(&client, &server.uri(), CLIENT_ID, "CERT1")
        .await
        .unwrap();
    assert_eq!(result, json!({"revoked": true, "certificateId": "CERT1"}));
}

#[tokio::test]
async fn rotation_api_error_surfaces_as_api_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(certificates_path()))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "title": "Forbidden",
            "o:errorDetails": [{"detail": "missing Manage own OAuth 2.0 Client Credentials certificates permission", "o:errorCode": "INSUFFICIENT_PERMISSION"}]
        })))
        .mount(&server)
        .await;

    let client = client_for(&server);
    let error = cert::list(&client, &server.uri(), CLIENT_ID)
        .await
        .unwrap_err();
    match error {
        CliError::Api { status, .. } => assert_eq!(status, 403),
        other => panic!("expected Api error, got {other:?}"),
    }
}
