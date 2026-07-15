use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use rand::Rng;
use sha2::{Digest, Sha256};

use crate::auth::loopback;
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
        match &token.refresh_token {
            Some(rotated_refresh) => {
                self.store
                    .set(
                        &self.alias,
                        &AccountSecrets::AuthCode {
                            client_id: self.client_id.clone(),
                            refresh_token: Some(rotated_refresh.clone()),
                        },
                    )
                    .map_err(|store_error| {
                        // NetSuite has already invalidated the old refresh token server-side by
                        // this point (refresh tokens are one-time-use), so there's no safe
                        // fallback — the fresh access token we hold in memory can't be persisted
                        // for reuse either without the rotated refresh token to pair it with next
                        // time. Make the account's broken state explicit instead of surfacing a
                        // bare keychain error that gives no indication the account now needs
                        // re-authentication.
                        CliError::Auth(format!(
                            "NetSuite rotated the refresh token but saving it to the keychain \
                             failed ({store_error}); this account must be re-authenticated: run \
                             `netsuite-cli account add {} --flow auth-code …`",
                            self.alias
                        ))
                    })?;
            }
            None => {
                // NetSuite documents that it rotates the refresh token on every use, so a
                // successful refresh response without one is unexpected — but the old refresh
                // token is still one-time-use and has already been invalidated server-side by
                // this call regardless. Don't fail the command that just got a perfectly good
                // access token; warn instead so the eventual "refresh failed" on the next
                // expiry isn't a surprise with no history to explain it.
                eprintln!(
                    "warning: NetSuite's refresh response for '{}' did not include a new \
                     refresh token; the stored one is one-time-use and has already been \
                     invalidated, so the next refresh will likely fail — if it does, \
                     re-authenticate with `netsuite-cli account add {} --flow auth-code …`",
                    self.alias, self.alias
                );
            }
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
        loopback::read_pasted_redirect(|query| parse_callback_query(query, &state))?
    } else {
        loopback::listen_for_redirect(port, |query| parse_callback_query(query, &state)).await?
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
}
