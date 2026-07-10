use std::future::Future;
use std::io::ErrorKind;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use rand::Rng;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

use crate::auth::{TokenProvider, TokenResponse};
use crate::error::CliError;
use crate::secrets::{AccountSecrets, CachedToken, SecretStore};

pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

pub fn generate_pkce() -> Pkce {
    let verifier = random_token(64);
    Pkce {
        challenge: challenge_for_verifier(&verifier),
        verifier,
    }
}

pub fn generate_state() -> String {
    random_token(32)
}

fn random_token(length: usize) -> String {
    const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";
    let mut rng = rand::rng();
    (0..length)
        .map(|_| CHARSET[rng.random_range(0..CHARSET.len())] as char)
        .collect()
}

fn challenge_for_verifier(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

pub fn authorize_url(
    app_base: &str,
    client_id: &str,
    redirect_uri: &str,
    scopes: &[String],
    state: &str,
    challenge: &str,
) -> String {
    let mut url =
        url::Url::parse(&format!("{app_base}/app/login/oauth2/authorize.nl")).expect("valid base");
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("scope", &scopes.join(" "))
        .append_pair("state", state)
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256");
    url.to_string()
}

pub fn parse_callback_query(query: &str, expected_state: &str) -> Result<String, CliError> {
    let pairs: Vec<(String, String)> = url::form_urlencoded::parse(query.as_bytes())
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    let lookup = |name: &str| {
        pairs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    };
    if let Some(oauth_error) = lookup("error") {
        return Err(CliError::Auth(format!(
            "authorization denied: {oauth_error}"
        )));
    }
    if lookup("state").as_deref() != Some(expected_state) {
        return Err(CliError::Auth(
            "state mismatch in OAuth callback — possible CSRF, aborting".into(),
        ));
    }
    lookup("code").ok_or_else(|| CliError::Auth("no authorization code in callback".into()))
}

pub async fn exchange_code(
    http: &reqwest::Client,
    token_url: &str,
    client_id: &str,
    code: &str,
    redirect_uri: &str,
    verifier: &str,
) -> Result<TokenResponse, CliError> {
    post_token(
        http,
        token_url,
        &[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("code_verifier", verifier),
            ("client_id", client_id),
        ],
    )
    .await
}

pub async fn refresh(
    http: &reqwest::Client,
    token_url: &str,
    client_id: &str,
    refresh_token: &str,
) -> Result<TokenResponse, CliError> {
    post_token(
        http,
        token_url,
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", client_id),
        ],
    )
    .await
}

async fn post_token(
    http: &reqwest::Client,
    token_url: &str,
    form: &[(&str, &str)],
) -> Result<TokenResponse, CliError> {
    let response = http
        .post(token_url)
        .form(form)
        .send()
        .await
        .map_err(|send_error| CliError::Network(format!("token request failed: {send_error}")))?;
    let status = response.status();
    let body = response.text().await.map_err(|read_error| {
        CliError::Network(format!("reading token response failed: {read_error}"))
    })?;
    if !status.is_success() {
        return Err(CliError::Auth(format!(
            "token endpoint returned {status}: {body}"
        )));
    }
    serde_json::from_str(&body)
        .map_err(|parse_error| CliError::Auth(format!("bad token response: {parse_error}")))
}

pub struct AuthCodeProvider {
    http: reqwest::Client,
    alias: String,
    token_url: String,
    client_id: String,
    store: Arc<dyn SecretStore>,
}

impl AuthCodeProvider {
    pub fn new(
        http: reqwest::Client,
        alias: String,
        token_url: String,
        client_id: String,
        store: Arc<dyn SecretStore>,
    ) -> Self {
        AuthCodeProvider {
            http,
            alias,
            token_url,
            client_id,
            store,
        }
    }

    async fn refresh_access_token(&self) -> Result<String, CliError> {
        let Some(AccountSecrets::AuthCode {
            refresh_token: Some(current_refresh),
            ..
        }) = self.store.get(&self.alias)?
        else {
            return Err(CliError::Auth(format!(
                "no refresh token for '{}'; run `netsuite-cli account add {} --flow auth-code …` to re-authenticate",
                self.alias, self.alias
            )));
        };
        let token = refresh(&self.http, &self.token_url, &self.client_id, &current_refresh)
            .await
            .map_err(|refresh_error| {
                CliError::Auth(format!(
                    "refresh failed ({refresh_error}); re-authenticate with `netsuite-cli account add {} --flow auth-code …`",
                    self.alias
                ))
            })?;
        if let Some(rotated_refresh) = &token.refresh_token {
            self.store.set(
                &self.alias,
                &AccountSecrets::AuthCode {
                    client_id: self.client_id.clone(),
                    refresh_token: Some(rotated_refresh.clone()),
                },
            )?;
        }
        let now_epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        self.store.set_token(
            &self.alias,
            &CachedToken {
                access_token: token.access_token.clone(),
                expires_at_epoch: now_epoch + token.expires_in,
            },
        )?;
        Ok(token.access_token)
    }
}

