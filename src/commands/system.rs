use serde_json::Value;

use crate::client::NsClient;
use crate::error::CliError;

pub async fn server_time(client: &NsClient) -> Result<Value, CliError> {
    let response = client
        .request(
            reqwest::Method::GET,
            "/services/rest/system/v1/serverTime",
            &[],
            &[],
            None,
        )
        .await?;
    Ok(response.body.unwrap_or(Value::Null))
}

pub async fn governance_limits(client: &NsClient) -> Result<Value, CliError> {
    let response = client
        .request(
            reqwest::Method::GET,
            "/services/rest/system/v1/governanceLimits",
            &[],
            &[],
            None,
        )
        .await?;
    Ok(response.body.unwrap_or(Value::Null))
}
