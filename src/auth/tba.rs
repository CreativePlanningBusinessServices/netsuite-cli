use std::collections::HashMap;

use base64::Engine;
use hmac::{Hmac, Mac};
use rand::Rng;
use sha2::Sha256;

use crate::error::CliError;

#[derive(Debug)]
pub struct RequestToken {
    pub token: String,
    pub secret: String,
}

#[derive(Debug)]
pub struct MintedToken {
    pub token_id: String,
    pub token_secret: String,
}

/// Interactive TBA (Token-Based Authentication) authorization: obtains a request token,
/// sends the user to NetSuite to authorize it, then exchanges the resulting verifier for a
/// minted access token. Mirrors authcode::run_login_flow's shape (see that function for the
/// loopback-vs-paste rationale) but drives NetSuite's three-step OAuth 1.0a REST endpoints
/// instead of OAuth 2.0. Not directly unit-testable since it opens a browser and blocks on a
/// redirect — the step functions and pure helpers below carry the test coverage instead.
pub async fn run_tba_flow(
    http: &reqwest::Client,
    restlet_base: &str,
    app_base: &str,
    consumer_key: &str,
    consumer_secret: &str,
    port: u16,
    paste_mode: bool,
) -> Result<MintedToken, CliError> {
    let callback_url = format!("https://localhost:{port}/callback");
    let request_token = obtain_request_token(
        http,
        restlet_base,
        consumer_key,
        consumer_secret,
        &callback_url,
    )
    .await?;
    let state = generate_tba_state();
    let authorize = tba_authorize_url(app_base, &request_token.token, &state);
    eprintln!(
        "Open this URL to authorize SOAP access (or it will open automatically):\n{authorize}"
    );
    let _ = webbrowser::open(&authorize);

    let verifier = if paste_mode {
        crate::auth::loopback::read_pasted_redirect(|query| {
            parse_tba_callback(query, &request_token.token, &state)
        })?
    } else {
        crate::auth::loopback::listen_for_redirect(port, |query| {
            parse_tba_callback(query, &request_token.token, &state)
        })
        .await?
    };

    exchange_for_access_token(
        http,
        restlet_base,
        consumer_key,
        consumer_secret,
        &request_token,
        &verifier,
    )
    .await
}

pub async fn obtain_request_token(
    http: &reqwest::Client,
    restlet_base: &str,
    consumer_key: &str,
    consumer_secret: &str,
    callback_url: &str,
) -> Result<RequestToken, CliError> {
    let url = format!("{}/rest/requesttoken", restlet_base.trim_end_matches('/'));
    let oauth_params: Vec<(&str, String)> = vec![
        ("oauth_callback", callback_url.to_string()),
        ("oauth_consumer_key", consumer_key.to_string()),
        ("oauth_nonce", generate_nonce()),
        ("oauth_signature_method", "HMAC-SHA256".to_string()),
        ("oauth_timestamp", epoch_seconds().to_string()),
        ("oauth_version", "1.0".to_string()),
    ];
    let pairs = signed_form_post(http, &url, oauth_params, consumer_secret, "").await?;
    if pairs.get("oauth_callback_confirmed").map(String::as_str) != Some("true") {
        return Err(CliError::Auth(
            "NetSuite did not confirm the TBA callback; check the integration record's callback URL"
                .into(),
        ));
    }
    Ok(RequestToken {
        token: required(&pairs, "oauth_token")?,
        secret: required(&pairs, "oauth_token_secret")?,
    })
}

pub async fn exchange_for_access_token(
    http: &reqwest::Client,
    restlet_base: &str,
    consumer_key: &str,
    consumer_secret: &str,
    request_token: &RequestToken,
    verifier: &str,
) -> Result<MintedToken, CliError> {
    let url = format!("{}/rest/accesstoken", restlet_base.trim_end_matches('/'));
    let oauth_params: Vec<(&str, String)> = vec![
        ("oauth_consumer_key", consumer_key.to_string()),
        ("oauth_nonce", generate_nonce()),
        ("oauth_signature_method", "HMAC-SHA256".to_string()),
        ("oauth_timestamp", epoch_seconds().to_string()),
        ("oauth_token", request_token.token.clone()),
        ("oauth_verifier", verifier.to_string()),
        ("oauth_version", "1.0".to_string()),
    ];
    let pairs = signed_form_post(
        http,
        &url,
        oauth_params,
        consumer_secret,
        &request_token.secret,
    )
    .await?;
    Ok(MintedToken {
        token_id: required(&pairs, "oauth_token")?,
        token_secret: required(&pairs, "oauth_token_secret")?,
    })
}

