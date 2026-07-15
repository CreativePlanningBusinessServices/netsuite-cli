use serde_json::{Value, json};

use crate::error::CliError;
use crate::soap::SoapClient;
use crate::soap::parse::SoapSearchResult;
use crate::soap::search_types;

const DEFAULT_PAGE_SIZE: u64 = 1000;
const MIN_PAGE_SIZE: u64 = 5; // SOAP searchPreferences minimum
const MAX_PAGE_SIZE: u64 = 1000; // SOAP searchPreferences maximum
const MAX_PAGES: u64 = 1000; // same defensive cap as suiteql.rs

pub async fn run(
    soap: &SoapClient,
    saved_search_id: &str,
    record_type: &str,
    limit: Option<u64>,
    fetch_all: bool,
) -> Result<Value, CliError> {
    let search_type = search_types::lookup(record_type).ok_or_else(|| {
        CliError::Usage(format!(
            "unknown record type '{record_type}'; valid types: {}",
            search_types::known_type_names()
        ))
    })?;
    let id_attribute = saved_search_id_attribute(saved_search_id)?;
    let page_size = limit.unwrap_or(DEFAULT_PAGE_SIZE);
    if !(MIN_PAGE_SIZE..=MAX_PAGE_SIZE).contains(&page_size) {
        return Err(CliError::Usage(format!(
            "--limit must be between {MIN_PAGE_SIZE} and {MAX_PAGE_SIZE}, got {page_size}"
        )));
    }

    let first_page = soap
        .search(&search_type, id_attribute, saved_search_id, page_size)
        .await?;
    if !fetch_all {
        return Ok(page_output(&first_page, first_page.rows.clone()));
    }

    let mut merged_rows = first_page.rows.clone();
    let mut last_page_index = first_page.page_index;
    let mut pages_fetched: u64 = 1;
    while last_page_index < first_page.total_pages && pages_fetched < MAX_PAGES {
        let next = soap
            .search_more(&first_page.search_id, last_page_index + 1)
            .await?;
        pages_fetched += 1;
        // A page that doesn't advance the index means the server is misbehaving;
        // stop rather than looping forever (mirrors suiteql.rs's guard).
        if next.page_index <= last_page_index {
            return Ok(json!({
                "items": merged_rows, "count": merged_rows.len(),
                "totalRecords": first_page.total_records,
                "totalPages": first_page.total_pages,
                "pageIndex": last_page_index,
                "hasMore": true,
            }));
        }
        merged_rows.extend(next.rows);
        last_page_index = next.page_index;
    }
    Ok(json!({
        "items": merged_rows, "count": merged_rows.len(),
        "totalRecords": first_page.total_records,
        "totalPages": first_page.total_pages,
        "pageIndex": last_page_index,
        "hasMore": last_page_index < first_page.total_pages,
    }))
}

fn saved_search_id_attribute(saved_search_id: &str) -> Result<&'static str, CliError> {
    if saved_search_id.starts_with("customsearch") {
        Ok("savedSearchScriptId")
    } else if !saved_search_id.is_empty() && saved_search_id.chars().all(|ch| ch.is_ascii_digit()) {
        Ok("savedSearchId")
    } else {
        Err(CliError::Usage(format!(
            "saved-search id must be a numeric internal id or a customsearch_* script id, got '{saved_search_id}'"
        )))
    }
}

fn page_output(page: &SoapSearchResult, items: Vec<Value>) -> Value {
    json!({
        "items": items, "count": page.rows.len(),
        "totalRecords": page.total_records, "totalPages": page.total_pages,
        "pageIndex": page.page_index,
        "hasMore": page.page_index < page.total_pages,
    })
}
