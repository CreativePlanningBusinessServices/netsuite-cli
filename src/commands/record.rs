use serde_json::{Value, json};

use crate::client::NsClient;
use crate::error::CliError;

const DEFAULT_PAGE_LIMIT: u64 = 1000;

pub async fn get(
    client: &NsClient,
    record_type: &str,
    record_id: &str,
    fields: Option<String>,
    expand_sub_resources: bool,
) -> Result<Value, CliError> {
    let mut query: Vec<(&str, String)> = Vec::new();
    if let Some(field_list) = fields {
        query.push(("fields", field_list));
    }
    if expand_sub_resources {
        query.push(("expandSubResources", "true".into()));
    }
    let response = client
        .request(
            reqwest::Method::GET,
            &format!("/services/rest/record/v1/{record_type}/{record_id}"),
            &query,
            &[],
            None,
        )
        .await?;
    Ok(response.body.unwrap_or(Value::Null))
}

pub async fn list(
    client: &NsClient,
    record_type: &str,
    q_filter: Option<String>,
    limit: Option<u64>,
    offset: Option<u64>,
    fetch_all: bool,
) -> Result<Value, CliError> {
    let page_limit = limit.unwrap_or(DEFAULT_PAGE_LIMIT);
    let mut page_offset = offset.unwrap_or(0);
    let mut merged_items: Vec<Value> = Vec::new();
    loop {
        let mut query: Vec<(&str, String)> = vec![
            ("limit", page_limit.to_string()),
            ("offset", page_offset.to_string()),
        ];
        if let Some(ref filter) = q_filter {
            query.push(("q", filter.clone()));
        }
        let response = client
            .request(
                reqwest::Method::GET,
                &format!("/services/rest/record/v1/{record_type}"),
                &query,
                &[],
                None,
            )
            .await?;
        let page = response.body.unwrap_or(Value::Null);
        if !fetch_all {
            return Ok(page);
        }
        merged_items.extend(page["items"].as_array().cloned().unwrap_or_default());
        if page["hasMore"].as_bool() != Some(true) {
            let total = page["totalResults"].clone();
            return Ok(json!({
                "items": merged_items, "count": merged_items.len(),
                "hasMore": false, "totalResults": total,
            }));
        }
        page_offset += page_limit;
    }
}

pub async fn create(client: &NsClient, record_type: &str, body: Value) -> Result<Value, CliError> {
    let response = client
        .request(
            reqwest::Method::POST,
            &format!("/services/rest/record/v1/{record_type}"),
            &[],
            &[],
            Some(&body),
        )
        .await?;
    let location = response.location.unwrap_or_default();
    let new_id = location.rsplit('/').next().unwrap_or_default().to_string();
    Ok(json!({"id": new_id, "location": location}))
}

pub async fn update(
    client: &NsClient,
    record_type: &str,
    record_id: &str,
    body: Value,
    replace_sublists: Option<String>,
) -> Result<Value, CliError> {
    let mut query: Vec<(&str, String)> = Vec::new();
    if let Some(sublists) = replace_sublists {
        query.push(("replace", sublists));
    }
    client
        .request(
            reqwest::Method::PATCH,
            &format!("/services/rest/record/v1/{record_type}/{record_id}"),
            &query,
            &[],
            Some(&body),
        )
        .await?;
    Ok(json!({"updated": true, "id": record_id}))
}

pub async fn upsert(
    client: &NsClient,
    record_type: &str,
    external_id: &str,
    body: Value,
) -> Result<Value, CliError> {
    client
        .request(
            reqwest::Method::PUT,
            &format!("/services/rest/record/v1/{record_type}/eid:{external_id}"),
            &[],
            &[],
            Some(&body),
        )
        .await?;
    Ok(json!({"upserted": true, "externalId": external_id}))
}

pub async fn delete(
    client: &NsClient,
    record_type: &str,
    record_id: &str,
) -> Result<Value, CliError> {
    client
        .request(
            reqwest::Method::DELETE,
            &format!("/services/rest/record/v1/{record_type}/{record_id}"),
            &[],
            &[],
            None,
        )
        .await?;
    Ok(json!({"deleted": true, "id": record_id}))
}
