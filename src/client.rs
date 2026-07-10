use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use crate::auth::TokenProvider;
use crate::error::CliError;

const MAX_ATTEMPTS: u32 = 4;
const BASE_BACKOFF_MILLIS: u64 = 500;

pub struct NsClient {
    http: reqwest::Client,
    base: String,
    tokens: Arc<dyn TokenProvider>,
}

#[derive(Debug)]
pub struct NsResponse {
    pub status: u16,
    pub body: Option<Value>,
    pub location: Option<String>,
}

impl NsClient {
    pub fn new(http: reqwest::Client, base: String, tokens: Arc<dyn TokenProvider>) -> NsClient {
        NsClient {
            http,
            base: base.trim_end_matches('/').to_string(),
            tokens,
        }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    pub async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        query: &[(&str, String)],
        headers: &[(&str, &str)],
        body: Option<&Value>,
    ) -> Result<NsResponse, CliError> {
        let url = if path.starts_with("https://") || path.starts_with("http://") {
            path.to_string()
        } else {
            format!("{}{}", self.base, path)
        };

        let mut reauthorized = false;
        let mut attempt = 0;
        loop {
            attempt += 1;
            let token = self.tokens.access_token().await?;
            let mut request = self
                .http
                .request(method.clone(), &url)
                .bearer_auth(&token)
                .query(query);
            for (name, value) in headers {
                request = request.header(*name, *value);
            }
            if let Some(json_body) = body {
                request = request.json(json_body);
            }

            let response = request.send().await.map_err(|send_error| {
                CliError::Network(format!("request to {url} failed: {send_error}"))
            })?;
            let status = response.status();

            if status.as_u16() == 401 && !reauthorized && attempt < MAX_ATTEMPTS {
                self.tokens.invalidate();
                reauthorized = true;
                continue;
            }
            if (status.as_u16() == 429 || status.is_server_error()) && attempt < MAX_ATTEMPTS {
                let delay = retry_after_seconds(&response)
                    .map(Duration::from_secs)
                    .unwrap_or_else(|| {
                        Duration::from_millis(BASE_BACKOFF_MILLIS * 2u64.pow(attempt - 1))
                    });
                tokio::time::sleep(delay).await;
                continue;
            }

            let location = response
                .headers()
                .get("location")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            let raw = response.text().await.map_err(|read_error| {
                CliError::Network(format!("reading response from {url} failed: {read_error}"))
            })?;
            let parsed: Option<Value> = if raw.trim().is_empty() {
                None
            } else {
                serde_json::from_str(&raw).ok()
            };

            if status.is_success() {
                return Ok(NsResponse {
                    status: status.as_u16(),
                    body: parsed,
                    location,
                });
            }
            return Err(api_error(status.as_u16(), parsed, raw));
        }
    }
}

fn retry_after_seconds(response: &reqwest::Response) -> Option<u64> {
    response
        .headers()
        .get("retry-after")?
        .to_str()
        .ok()?
        .parse()
        .ok()
}

fn api_error(status: u16, parsed: Option<Value>, raw: String) -> CliError {
    let Some(body) = parsed else {
        return CliError::Api {
            status,
            message: raw,
            details: vec![],
        };
    };
    let details: Vec<Value> = body["o:errorDetails"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let title = body["title"].as_str().unwrap_or("request failed");
    let first_detail = details
        .first()
        .and_then(|detail| detail["detail"].as_str())
        .unwrap_or("");
    let message = if first_detail.is_empty() {
        title.to_string()
    } else {
        format!("{title}: {first_detail}")
    };
    CliError::Api {
        status,
        message,
        details,
    }
}
