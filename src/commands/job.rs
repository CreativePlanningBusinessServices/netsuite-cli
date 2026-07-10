use serde_json::{Value, json};

use crate::client::NsClient;
use crate::error::CliError;

pub async fn submit(
    client: &NsClient,
    http_method: reqwest::Method,
    path: &str,
    body: Option<Value>,
    idempotency_key: Option<String>,
) -> Result<Value, CliError> {
    let mut headers: Vec<(&str, &str)> = vec![("Prefer", "respond-async")];
    if let Some(ref key) = idempotency_key {
        headers.push(("X-NetSuite-Idempotency-Key", key.as_str()));
    }
    let response = client
        .request(http_method, path, &[], &headers, body.as_ref())
        .await?;
    let Some(location) = response.location else {
        return Err(CliError::Api {
            status: response.status,
            message: "job submit succeeded but no Location header was returned; job id unknown"
                .into(),
            details: vec![],
        });
    };
    let job_id = location.rsplit('/').next().unwrap_or_default().to_string();
    Ok(json!({"jobId": job_id, "location": location, "status": response.status}))
}

pub async fn status(client: &NsClient, job_id: &str) -> Result<Value, CliError> {
    let response = client
        .request(
            reqwest::Method::GET,
            &format!("/services/rest/async/v1/job/{job_id}"),
            &[],
            &[],
            None,
        )
        .await?;
    Ok(response.body.unwrap_or(Value::Null))
}

pub async fn tasks(client: &NsClient, job_id: &str) -> Result<Value, CliError> {
    let response = client
        .request(
            reqwest::Method::GET,
            &format!("/services/rest/async/v1/job/{job_id}/task/"),
            &[],
            &[],
            None,
        )
        .await?;
    Ok(response.body.unwrap_or(Value::Null))
}

pub async fn result(
    client: &NsClient,
    job_id: &str,
    task_id: Option<String>,
) -> Result<Value, CliError> {
    let resolved_task_id = match task_id {
        Some(explicit_task_id) => explicit_task_id,
        None => resolve_single_task_id(client, job_id).await?,
    };
    let response = client
        .request(
            reqwest::Method::GET,
            &format!("/services/rest/async/v1/job/{job_id}/task/{resolved_task_id}/result"),
            &[],
            &[],
            None,
        )
        .await?;
    Ok(response.body.unwrap_or(Value::Null))
}

/// `job result` without `--task` is only unambiguous when the job has exactly one task;
/// otherwise the caller must disambiguate.
async fn resolve_single_task_id(client: &NsClient, job_id: &str) -> Result<String, CliError> {
    let task_list = tasks(client, job_id).await?;
    let task_ids = extract_task_ids(&task_list);
    match task_ids.as_slice() {
        [single_task_id] => Ok(single_task_id.clone()),
        _ => Err(CliError::Usage(format!(
            "job {job_id} has {} task(s) ({}); pass --task to select one",
            task_ids.len(),
            task_ids.join(", ")
        ))),
    }
}

/// The task-list schema only guarantees `links` with a self href ending in
/// `.../task/{taskId}`; `id` is a speculative convenience field some responses
/// omit, so it falls back to deriving the id from the self link's href.
fn extract_task_ids(task_list: &Value) -> Vec<String> {
    task_list["items"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(extract_task_id)
        .collect()
}

fn extract_task_id(item: &Value) -> Option<String> {
    if let Some(id) = item["id"].as_str() {
        return Some(id.to_string());
    }
    let href = self_link_href(item)?;
    let task_id = href.rsplit('/').next().unwrap_or_default();
    if task_id.is_empty() {
        None
    } else {
        Some(task_id.to_string())
    }
}

fn self_link_href(item: &Value) -> Option<&str> {
    let links = item["links"].as_array()?;
    let self_link = links
        .iter()
        .find(|link| link["rel"] == "self")
        .or_else(|| links.first())?;
    self_link["href"].as_str()
}
