---
name: netsuite-cli
description: Use when a task needs NetSuite data or metadata from the command line — reading or writing records, running SuiteQL queries, running a saved search, discovering record schemas, calling RESTlets, or driving async REST jobs. Covers picking the right subcommand, bootstrapping an account, workflow recipes, and triaging errors by exit code.
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
| One sublist line or its subrecord | `record get <type> <id> --sub <sublist>/<lineId>[/<subrecord>]` |
| Query, aggregate, filter, join | `suiteql "SELECT ..."` |
| Run, or create/edit, a saved search definition (preferred) | `restlet call --script customscript_cp_saved_search_rl --deploy customdeploy_cp_saved_search_rl --method GET\|POST\|PUT ...` — see **Saved searches via RESTlet** below |
| Run an existing saved search as-is, legacy SOAP fallback | `saved-search run <id> --type <recordtype>` |
| Unknown record type or field names | `describe --list`, then `describe <type>` |
| Create / update / delete a record | `record create` / `record update` / `record delete` |
| Upsert keyed on your own id | `record upsert <type> <externalId>` |
| Turn one record into another (SO→invoice, order→fulfillment) | `record transform <srcType> <srcId> <targetType>` |
| Preview defaulted fields, write nothing | `record create-form <type>` / `record edit-form <type> <id>` / `record transform ... --form` |
| Valid dropdown values for a field | `record select-options <type> --fields <f1,f2>` |
| Link/unlink a contact or file | `record attach` / `record detach` |
| Server clock / concurrency limits | `system server-time` / `system governance-limits` |
| An endpoint the CLI lacks | `raw <METHOD> /services/rest/...` |
| Long-running request | `job submit <METHOD> <path>` → `job status <id>` → `job result <id>` |
| Bulk create/update/delete (≤100 records) | `raw` batch collection — see **Batch** below (NOT `job submit`) |
| Deployed RESTlet script | `restlet call --script <N> --deploy <N> --method <M>` |

## Bootstrap (once per machine + account)

```bash
netsuite-cli --version || true   # not installed? README "Install" has release + cargo paths
netsuite-cli account list        # any accounts registered?
netsuite-cli account test --account <alias>   # proves auth end to end
```

No account registered yet? Two valid paths:

**Have M2M credentials already** (client id + cert id + key file):

```bash
netsuite-cli account add <alias> --account-id <ID> --flow m2m \
    --client-id <CLIENT_ID> --cert-id <CERT_ID> --key <path/to/key.pem>
```

**Have only a browser login (built-in client ID):** release builds embed a prebuilt
integration's Client ID, making `--client-id` optional on every command. Authenticate via
auth-code with the built-in client ID, then use that access token to drive NetSuite's
certificate-rotation API and self-provision M2M — no NetSuite UI steps:

```bash
netsuite-cli account login bootstrap   # one browser login (a human must complete it); the account is discovered from the callback — no --account-id needed; records the user's entity/role ids
netsuite-cli account cert generate                                     # writes netsuite-m2m-key.pem (secret) + netsuite-m2m-cert.pem
netsuite-cli account cert upload --cert netsuite-m2m-cert.pem --account bootstrap
#   → {"certificateId": "..."}  (uploads via the certificate rotation API, mapping the
#      cert to the logged-in user + role; needs the "Manage own OAuth 2.0 Client
#      Credentials certificates" permission on that role)
netsuite-cli account add <alias> --account-id <ID> --flow m2m \
    --cert-id <certificateId> --key netsuite-m2m-key.pem               # client id defaults to the built-in one
netsuite-cli account test --account <alias>
```

The `bootstrap` auth-code account is itself a fully valid way to authenticate — keep using
it directly if M2M isn't needed. If the build has no built-in client ID (`account add`
says so), fall back to the README's "NetSuite setup" for the one-time integration-record
steps a human admin must do.

The built-in integration is **OAuth 2.0-only (REST)**. It does not cover `saved-search run`
(SOAP/TBA), which needs the consumer key/secret of a separate TBA-only integration record —
never the built-in client ID. When `account add` offers to chain SOAP setup and you don't
have that pair, answer `N`; everything except `saved-search run` works without it.

- This skill ships embedded in the netsuite-cli binary — `netsuite-cli update` (or
  `netsuite-cli skill install`) refreshes it automatically.
- **M2M is the right flow for agents**: unattended, no browser, safe under
  parallel invocations. Auth-code acts as a named user but its refresh tokens
  are one-time-rotating — never run parallel commands against an auth-code
  account whose token may be expired.
- Account targeting: `--account <alias>` flag → `NETSUITE_ACCOUNT` env var →
  configured default (`account set-default`).
