use serde_json::Value;

use crate::client::NsClient;
use crate::error::CliError;

pub async fn call(
    client: &NsClient,
    restlet_base: &str,
    script: &str,
    deploy: &str,
    http_method: reqwest::Method,
    params: &[(String, String)],
    body: Option<Value>,
) -> Result<Value, CliError> {
    let restlet_url = format!("{restlet_base}/app/site/hosting/restlet.nl");
    let mut query: Vec<(&str, String)> = vec![
        ("script", script.to_string()),
        ("deploy", deploy.to_string()),
    ];
    for (param_name, param_value) in params {
        query.push((param_name.as_str(), param_value.clone()));
    }
    let response = client
        .request(http_method, &restlet_url, &query, &[], body.as_ref())
        .await?;
    Ok(response.body.unwrap_or(Value::Null))
}