impl TokenProvider for AuthCodeProvider {
    fn access_token<'life>(
        &'life self,
    ) -> Pin<Box<dyn Future<Output = Result<String, CliError>> + Send + 'life>> {
        Box::pin(async move {
            let now_epoch = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();
            if let Some(cached) = self.store.get_token(&self.alias)?
                && cached.is_valid_at(now_epoch)
            {
                return Ok(cached.access_token);
            }
            self.refresh_access_token().await
        })
    }

    fn invalidate(&self) {
        let _ = self.store.delete_token(&self.alias);
    }
}

/// Interactive login: opens the browser at the authorize URL, then either reads the
/// pasted redirect URL (`paste_mode`) or runs a one-shot HTTPS loopback listener to
/// catch the redirect itself. NetSuite rejects plain `http://` redirect URIs, so the
/// listener terminates TLS with a throwaway self-signed cert for `localhost`.
pub async fn run_login_flow(
    http: &reqwest::Client,
    app_base: &str,
    token_url: &str,
    client_id: &str,
    port: u16,
    paste_mode: bool,
) -> Result<TokenResponse, CliError> {
    let pkce = generate_pkce();
    let state = generate_state();
    let redirect_uri = format!("https://localhost:{port}/callback");
    let scopes: Vec<String> = ["restlets", "rest_webservices", "suite_analytics"]
        .iter()
        .map(|scope| scope.to_string())
        .collect();
    let url = authorize_url(
        app_base,
        client_id,
        &redirect_uri,
        &scopes,
        &state,
        &pkce.challenge,
    );

    eprintln!("Open this URL to log in (or it will open automatically):\n{url}");
    let _ = webbrowser::open(&url);

    let code = if paste_mode {
        read_pasted_redirect(&state)?
    } else {
        listen_for_redirect(port, &state).await?
    };

    exchange_code(
        http,
        token_url,
        client_id,
        &code,
        &redirect_uri,
        &pkce.verifier,
    )
    .await
}

fn read_pasted_redirect(expected_state: &str) -> Result<String, CliError> {
    eprintln!("Paste the full redirect URL after logging in:");
    let mut pasted_line = String::new();
    std::io::stdin()
        .read_line(&mut pasted_line)
        .map_err(|read_error| CliError::Auth(format!("failed to read pasted URL: {read_error}")))?;
    let pasted_line = pasted_line.trim();
    let query = pasted_line
        .split_once('?')
        .map(|(_, query)| query)
        .unwrap_or(pasted_line);
    parse_callback_query(query, expected_state)
}

// A stalled or malicious local connection that opens the TLS session but never finishes
// sending its request must not be able to hang `account add` forever.
const CONNECTION_READ_TIMEOUT: Duration = Duration::from_secs(30);

async fn listen_for_redirect(port: u16, expected_state: &str) -> Result<String, CliError> {
    let acceptor = build_loopback_tls_acceptor()?;
    let listener = TcpListener::bind(("127.0.0.1", port))
        .await
        .map_err(|bind_error| {
            CliError::Network(format!(
                "cannot bind https://localhost:{port}: {bind_error}"
            ))
        })?;
    eprintln!("Waiting for the OAuth redirect on https://localhost:{port}/callback …");

    loop {
        let (tcp_stream, _peer_addr) = listener
            .accept()
            .await
            .map_err(|accept_error| CliError::Network(format!("accept failed: {accept_error}")))?;
        // A failed handshake is typically the browser's cert-warning probe connection —
        // ignore it and keep accepting rather than aborting the whole login attempt.
        let Ok(mut tls_stream) = acceptor.accept(tcp_stream).await else {
            continue;
        };

        let request_head =
            match tokio::time::timeout(CONNECTION_READ_TIMEOUT, read_request_head(&mut tls_stream))
                .await
            {
                Ok(Ok(head)) => head,
                Ok(Err(_)) | Err(_) => continue,
            };
        let Some(request_path) = request_line_path(&request_head) else {
            continue;
        };

        if !request_path.starts_with("/callback") {
            let _ = tls_stream.write_all(NOT_FOUND_RESPONSE).await;
            continue;
        }

        let query = request_path
            .split_once('?')
            .map(|(_, query)| query)
            .unwrap_or("");
        let callback_result = parse_callback_query(query, expected_state);

        let _ = tls_stream
            .write_all(callback_response(&callback_result).as_bytes())
            .await;
        let _ = tls_stream.shutdown().await;

        return callback_result;
    }
}