- `account add` never prompts when non-interactive — if no SOAP token is stored yet for the
  alias, it prints a one-line stderr tip pointing at `account soap-auth <alias>` (a re-add with a
  token already stored prints nothing); on a TTY it offers to chain SOAP setup immediately
  (answer `N` to skip). The add JSON gains `"soapTokenStored"` only when the chained setup
  actually ran.

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
- **Sublists:** send them in `--data` as nested `{"items": [...]}`. On update,
  keyed lines merge by key and non-keyed lines append; `--replace item` (on
  `record create` or `record update`, comma-separated for several) swaps in
  exactly the lines you send. Delete a sublist with
  `--data '{"item":{"items":[]}}' --replace item` — fails if the sublist is
  mandatory.
- **Subrecords:** nest them inside their parent sublist line in `--data`
  (e.g. `addressbookaddress` inside an `addressbook` item). Read one directly
  with `record get <type> <id> --sub addressbook/24/addressbookaddress`
  (`--sub addressbook/24` for just the line), or inline everything with
  `record get ... --expand-sub-resources`.
- **Data input:** `--data '<json>'`, `--data @file.json`, or `--data -` (stdin).
- **Forms preview, never write:** `create-form` / `edit-form` / `transform --form`
  return the record as NetSuite would default it, without saving — use before a
  risky create or transform.
- **select-options dependent fields:** pass current values via
  `--data '{"subsidiary":{"id":1}}'`; add a record id positional
  (`record select-options salesOrder 123 --fields item`) for an existing
  record's context.
- **External ids everywhere:** any id positional accepts `eid:<yourId>`.
- **Saved searches vs `suiteql`:** reach for `restlet call` against `cp_saved_search_rl` (see
  **Saved searches via RESTlet** below) instead of `suiteql` when you want the saved search's own
  filters/formulas/columns exactly as the search owner built them, when the data you need is only
  exposed via a saved search (no equivalent SuiteQL table/view), or when you need to create/edit a
  definition. Otherwise prefer `suiteql` — it's easier to iterate on. `saved-search run <id> --type
  <recordtype>` (SOAP) is a legacy fallback only — see the borrowed-time gotcha below.

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

## Saved searches via RESTlet (preferred)

`cp_saved_search_rl` (`customscript_cp_saved_search_rl` / `customdeploy_cp_saved_search_rl`, same
ids in prod and SB2) creates, edits, describes, and runs saved searches over a round-trippable
JSON definition — call it with `restlet call`. It's the go-forward replacement for the SOAP
`saved-search run` path (see the borrowed-time gotcha below); NetSuite has no native REST API for
saved-search definitions.

**Definition JSON** (describe's response shape; also the POST/PUT body shape):
```json
{
  "id": "customsearch_erp_referrals",
  "internalId": 2846,
  "title": "ERP Referrals",
  "type": "transaction",
  "isPublic": true,
  "filterExpression": [["field", "operator", "value"], "AND", [...]],
  "columns": [{"name": "entity", "join": "...", "summary": "SUM", "formula": "...", "sort": "ASC", "label": "..."}]
}
```
`id` is optional on create (omit to let NetSuite generate one) and, along with `type`, immutable
afterward — describe **omits `id` entirely** (not `null`) for a search with no script id (e.g. a
private UI-created one); target those with `internalId` instead. `title` is always required. Only
`name` is required per column. `filterExpression` is always an array of term-arrays and
`"AND"`/`"OR"`/`"NOT"` strings — **a bare single term must be wrapped**: `[["isinactive","is","F"]]`,
not `["isinactive","is","F"]` (the RESTlet returns a pointed `{error}` if you forget).

**Describe** — GET with `id` (script id or numeric internal id), no `run`:
```bash
netsuite-cli restlet call --script customscript_cp_saved_search_rl --deploy customdeploy_cp_saved_search_rl \
  --method GET --param id=2846
# → the definition JSON shape above
```

**Run** — GET with `run=T` or `run=true` (case-insensitive; any other non-empty value errors):
`pageSize` 5–1000 (default 1000), `pageIndex` 0-based (default 0):
```bash
netsuite-cli restlet call --script customscript_cp_saved_search_rl --deploy customdeploy_cp_saved_search_rl \
  --method GET --param id=2846 --param run=T --param pageSize=5 --param pageIndex=0
# → {"items":[{"<columnKey>":{"value":...,"text":...}, ...}, ...],
#     "count":5,"totalRecords":N,"totalPages":N,"pageIndex":0,"hasMore":true}
```
Both GET variants return a JSON **string** body — NetSuite serializes a RESTlet response off the
*request's* Content-Type, and a bodyless GET sends none. `restlet call` parses it transparently;
a raw HTTP caller must `JSON.parse()` it itself.

**Create** — POST the definition (`type` required; omit `id` to auto-generate one; `internalId`
is rejected on create — it has no meaning until the search is saved):
```bash
netsuite-cli restlet call --script customscript_cp_saved_search_rl --deploy customdeploy_cp_saved_search_rl \
  --method POST --data '{
    "title": "CP Example", "type": "customer", "isPublic": true,
    "filterExpression": [["isinactive", "is", "F"]],
    "columns": [{"name": "entityid", "sort": "ASC"}, {"name": "email"}]
  }'
