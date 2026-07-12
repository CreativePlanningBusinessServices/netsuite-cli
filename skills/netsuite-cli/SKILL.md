---
name: netsuite-cli
description: Use when a task needs NetSuite data or metadata from the command line — reading or writing records, running SuiteQL queries, discovering record schemas, calling RESTlets, or driving async REST jobs. Covers picking the right subcommand, bootstrapping an account, workflow recipes, and triaging errors by exit code.
---

# netsuite-cli

Single-binary CLI for the NetSuite REST API, built for agents: JSON-only stdout,
structured JSON errors on stderr, deterministic exit codes. Full command
reference: `netsuite-cli <subcommand> --help` (examples are copy-paste correct)
and the repo README.

## Pick the right command

| You have / need | Use |
|---|---|
| Record type + internal id | `record get <type> <id>` |
| Query, aggregate, filter, join | `suiteql "SELECT ..."` |
| Unknown record type or field names | `describe --list`, then `describe <type>` |
| Create / update / delete a record | `record create` / `record update` / `record delete` |
| Upsert keyed on your own id | `record upsert <type> <externalId>` |
| An endpoint the CLI lacks | `raw <METHOD> /services/rest/...` |
| Long-running request | `job submit <METHOD> <path>` → `job status <id>` → `job result <id>` |
| Bulk create/update/delete (≤100 records) | `raw` batch collection — see **Batch** below (NOT `job submit`) |
| Deployed RESTlet script | `restlet call --script <N> --deploy <N> --method <M>` |

## Bootstrap (once per machine + account)

```bash
netsuite-cli --version || true   # not installed? README "Install" has release + cargo paths
netsuite-cli account list        # any accounts registered?
netsuite-cli account add <alias> --account-id <ID> --flow m2m \
    --client-id <CLIENT_ID> --cert-id <CERT_ID> --key <path/to/key.pem>
netsuite-cli account test --account <alias>   # proves auth end to end
```

- No credentials yet? The NetSuite-side setup (integration record + certificate
  upload) needs a human admin once — steps are in the README under "NetSuite setup".
- **M2M is the right flow for agents**: unattended, no browser, safe under
  parallel invocations. Auth-code acts as a named user but its refresh tokens
  are one-time-rotating — never run parallel commands against an auth-code
  account whose token may be expired.
- Account targeting: `--account <alias>` flag → `NETSUITE_ACCOUNT` env var →
  configured default (`account set-default`).

## Recipes

- **Discover before writing:** field names are camelCase and account-specific.
  `describe <type>` returns the JSON Schema; build `--data` bodies from it.
- **All rows:** add `--all` to `record list` / `suiteql`. If the merged result
  still says `"hasMore": true`, the server misbehaved and the data is partial —
  treat it as incomplete, not done.
- **Create returns** `{"id", "location"}` — the id is parsed from the Location
  header; an error means the record may not exist, so re-check before retrying.
- **SuiteQL values arrive as strings** (`"count": "143"`); cast after parsing.
- **`record list` items are id+link stubs** — fetch full rows via `record get`,
  or just use `suiteql` for bulk field reads.
- **Sublists on update:** `record update ... --replace item` replaces the whole
  `item` sublist; without `--replace`, body lines merge into existing ones.
- **Data input:** `--data '<json>'`, `--data @file.json`, or `--data -` (stdin).

## Batch / bulk (record collections)

NetSuite REST **does** support batch ops (verified end-to-end 2026-07-11). Reach
for them at hundreds–thousands of records; for a few dozen, a `record
update`/`create` loop is simpler and gives per-record confirmation. Endpoint
`/services/rest/record/v1/<type>`, **≤100 records/request, always async**.

Drive them with **`raw`, not `job submit`** — `job submit` auto-adds `Prefer:
respond-async` but takes no `--header`/`--query` and forces
`application/json`, so a collection body returns `400 INVALID_CONTENT`
(confirmed). `raw` lets you set both headers and `?ids=`.

```bash
# CREATE/UPDATE — PATCH updates need `id` inside each item; both headers required
netsuite-cli raw POST /services/rest/record/v1/<type> \
  --header 'Prefer: respond-async' \
  --header 'Content-Type: application/vnd.oracle.resource+json; type=collection' \
  --data '{"items":[{"name":"A"},{"name":"B"}]}'
# → {"location":".../async/v1/job/<N>","status":202}

# RETRIEVE / DELETE — no content-type, just the async Prefer header + ?ids=
netsuite-cli raw GET    /services/rest/record/v1/<type> --query expandRecords=true --query ids=1,2,3 --header 'Prefer: respond-async'
netsuite-cli raw DELETE /services/rest/record/v1/<type> --query ids=1,2 --header 'Prefer: respond-async'

# Track + collect: job status <N> (→ progress: succeeded), job tasks <N> (task links),
# then per task: raw GET /services/rest/async/v1/job/<N>/task/<T>/result  (has the record id/outcome)
```

- The collection **content-type override works** because the CLI applies
  `--header` before serializing (`.json()` only adds `application/json` if
  Content-Type is unset) — don't "simplify" it away.
- **Batch still fires per-record UserEvents** — bundling saves HTTP round-trips,
  not server-side script runs; it does not skip afterSubmit logic.

## Errors: exit code → action

| Exit | Kind | Action |
|---|---|---|
| 1 | API | Parse stderr JSON: `details[].["o:errorCode"]` (`NONEXISTENT_ID`, `INVALID_CONTENT`, …) says what to fix |
| 2 | usage | Re-run with `--help`; the examples are exact |
| 3 | auth | M2M: credentials wrong/revoked → re-run `account add`. Auth-code: refresh token expired → re-run `account add <alias> --flow auth-code ...` |
| 4 | network | Retries with backoff (429/5xx) already happened — the failure is real |

## Gotchas

- stdout is ALWAYS machine JSON; `--pretty` is for humans. Only `--help` /
  `--version` print human text.
- Windows: use EC P-256 keys — RSA PEMs exceed the Windows credential store
  size limit and `account add` will reject them.
- `raw GET /services/rest/record/v1/metadata-catalog --query select=<type>`
  returns a single object; omit `select` to get the `{"items": [...]}` list.
- HTTP methods parse case-insensitively (`GET` and `get` both work).
- **SuiteQL on custom records uses `id`, not `internalid`** — `SELECT
  internalid FROM customrecord_...` errors with `Unknown identifier
  'internalid'` (standard records accept both). Use `id` and it silently
  returns nothing on some shapes, so prefer `id` everywhere for custom records.