fn build_loopback_tls_acceptor() -> Result<TlsAcceptor, CliError> {
    let certified_key =
        rcgen::generate_simple_self_signed(vec!["localhost".into()]).map_err(|cert_error| {
            CliError::Auth(format!("cannot generate loopback TLS cert: {cert_error}"))
        })?;
    let cert_der: CertificateDer<'static> = certified_key.cert.der().clone();
    let key_der: PrivateKeyDer<'static> = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
        certified_key.key_pair.serialize_der(),
    ));

    let crypto_provider = Arc::new(rustls::crypto::ring::default_provider());
    let server_config = rustls::ServerConfig::builder_with_provider(crypto_provider)
        .with_safe_default_protocol_versions()
        .map_err(|config_error| {
            CliError::Auth(format!(
                "cannot select TLS protocol versions: {config_error}"
            ))
        })?
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)
        .map_err(|config_error| {
            CliError::Auth(format!("cannot build loopback TLS config: {config_error}"))
        })?;

    Ok(TlsAcceptor::from(Arc::new(server_config)))
}

const MAX_REQUEST_HEAD_BYTES: usize = 8192;

async fn read_request_head<Stream: AsyncReadExt + Unpin>(
    stream: &mut Stream,
) -> std::io::Result<String> {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 512];
    loop {
        let bytes_read = stream.read(&mut chunk).await?;
        if bytes_read == 0 {
            return Err(std::io::Error::new(
                ErrorKind::UnexpectedEof,
                "connection closed before request head",
            ));
        }
        buffer.extend_from_slice(&chunk[..bytes_read]);
        if buffer.windows(4).any(|window| window == b"\r\n\r\n")
            || buffer.len() >= MAX_REQUEST_HEAD_BYTES
        {
            break;
        }
    }
    Ok(String::from_utf8_lossy(&buffer).into_owned())
}

fn request_line_path(request_head: &str) -> Option<&str> {
    request_head.lines().next()?.split_whitespace().nth(1)
}

const NOT_FOUND_RESPONSE: &[u8] =
    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

/// Pure helper so the browser's callback page reflects the actual outcome instead of always
/// claiming success. Never interpolates raw query values into the body — only these two fixed,
/// safe strings are ever shown — so a malicious `?error=` or `?code=` value can't be reflected
/// into the response.
fn callback_response_body(callback_result: &Result<String, CliError>) -> &'static str {
    match callback_result {
        Ok(_) => "<html><body>Login complete \u{2014} return to your terminal.</body></html>",
        Err(_) => "<html><body>Login failed \u{2014} return to your terminal.</body></html>",
    }
}

fn callback_response(callback_result: &Result<String, CliError>) -> String {
    let body = callback_response_body(callback_result);
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_matches_rfc7636_test_vector() {
        let challenge = challenge_for_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk");
        assert_eq!(challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn generated_pkce_and_state_meet_netsuite_rules() {
        let pkce = generate_pkce();
        assert!(pkce.verifier.len() >= 43 && pkce.verifier.len() <= 128);
        assert!(
            pkce.verifier
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || "-._~".contains(ch))
        );
        let state = generate_state();
        assert!(state.len() >= 22 && state.len() <= 1024);
    }

    #[test]
    fn authorize_url_contains_all_required_params() {
        let url = authorize_url(
            "https://123456-sb1.app.netsuite.com",
            "cid",
            "https://localhost:8899/callback",
            &["rest_webservices".into(), "restlets".into()],
            "STATESTATESTATESTATE22",
            "CHALLENGE",
        );
        assert!(
            url.starts_with("https://123456-sb1.app.netsuite.com/app/login/oauth2/authorize.nl?")
        );
        for expected in [
            "response_type=code",
            "client_id=cid",
            "code_challenge_method=S256",
            "code_challenge=CHALLENGE",
            "state=STATESTATESTATESTATE22",
            "scope=rest_webservices+restlets",
            "redirect_uri=https%3A%2F%2Flocalhost%3A8899%2Fcallback",
        ] {
            assert!(url.contains(expected), "missing {expected} in {url}");
        }
    }

    #[test]
    fn callback_query_parsing_extracts_code_and_validates_state() {
        let parsed =
            parse_callback_query("code=abc123&state=EXPECTED&role=3&entity=9", "EXPECTED").unwrap();
        assert_eq!(parsed, "abc123");
        assert!(parse_callback_query("code=abc&state=WRONG", "EXPECTED").is_err());
        assert!(parse_callback_query("error=access_denied&state=EXPECTED", "EXPECTED").is_err());
    }

    #[test]
    fn callback_response_body_reflects_success_or_failure() {
        let success = callback_response_body(&Ok("abc123".to_string()));
        assert!(success.contains("Login complete"));

        let denied = callback_response_body(&Err(CliError::Auth(
            "authorization denied: access_denied".into(),
        )));
        assert!(denied.contains("Login failed"));
        assert!(!denied.contains("access_denied"));

        let state_mismatch = callback_response_body(&Err(CliError::Auth(
            "state mismatch in OAuth callback — possible CSRF, aborting".into(),
        )));
        assert!(state_mismatch.contains("Login failed"));
    }
}