async fn signed_form_post(
    http: &reqwest::Client,
    url: &str,
    oauth_params: Vec<(&str, String)>,
    consumer_secret: &str,
    token_secret: &str,
) -> Result<HashMap<String, String>, CliError> {
    let signature = oauth_signature("POST", url, &oauth_params, consumer_secret, token_secret);
    let authorization = oauth_authorization_header(&oauth_params, &signature);
    let response = http
        .post(url)
        .header("Authorization", authorization)
        .send()
        .await
        .map_err(|send_error| {
            CliError::Network(format!("TBA request to {url} failed: {send_error}"))
        })?;
    let status = response.status();
    let body = response.text().await.map_err(|read_error| {
        CliError::Network(format!("reading TBA response failed: {read_error}"))
    })?;
    if !status.is_success() {
        return Err(CliError::Auth(format!(
            "TBA endpoint {url} returned {status}: {body}"
        )));
    }
    Ok(url::form_urlencoded::parse(body.as_bytes())
        .map(|(name, value)| (name.into_owned(), value.into_owned()))
        .collect())
}

pub fn tba_authorize_url(app_base: &str, request_token: &str, state: &str) -> String {
    let mut url = url::Url::parse(&format!("{app_base}/app/login/secure/authorizetoken.nl"))
        .expect("valid base");
    url.query_pairs_mut()
        .append_pair("oauth_token", request_token)
        .append_pair("state", state);
    url.to_string()
}

pub fn parse_tba_callback(
    query: &str,
    expected_token: &str,
    expected_state: &str,
) -> Result<String, CliError> {
    let pairs: HashMap<String, String> = url::form_urlencoded::parse(query.as_bytes())
        .map(|(name, value)| (name.into_owned(), value.into_owned()))
        .collect();
    if pairs.get("state").map(String::as_str) != Some(expected_state) {
        return Err(CliError::Auth(
            "state mismatch in TBA callback — possible CSRF, aborting".into(),
        ));
    }
    if pairs.get("oauth_token").map(String::as_str) != Some(expected_token) {
        return Err(CliError::Auth(
            "TBA callback returned a different request token, aborting".into(),
        ));
    }
    pairs
        .get("oauth_verifier")
        .cloned()
        .ok_or_else(|| CliError::Auth("no oauth_verifier in TBA callback (consent denied?)".into()))
}

pub fn generate_tba_state() -> String {
    const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::rng();
    (0..32)
        .map(|_| CHARSET[rng.random_range(0..CHARSET.len())] as char)
        .collect()
}

fn required(pairs: &HashMap<String, String>, field: &str) -> Result<String, CliError> {
    pairs
        .get(field)
        .cloned()
        .ok_or_else(|| CliError::Auth(format!("TBA response missing expected field '{field}'")))
}

pub fn token_passport_signature(
    account_id: &str,
    consumer_key: &str,
    token_id: &str,
    nonce: &str,
    timestamp: u64,
    consumer_secret: &str,
    token_secret: &str,
) -> String {
    let base_string = format!("{account_id}&{consumer_key}&{token_id}&{nonce}&{timestamp}");
    let signing_key = format!("{consumer_secret}&{token_secret}");
    hmac_sha256_base64(&signing_key, &base_string)
}

pub fn oauth_signature(
    http_method: &str,
    url: &str,
    params: &[(&str, String)],
    consumer_secret: &str,
    token_secret: &str,
) -> String {
    let mut sorted_params: Vec<(&str, &str)> = params
        .iter()
        .map(|(name, value)| (*name, value.as_str()))
        .collect();
    sorted_params.sort();
    let parameter_string = sorted_params
        .iter()
        .map(|(name, value)| format!("{}={}", percent_encode(name), percent_encode(value)))
        .collect::<Vec<_>>()
        .join("&");
    let base_string = format!(
        "{http_method}&{}&{}",
        percent_encode(url),
        percent_encode(&parameter_string)
    );
    let signing_key = format!(
        "{}&{}",
        percent_encode(consumer_secret),
        percent_encode(token_secret)
    );
    hmac_sha256_base64(&signing_key, &base_string)
}