```

**Update** — PUT the full definition (full-definition replace, not a patch); target with `id` or
`internalId`. `title`, `filterExpression` and `columns` are all **required keys** on PUT — omitting
either array wipes it (send back the array describe gave you, even unchanged); `isPublic` is the
only field that's still optional and preserved when omitted:
```bash
netsuite-cli restlet call --script customscript_cp_saved_search_rl --deploy customdeploy_cp_saved_search_rl \
  --method PUT --data '{
    "id": "customsearch_cp_example", "title": "CP Example (renamed)", "type": "customer",
    "filterExpression": [["isinactive", "is", "F"], "AND", ["email", "isnotempty", ""]],
    "columns": [{"name": "entityid", "sort": "DESC"}, {"name": "email", "label": "Mail"}]
  }'
```
Describe output is round-trippable straight into PUT with zero edits — every field describe
returns is one PUT accepts — unless a rare title-lookup failure left describe's `title` null (see
below), in which case supply your own.

**Title resolution:** `N/search.load().title` is null account-wide — a permanent NetSuite
platform gap, not staleness. Describe, POST, and PUT responses all resolve the real title via a
`SELECT name FROM savedsearch WHERE id = ?` SuiteQL lookup layered on top; POST/PUT additionally
fall back to the caller-supplied title if that lookup ever fails, so their `title` is reliable.
Describe has no such fallback — on the rare lookup failure it returns `title: null`, and a caller
PUTing that back must supply its own title like any other required field.

**Limits:** no delete verb (create/describe/run/update only); `id` and `type` are immutable once
created (PUT with a different `type` returns `{"error": "type cannot be changed — the search's
type is <actual>"}`); the only editable fields are `title`, `filterExpression`, `columns`, and
`isPublic` — no scheduling, email alerts, or audience settings.

**Audience:** deployed with `allroles=T` (all internal roles) — SDF rejects scoping this
deployment's `audslctrole` to a custom role (tried both the uppercase enum-style and lowercase
scriptid forms of a real custom role; both errored `must not be <value>`, an apparent SDF platform
limitation on custom-role audience references). Access is gated by OAuth 2.0 client-credential
auth plus the calling role's own RESTlet execute permission, not audience scoping.

## Errors: exit code → action

| Exit | Kind | Action |
|---|---|---|
| 1 | API | Parse stderr JSON: `details[].["o:errorCode"]` (`NONEXISTENT_ID`, `INVALID_CONTENT`, …) says what to fix |
| 2 | usage | Re-run with `--help`; the examples are exact |
| 3 | auth | M2M: credentials wrong/revoked/expired → rotate via `account cert generate` + `account cert upload` (over an auth-code login), then re-run `account add --flow m2m` with the new certificateId. Auth-code: refresh token expired → re-run `account add <alias> --flow auth-code ...`. `saved-search run`: message mentions "SOAP token" → run `account soap-auth <alias>` (interactive; needs the integration record's TBA consumer key/secret — see README "Saved searches (SOAP)") |
| 4 | network | Retries with backoff (429/5xx) already happened — the failure is real. Exception: `saved-search run`'s SOAP client has no retry loop, so a transient network/5xx there is unretried — safe to retry the command yourself |

## Gotchas

- stdout is ALWAYS machine JSON; `--pretty` is for humans. Only `--help` /
  `--version` print human text.
- Windows: use EC P-256 keys — RSA PEMs exceed the Windows credential store
  size limit and `account add` will reject them.
- `raw GET /services/rest/record/v1/metadata-catalog --query select=<type>`
  returns a single object; omit `select` to get the `{"items": [...]}` list.
- HTTP methods parse case-insensitively (`GET` and `get` both work).
- **Browser logins redirect to `http://localhost:8899/callback`** (plain HTTP loopback). If
  NetSuite's authorize page rejects the redirect URI instead of showing the login/consent screen,
  the integration record is missing that exact URI — records set up for an older CLI version have
  only the `https://` spelling and need the `http://` one added.
- **SuiteQL on custom records uses `id`, not `internalid`** — `SELECT
  internalid FROM customrecord_...` errors with `Unknown identifier
  'internalid'` (standard records accept both). Use `id` and it silently
  returns nothing on some shapes, so prefer `id` everywhere for custom records.
- **`saved-search run` is on borrowed time — legacy fallback only:** it calls NetSuite's legacy
  SuiteTalk SOAP web services, which NetSuite is sunsetting — no new TBA/SOAP integrations after
  release 2027.1, and the SOAP endpoints are removed entirely in release 2028.2. Prefer
  `restlet call` against `cp_saved_search_rl` (see **Saved searches via RESTlet** above) — it's
  REST, needs no separate SOAP/TBA auth, and can create/edit definitions, not just run them.
