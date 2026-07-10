//! Live smoke tests against a real NetSuite sandbox account.
//!
//! Unlike the rest of the test suite (wiremock only, no real keychain), these hit the actual
//! NetSuite REST API and the real OS keychain through `context_for`, using an account alias
//! that must already exist in the local config (see README's "NetSuite setup" sections for how
//! to create one with `netsuite-cli account add`). They are all `#[ignore]`d so `cargo test`
//! never runs them by accident; run them explicitly with:
//!
//!   NETSUITE_LIVE_ALIAS=<alias> cargo test --test live_smoke -- --ignored
//!
//! CI only compiles this file (`cargo test` builds ignored tests without running them); it
//! never has sandbox credentials to run them with.

use std::time::Duration;

use netsuite_cli::commands::describe::{self, MetadataFormat};
use netsuite_cli::commands::suiteql;
use netsuite_cli::context::context_for;

fn live_alias() -> String {
    std::env::var("NETSUITE_LIVE_ALIAS").expect(
        "NETSUITE_LIVE_ALIAS must be set to an existing account alias to run live smoke tests",
    )
}

/// Equivalent of `netsuite-cli account test`: resolves the alias through the real config +
/// keychain, mints a real OAuth token, and fetches the metadata catalog.
#[tokio::test]
#[ignore = "hits a real NetSuite sandbox; run with NETSUITE_LIVE_ALIAS=<alias> cargo test --test live_smoke -- --ignored"]
async fn metadata_catalog_is_reachable_with_stored_credentials() {
    let alias = live_alias();
    let context = context_for(Some(&alias)).expect("account context resolves from local config");

    let response = context
        .client
        .request(
            reqwest::Method::GET,
            "/services/rest/record/v1/metadata-catalog",
            &[("select", "currency".to_string())],
            &[("Accept", "application/json")],
            None,
        )
        .await
        .expect("metadata catalog request succeeds");

    let body = response
        .body
        .expect("metadata catalog response has a JSON body");

    // With ?select=, NetSuite returns the record type object directly; without it,
    // the response is an {"items": [...]} collection.
    let describes_currency = body["name"] == "currency"
        || body["items"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item["name"] == "currency"));
    assert!(
        describes_currency,
        "expected the currency record type in the catalog: {body}"
    );
}

/// Equivalent of `netsuite-cli suiteql "SELECT id FROM currency"`; exercises the
/// `Prefer: transient` SuiteQL path end to end.
#[tokio::test]
#[ignore = "hits a real NetSuite sandbox; run with NETSUITE_LIVE_ALIAS=<alias> cargo test --test live_smoke -- --ignored"]
async fn suiteql_select_id_from_currency_returns_rows() {
    let alias = live_alias();
    let context = context_for(Some(&alias)).expect("account context resolves from local config");

    let result = suiteql::run(
        &context.client,
        "SELECT id FROM currency",
        None,
        None,
        false,
    )
    .await
    .expect("suiteql query succeeds");

    let items = result["items"]
        .as_array()
        .expect("suiteql response has an items array");
    assert!(!items.is_empty(), "expected at least one currency row");
}

/// Equivalent of `netsuite-cli describe currency`; uses a tempdir for the metadata cache so
/// this test never touches the real `~/.cache/netsuite-cli` (or platform equivalent) directory.
#[tokio::test]
#[ignore = "hits a real NetSuite sandbox; run with NETSUITE_LIVE_ALIAS=<alias> cargo test --test live_smoke -- --ignored"]
async fn describe_currency_returns_schema_metadata() {
    let alias = live_alias();
    let context = context_for(Some(&alias)).expect("account context resolves from local config");
    let cache_dir = tempfile::tempdir().expect("tempdir for metadata cache");

    let metadata = describe::describe_type(
        &context.client,
        "currency",
        MetadataFormat::Schema,
        cache_dir.path(),
        false,
        Duration::from_secs(3600),
    )
    .await
    .expect("describe currency succeeds");

    assert_eq!(
        metadata["type"], "object",
        "expected a JSON Schema object for currency: {metadata}"
    );
}
