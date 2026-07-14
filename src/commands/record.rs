use serde_json::{Value, json};

use crate::client::{NsClient, NsResponse};
use crate::error::CliError;

const DEFAULT_PAGE_LIMIT: u64 = 1000;
const CREATE_FORM_ACCEPT: &str = "application/vnd.oracle.resource+json; type=create-form";

pub async fn get(
    client: &NsClient,
    record_type: &str,
    record_id: &str,
    fields: Option<String>,
    expand_sub_resources: bool,
) -> Result<Value, CliError> {
    let query = view_query(fields, expand_sub_resources);
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

/// NetSuite's documented maximum page count for a single listing traversal.
const MAX_PAGES: u32 = 1000;

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
    let mut pages_fetched: u32 = 0;
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
        pages_fetched += 1;
        let page_item_count = page["items"].as_array().map(Vec::len).unwrap_or(0);
        merged_items.extend(page["items"].as_array().cloned().unwrap_or_default());

        let total = page["totalResults"].clone();
        let has_more = page["hasMore"].as_bool() == Some(true);
        if !has_more {
            return Ok(json!({
                "items": merged_items, "count": merged_items.len(),
                "hasMore": false, "totalResults": total,
                "offset": 0, "links": [],
            }));
        }
        // A page that claims more results but contributes nothing means the
        // server is misbehaving; stop rather than looping forever. Likewise,
        // never traverse past NetSuite's documented page cap.
        if page_item_count == 0 || pages_fetched >= MAX_PAGES {
            return Ok(json!({
                "items": merged_items, "count": merged_items.len(),
                "hasMore": true, "totalResults": total,
                "offset": 0, "links": [],
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
    created_record_result(response, "create")
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

/// Builds the fields/expandSubResources query pairs shared by `get` and the
/// form-preview operations.
fn view_query(fields: Option<String>, expand_sub_resources: bool) -> Vec<(&'static str, String)> {
    let mut query = Vec::new();
    if let Some(field_list) = fields {
        query.push(("fields", field_list));
    }
    if expand_sub_resources {
        query.push(("expandSubResources", "true".into()));
    }
    query
}

/// Shapes the `{"id", "location"}` result for operations whose success is a 204
/// with a Location header pointing at the record they created (create, transform).
fn created_record_result(response: NsResponse, operation: &str) -> Result<Value, CliError> {
    let Some(location) = response.location else {
        return Err(CliError::Api {
            status: response.status,
            message: format!(
                "{operation} succeeded but no Location header was returned; record id unknown"
            ),
            details: vec![],
        });
    };
    let new_id = location.rsplit('/').next().unwrap_or_default().to_string();
    Ok(json!({"id": new_id, "location": location}))
}

#[allow(clippy::too_many_arguments)]
pub async fn transform(
    client: &NsClient,
    source_type: &str,
    source_id: &str,
    target_type: &str,
    body: Option<Value>,
    form: bool,
    fields: Option<String>,
    expand_sub_resources: bool,
) -> Result<Value, CliError> {
    let path =
        format!("/services/rest/record/v1/{source_type}/{source_id}/!transform/{target_type}");
    let request_body = body.unwrap_or_else(|| json!({}));
    if form {
        let query = view_query(fields, expand_sub_resources);
        let response = client
            .request(
                reqwest::Method::POST,
                &path,
                &query,
                &[("Accept", CREATE_FORM_ACCEPT)],
                Some(&request_body),
            )
            .await?;
        return Ok(response.body.unwrap_or(Value::Null));
    }
    let response = client
        .request(reqwest::Method::POST, &path, &[], &[], Some(&request_body))
        .await?;
    created_record_result(response, "transform")
}

pub async fn attach(
    client: &NsClient,
    record_type: &str,
    record_id: &str,
    attach_type: &str,
    attach_id: &str,
    role: Option<String>,
) -> Result<Value, CliError> {
    let body = match &role {
        Some(role_id) => json!({"role": {"id": role_id}}),
        None => json!({}),
    };
    client
        .request(
            reqwest::Method::POST,
            &format!(
                "/services/rest/record/v1/{record_type}/{record_id}/!attach/{attach_type}/{attach_id}"
            ),
            &[],
            &[],
            Some(&body),
        )
        .await?;
    Ok(json!({
        "attached": true, "type": record_type, "id": record_id,
        "attachedType": attach_type, "attachedId": attach_id,
    }))
}

pub async fn detach(
    client: &NsClient,
    record_type: &str,
    record_id: &str,
    detach_type: &str,
    detach_id: &str,
) -> Result<Value, CliError> {
    client
        .request(
            reqwest::Method::POST,
            &format!(
                "/services/rest/record/v1/{record_type}/{record_id}/!detach/{detach_type}/{detach_id}"
            ),
            &[],
            &[],
            Some(&json!({})),
        )
        .await?;
    Ok(json!({
        "detached": true, "type": record_type, "id": record_id,
        "detachedType": detach_type, "detachedId": detach_id,
    }))
}
