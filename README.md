# netsuite-cli

A comprehensive NetSuite REST API CLI tool, written in Rust, designed for use by AI agents on macOS and Windows.

Basecamp task: https://basecamp.com/2808802/projects/8218129/todos/518729835

## Goals

- Target a specific NetSuite account per invocation, with an easy mechanic to switch accounts.
- Remember OAuth credentials per account (secure local credential storage).
- Cover the NetSuite REST API surface — record endpoints, SuiteQL, and endpoint metadata.
- Agent-friendly output and ergonomics: machine-readable (JSON) responses, predictable exit codes, discoverable help.

## Reference documentation

- [NetSuite REST Web Services help](https://docs.oracle.com/en/cloud/saas/netsuite/ns-online-help/book_1559132836.html)
- [REST API Browser (record/v1, 2026.1)](https://system.netsuite.com/help/helpcenter/en_US/APIs/REST_API_Browser/record/v1/2026.1/index.html)
- Each endpoint's metadata is retrievable via the REST metadata catalog described in the help documentation.

## Development

```bash
cargo build
cargo test
cargo run -- --help
```
