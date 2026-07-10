use std::path::Path;
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};

use crate::client::NsClient;
use crate::error::CliError;

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum MetadataFormat {
    Schema,
    Openapi,
}

impl MetadataFormat {
    fn accept(self) -> &'static str {
        match self {
            MetadataFormat::Schema => "application/schema+json",
            MetadataFormat::Openapi => "application/swagger+json",
        }
    }

    fn cache_suffix(self) -> &'static str {
        match self {
            MetadataFormat::Schema => "schema",
            MetadataFormat::Openapi => "openapi",
        }
    }
}

pub async fn list_types(client: &NsClient) -> Result<Value, CliError> {
    let response = client
        .request(
            reqwest::Method::GET,
            "/services/rest/record/v1/metadata-catalog",
            &[],
            &[("Accept", "application/json")],
            None,
        )
        .await?;
    let record_type_names: Vec<Value> = response.body.unwrap_or(Value::Null)["items"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|item| item["name"].as_str().map(Value::from))
        .collect();
    Ok(json!({"recordTypes": record_type_names}))
}

pub async fn describe_type(
    client: &NsClient,
    record_type: &str,
    format: MetadataFormat,
    cache_dir: &Path,
    refresh: bool,
    cache_ttl: Duration,
) -> Result<Value, CliError> {
    let cache_file = cache_dir.join(format!("{record_type}.{}.json", format.cache_suffix()));
    if !refresh && let Some(cached_metadata) = read_fresh_cache(&cache_file, cache_ttl) {
        return Ok(cached_metadata);
    }
    let response = client
        .request(
            reqwest::Method::GET,
            &format!("/services/rest/record/v1/metadata-catalog/{record_type}"),
            &[],
            &[("Accept", format.accept())],
            None,
        )
        .await?;
    let metadata = response.body.unwrap_or(Value::Null);
    let _ = std::fs::create_dir_all(cache_dir);
    let _ = std::fs::write(
        &cache_file,
        serde_json::to_vec(&metadata).expect("metadata is serializable"),
    );
    Ok(metadata)
}

fn read_fresh_cache(cache_file: &Path, cache_ttl: Duration) -> Option<Value> {
    let modified_at = std::fs::metadata(cache_file).ok()?.modified().ok()?;
    if SystemTime::now().duration_since(modified_at).ok()? > cache_ttl {
        return None;
    }
    serde_json::from_slice(&std::fs::read(cache_file).ok()?).ok()
}
