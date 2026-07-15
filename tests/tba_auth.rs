use netsuite_cli::auth::tba;
use netsuite_cli::error::CliError;
use wiremock::matchers::{header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn request_token_step_sends_signed_oauth_header_and_parses_form_response() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/rest/requesttoken"))
        .and(header_exists("Authorization"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            // NetSuite terminates the body with a newline; the parser must trim it or the
            // final parameter's value would be "true\n" (seen live against a real account).
            "oauth_token=reqtoken111&oauth_token_secret=reqtokensecret333&oauth_callback_confirmed=true\n"))
        .expect(1)
        .mount(&server)
        .await;

    let request_token = tba::obtain_request_token(
        &reqwest::Client::new(),
        &server.uri(),
        "consumerkey123",
        "consumersecret789",
        "https://localhost:8899/callback",
    )
    .await
    .unwrap();
    assert_eq!(request_token.token, "reqtoken111");
    assert_eq!(request_token.secret, "reqtokensecret333");

    let sent = &server.received_requests().await.unwrap()[0];
    let authorization = sent.headers.get("Authorization").unwrap().to_str().unwrap();
    assert!(authorization.starts_with("OAuth "));
    for expected in [
        "oauth_consumer_key=\"consumerkey123\"",
        "oauth_signature_method=\"HMAC-SHA256\"",
        "oauth_callback=\"https%3A%2F%2Flocalhost%3A8899%2Fcallback\"",
        "oauth_signature=\"",
    ] {
        assert!(
            authorization.contains(expected),
            "missing {expected} in {authorization}"
        );
    }
}

#[tokio::test]
async fn request_token_without_callback_confirmed_is_rejected() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/rest/requesttoken"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("oauth_token=reqtoken111&oauth_token_secret=reqtokensecret333"),
        )
        .mount(&server)
        .await;

    let error = tba::obtain_request_token(
        &reqwest::Client::new(),
        &server.uri(),
        "consumerkey123",
        "consumersecret789",
        "https://localhost:8899/callback",
    )
    .await
    .unwrap_err();
    assert!(matches!(error, CliError::Auth(_)));
    assert!(
        error.to_string().contains("callback"),
        "error should mention the callback confirmation problem: {error}"
    );
}

#[tokio::test]
async fn access_token_step_includes_verifier_and_parses_minted_token() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/rest/accesstoken"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("oauth_token=tokenid456&oauth_token_secret=tokensecret012\n"),
        )
        .mount(&server)
        .await;

    let minted = tba::exchange_for_access_token(
        &reqwest::Client::new(),
        &server.uri(),
        "consumerkey123",
        "consumersecret789",
        &tba::RequestToken {
            token: "reqtoken111".into(),
            secret: "reqtokensecret333".into(),
        },
        "verifier222",
    )
    .await
    .unwrap();
    assert_eq!(minted.token_id, "tokenid456");
    assert_eq!(minted.token_secret, "tokensecret012");

    let sent = &server.received_requests().await.unwrap()[0];
    let authorization = sent.headers.get("Authorization").unwrap().to_str().unwrap();
    assert!(authorization.contains("oauth_token=\"reqtoken111\""));
    assert!(authorization.contains("oauth_verifier=\"verifier222\""));
}

#[test]
fn tba_callback_parser_validates_token_state_and_extracts_verifier() {
    let query = "oauth_token=reqtoken111&oauth_verifier=verifier222&company=1234567&role=3&entity=9&state=STATE123";
    assert_eq!(
        tba::parse_tba_callback(query, "reqtoken111", "STATE123").unwrap(),
        "verifier222"
    );
    assert!(tba::parse_tba_callback(query, "reqtoken111", "WRONGSTATE").is_err());

    // A denied consent typically redirects WITHOUT oauth_token at all — this must be reported
    // as consent-denied guidance, not misdiagnosed as a token mismatch (CSRF-style) error.
    let denied = tba::parse_tba_callback("state=STATE123", "reqtoken111", "STATE123").unwrap_err();
    assert!(
        denied.to_string().contains("consent denied"),
        "expected consent-denied guidance, got: {denied}"
    );

    // A WRONG (but present) oauth_token must still be reported as a token mismatch.
    let mismatched = tba::parse_tba_callback(query, "OTHERTOKEN", "STATE123").unwrap_err();
    assert!(
        mismatched.to_string().contains("different request token"),
        "expected token-mismatch error, got: {mismatched}"
    );

    // Correct token and state but no oauth_verifier — exercises the missing-verifier branch
    // specifically, distinct from the no-oauth_token case above (that query is missing
    // oauth_token entirely, so it hits the consent-denied branch rather than the verifier check).
    let no_verifier = tba::parse_tba_callback(
        "oauth_token=reqtoken111&state=STATE123",
        "reqtoken111",
        "STATE123",
    )
    .unwrap_err();
    assert!(
        no_verifier.to_string().contains("consent denied"),
        "expected consent-denied guidance, got: {no_verifier}"
    );
}

#[test]
fn authorize_url_and_state_meet_netsuite_rules() {
    let url = tba::tba_authorize_url(
        "https://1234567-sb1.app.netsuite.com",
        "req token",
        "STATE123",
    );
    assert!(
        url.starts_with("https://1234567-sb1.app.netsuite.com/app/login/secure/authorizetoken.nl?")
    );
    assert!(url.contains("oauth_token=req+token") || url.contains("oauth_token=req%20token"));
    assert!(url.contains("state=STATE123"));

    let state = tba::generate_tba_state();
    assert_eq!(state.len(), 32);
    assert!(state.chars().all(|ch| ch.is_ascii_alphanumeric()));
}
