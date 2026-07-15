pub mod envelope;
pub mod parse;
pub mod search_types;

use crate::auth::tba;
use crate::error::CliError;
use crate::secrets::TbaSecrets;
use envelope::TokenPassport;
use parse::{SoapSearchResult, parse_search_response};
use search_types::SearchType;

#[derive(Debug)]
pub struct SoapClient {
    http: reqwest::Client,
    endpoint: String,
    account_id: String,
    consumer_key: String,
    consumer_secret: String,
    token_id: String,
    token_secret: String,
}

impl SoapClient {
    pub fn new(
        http: reqwest::Client,
        base: &str,
        account_id: &str,
        secrets: TbaSecrets,
    ) -> Result<SoapClient, CliError> {
        let (Some(token_id), Some(token_secret)) = (secrets.token_id, secrets.token_secret) else {
            return Err(CliError::Auth(
                "no SOAP token minted for this account; run `netsuite-cli account soap-auth <alias>`".into(),
            ));
        };
        Ok(SoapClient {
            http,
            endpoint: format!(
                "{}/services/NetSuitePort_2025_2",
                base.trim_end_matches('/')
            ),
            account_id: account_id.to_string(),
            consumer_key: secrets.consumer_key,
            consumer_secret: secrets.consumer_secret,
            token_id,
            token_secret,
        })
    }

    pub async fn search(
        &self,
        search_type: &SearchType,
        id_attribute: &str,
        saved_search_id: &str,
        page_size: u64,
    ) -> Result<SoapSearchResult, CliError> {
        let envelope = envelope::search_envelope(
            &self.passport(),
            page_size,
            search_type,
            id_attribute,
            saved_search_id,
        );
        self.post("search", envelope).await
    }

    pub async fn search_more(
        &self,
        search_id: &str,
        page_index: u64,
    ) -> Result<SoapSearchResult, CliError> {
        let envelope = envelope::search_more_envelope(&self.passport(), search_id, page_index);
        self.post("searchMoreWithId", envelope).await
    }

    fn passport(&self) -> TokenPassport {
        let nonce = tba::generate_nonce();
        let timestamp = tba::epoch_seconds();
        let signature = tba::token_passport_signature(
            &self.account_id,
            &self.consumer_key,
            &self.token_id,
            &nonce,
            timestamp,
            &self.consumer_secret,
            &self.token_secret,
        );
        TokenPassport {
            account_id: self.account_id.clone(),
            consumer_key: self.consumer_key.clone(),
            token_id: self.token_id.clone(),
            nonce,
            timestamp,
            signature,
        }
    }

    async fn post(
        &self,
        soap_action: &str,
        envelope: String,
    ) -> Result<SoapSearchResult, CliError> {
        let response = self
            .http
            .post(&self.endpoint)
            .header("Content-Type", "text/xml; charset=utf-8")
            .header("SOAPAction", format!("\"{soap_action}\""))
            .body(envelope)
            .send()
            .await
            .map_err(|send_error| {
                CliError::Network(format!(
                    "SOAP request to {} failed: {send_error}",
                    self.endpoint
                ))
            })?;
        let http_status = response.status().as_u16();
        let body = response.text().await.map_err(|read_error| {
            CliError::Network(format!("reading SOAP response failed: {read_error}"))
        })?;
        // Faults arrive with HTTP 500 but the body is authoritative either way.
        if body.contains("<") {
            return parse_search_response(&body);
        }
        Err(CliError::Api {
            status: http_status,
            message: body,
            details: vec![],
        })
    }
}
