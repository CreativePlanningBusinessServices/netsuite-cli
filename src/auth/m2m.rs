use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde_json::json;

use crate::auth::{TokenProvider, TokenResponse};
use crate::error::CliError;
use crate::secrets::{CachedToken, SecretStore};

pub struct M2mConfig {
    pub token_url: String,
    pub client_id: String,
    pub cert_id: String,
    pub private_key_pem: String,
    pub scopes: Vec<String>,
}

pub struct M2mProvider {
    http: reqwest::Client,
    alias: String,
    config: M2mConfig,
    store: Arc<dyn SecretStore>,
}

impl M2mProvider {
    pub fn new(
        http: reqwest::Client,
        alias: String,
        config: M2mConfig,
        store: Arc<dyn SecretStore>,
    ) -> Self {
        M2mProvider {
            http,
            alias,
            config,
            store,
        }
    }

    async fn fetch_fresh_token(&self) -> Result<String, CliError> {
        let now_epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let assertion = build_assertion(&self.config, now_epoch)?;
        let response = self
            .http
            .post(&self.config.token_url)
            .form(&[
                ("grant_type", "client_credentials"),
                (
                    "client_assertion_type",
                    "urn:ietf:params:oauth:client-assertion-type:jwt-bearer",
                ),
                ("client_assertion", assertion.as_str()),
            ])
            .send()
            .await
            .map_err(|send_error| {
                CliError::Network(format!("token request failed: {send_error}"))
            })?;

        let status = response.status();
        let body = response.text().await.map_err(|read_error| {
            CliError::Network(format!("reading token response failed: {read_error}"))
        })?;
        if !status.is_success() {
            return Err(CliError::Auth(format!(
                "token endpoint returned {status}: {body}"
            )));
        }
        let token: TokenResponse = serde_json::from_str(&body)
            .map_err(|parse_error| CliError::Auth(format!("bad token response: {parse_error}")))?;
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

impl TokenProvider for M2mProvider {
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
            self.fetch_fresh_token().await
        })
    }

    fn invalidate(&self) {
        let _ = self.store.delete_token(&self.alias);
    }
}

pub fn build_assertion(config: &M2mConfig, now_epoch: u64) -> Result<String, CliError> {
    let (encoding_key, algorithm) = encoding_key_for_pem(&config.private_key_pem)?;
    let mut header = Header::new(algorithm);
    header.kid = Some(config.cert_id.clone());
    let claims = json!({
        "iss": config.client_id,
        "scope": config.scopes,
        "aud": config.token_url,
        "iat": now_epoch,
        "exp": now_epoch + 3300, // must stay under the documented iat+3600 ceiling
        "jti": uuid::Uuid::new_v4().to_string(),
    });
    jsonwebtoken::encode(&header, &claims, &encoding_key)
        .map_err(|sign_error| CliError::Auth(format!("cannot sign assertion: {sign_error}")))
}

// NetSuite accepts PS256/384/512 and ES256/384/512 — never plain RS256.
fn encoding_key_for_pem(pem: &str) -> Result<(EncodingKey, Algorithm), CliError> {
    if let Ok(rsa_key) = EncodingKey::from_rsa_pem(pem.as_bytes()) {
        return Ok((rsa_key, Algorithm::PS256));
    }
    if let Ok(ec_key) = EncodingKey::from_ec_pem(pem.as_bytes()) {
        return Ok((ec_key, Algorithm::ES256));
    }
    Err(CliError::Auth(
        "private key is not a valid RSA or EC PEM".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    fn test_key_pem() -> String {
        rcgen::KeyPair::generate().unwrap().serialize_pem() // ECDSA P-256 by default
    }

    fn decode_segment(segment: &str) -> serde_json::Value {
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(segment)
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[test]
    fn assertion_has_verified_header_and_claims() {
        let config = M2mConfig {
            token_url:
                "https://123456-sb1.suitetalk.api.netsuite.com/services/rest/auth/oauth2/v1/token"
                    .into(),
            client_id: "myclientid".into(),
            cert_id: "certkid123".into(),
            private_key_pem: test_key_pem(),
            scopes: vec!["rest_webservices".into(), "restlets".into()],
        };
        let assertion = build_assertion(&config, 1_700_000_000).unwrap();
        let segments: Vec<&str> = assertion.split('.').collect();
        assert_eq!(segments.len(), 3);

        let header = decode_segment(segments[0]);
        assert_eq!(header["alg"], "ES256");
        assert_eq!(header["typ"], "JWT");
        assert_eq!(header["kid"], "certkid123");

        let claims = decode_segment(segments[1]);
        assert_eq!(claims["iss"], "myclientid");
        assert_eq!(claims["aud"], config.token_url);
        assert_eq!(
            claims["scope"],
            serde_json::json!(["rest_webservices", "restlets"])
        );
        assert_eq!(claims["iat"], 1_700_000_000_u64);
        assert_eq!(claims["exp"], 1_700_000_000_u64 + 3300);
        assert!(claims["jti"].as_str().unwrap().len() >= 16);
    }

    #[test]
    fn unparseable_key_is_an_auth_error() {
        let config = M2mConfig {
            token_url: "https://x/token".into(),
            client_id: "c".into(),
            cert_id: "k".into(),
            private_key_pem: "not a pem".into(),
            scopes: vec![],
        };
        assert!(matches!(
            build_assertion(&config, 0),
            Err(crate::error::CliError::Auth(_))
        ));
    }
}