pub fn oauth_authorization_header(params: &[(&str, String)], signature: &str) -> String {
    let mut header_params: Vec<String> = params
        .iter()
        .map(|(name, value)| format!(r#"{}="{}""#, name, percent_encode(value)))
        .collect();
    header_params.push(format!(
        r#"oauth_signature="{}""#,
        percent_encode(signature)
    ));
    format!("OAuth {}", header_params.join(", "))
}

pub fn hmac_sha256_base64(key: &str, message: &str) -> String {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(key.as_bytes()).expect("HMAC accepts keys of any length");
    mac.update(message.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
}

pub fn percent_encode(raw: &str) -> String {
    raw.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

pub fn generate_nonce() -> String {
    const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::rng();
    (0..20)
        .map(|_| CHARSET[rng.random_range(0..CHARSET.len())] as char)
        .collect()
}

pub fn epoch_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock after 1970")
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_passport_signature_matches_reference_vector() {
        // base string: account&consumerKey&token&nonce&timestamp
        // key:         consumerSecret&tokenSecret
        let signature = token_passport_signature(
            "1234567_SB1",
            "consumerkey123",
            "tokenid456",
            "ABCDEFGHIJKLMNOPQRST",
            1_700_000_000,
            "consumersecret789",
            "tokensecret012",
        );
        assert_eq!(signature, "8VL7CiUwRAgjw1e1P/e6GkZsWf1Bmc6T3J+DXl7WLc0=");
    }

    #[test]
    fn oauth_signature_matches_reference_vector_for_request_token() {
        let params = [
            (
                "oauth_callback",
                "https://localhost:8899/callback".to_string(),
            ),
            ("oauth_consumer_key", "consumerkey123".to_string()),
            ("oauth_nonce", "ABCDEFGHIJKLMNOPQRST".to_string()),
            ("oauth_signature_method", "HMAC-SHA256".to_string()),
            ("oauth_timestamp", "1700000000".to_string()),
            ("oauth_version", "1.0".to_string()),
        ];
        let signature = oauth_signature(
            "POST",
            "https://1234567-sb1.restlets.api.netsuite.com/rest/requesttoken",
            &params,
            "consumersecret789",
            "",
        );
        assert_eq!(signature, "5sgbCQr3mRLrl1Jgmkg/UnNLwzGofJ5jkktNvRVQlYE=");
    }

    #[test]
    fn oauth_signature_matches_reference_vector_for_access_token() {
        let params = [
            ("oauth_consumer_key", "consumerkey123".to_string()),
            ("oauth_nonce", "ABCDEFGHIJKLMNOPQRST".to_string()),
            ("oauth_signature_method", "HMAC-SHA256".to_string()),
            ("oauth_timestamp", "1700000000".to_string()),
            ("oauth_token", "reqtoken111".to_string()),
            ("oauth_verifier", "verifier222".to_string()),
            ("oauth_version", "1.0".to_string()),
        ];
        let signature = oauth_signature(
            "POST",
            "https://1234567-sb1.restlets.api.netsuite.com/rest/accesstoken",
            &params,
            "consumersecret789",
            "reqtokensecret333",
        );
        assert_eq!(signature, "lH8IsRMocNRdk2TflE43sLDNU0MYzbberQZJgokyLKI=");
    }

    #[test]
    fn percent_encoding_covers_rfc3986_reserved_characters() {
        assert_eq!(
            percent_encode("https://x/y?a=b&c"),
            "https%3A%2F%2Fx%2Fy%3Fa%3Db%26c"
        );
        assert_eq!(percent_encode("safe-._~AZaz09"), "safe-._~AZaz09");
        assert_eq!(percent_encode("sp ace+plus"), "sp%20ace%2Bplus");
    }

    #[test]
    fn nonce_is_twenty_alphanumeric_characters() {
        let nonce = generate_nonce();
        assert_eq!(nonce.len(), 20);
        assert!(nonce.chars().all(|ch| ch.is_ascii_alphanumeric()));
        assert_ne!(generate_nonce(), nonce);
    }

    #[test]
    fn authorization_header_percent_encodes_values_and_appends_signature() {
        let header = oauth_authorization_header(
            &[("oauth_consumer_key", "key/with/slash".to_string())],
            "sig+base64=",
        );
        assert!(header.starts_with("OAuth "));
        assert!(header.contains(r#"oauth_consumer_key="key%2Fwith%2Fslash""#));
        assert!(header.ends_with(r#"oauth_signature="sig%2Bbase64%3D""#));
    }
}
