use serde_json::{Value, json};

use crate::client::NsClient;
use crate::error::CliError;

const DEFAULT_PAGE_LIMIT: u64 = 1000;

/// NetSuite's documented maximum page count for a single listing traversal.
const MAX_PAGES: u32 = 1000;

pub async fn run(
    client: &NsClient,
    query_text: &str,
    limit: Option<u64>,
    offset: Option<u64>,
    fetch_all: bool,
) -> Result<Value, CliError> {
    let page_limit = limit.unwrap_or(DEFAULT_PAGE_LIMIT);
    let mut page_offset = offset.unwrap_or(0);
    let mut merged_items: Vec<Value> = Vec::new();
    let mut pages_fetched: u32 = 0;
    let body = json!({"q": query_text});
    loop {
        let query: Vec<(&str, String)> = vec![
            ("limit", page_limit.to_string()),
            ("offset", page_offset.to_string()),
        ];
        let response = client
            .request(
                reqwest::Method::POST,
                "/services/rest/query/v1/suiteql",
                &query,
                &[("Prefer", "transient")],
                Some(&body),
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
