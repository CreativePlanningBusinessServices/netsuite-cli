# netsuite-cli

A NetSuite REST API command-line tool built for AI agents, not humans typing at a terminal.
Every subcommand takes explicit, typed flags (no interactive prompts beyond one-time auth
setup), emits a single JSON value on stdout on success, and reports failures as a single JSON
object on stderr with a predictable exit code — so an agent can invoke it, parse the result, and
branch on outcome without screen-scraping. It covers record CRUD and special operations
(transform, form previews, select options, attach/detach), SuiteQL, RESTlets, metadata
discovery, system endpoints, async jobs, and raw passthrough requests against any number of
NetSuite accounts, switching between them with one flag.

## Quick start for AI agents

```bash
# Install: grab the release asset for your platform (or: cargo install --git <repo URL>)
gh release download -R CreativePlanningBusinessServices/netsuite-cli \
    --pattern "*aarch64-apple-darwin*" && unzip -o netsuite-cli-*.zip
install -m 0755 netsuite-cli "$HOME/.local/bin/"   # any writable PATH dir

# Bootstrap (credentials: see "NetSuite setup" below — one-time human step)
netsuite-cli account add <alias> --account-id <ID> --flow m2m \
    --client-id <CLIENT_ID> --cert-id <CERT_ID> --key <key.pem>
netsuite-cli account test --account <alias>

# The three commands that cover most work
netsuite-cli describe --list                       # what record types exist?
netsuite-cli suiteql "SELECT id, entityid FROM customer" --limit 10
netsuite-cli record get customer <id> --expand-sub-resources
```

Everything prints JSON on stdout; errors are JSON on stderr with deterministic
exit codes (see [Output contract](#output-contract)). Agents should load
[`skills/netsuite-cli/SKILL.md`](skills/netsuite-cli/SKILL.md) for command
selection, recipes, and error triage.

## Install

Download the archive for your platform from the [Releases page](https://github.com/CreativePlanningBusinessServices/netsuite-cli/releases),
unzip it, and put `netsuite-cli` (or `netsuite-cli.exe` on Windows) on your `PATH`. Asset names use
the release's `v`-prefixed git tag (e.g. tag `v0.1.0`), not the bare crate version. Releases are
published for:

- `netsuite-cli-v0.1.0-aarch64-apple-darwin.zip` (Apple Silicon Mac)
- `netsuite-cli-v0.1.0-x86_64-apple-darwin.zip` (Intel Mac)
- `netsuite-cli-v0.1.0-x86_64-pc-windows-msvc.zip` (Windows)

Once installed, `netsuite-cli update` checks GitHub Releases for a newer version and installs it
in place — see [Updating](#updating).

## NetSuite setup

`netsuite-cli` supports two OAuth 2.0 grants, chosen per account with `account add --flow`:

- **`m2m`** (client credentials, machine-to-machine) — no browser, no user session, no refresh
  token to babysit; the CLI signs a fresh JWT assertion for every token request. Best for
  unattended/agent use against a service account.
- **`auth-code`** (authorization code + PKCE, public client) — impersonates a real logged-in
  user's permissions via a one-time browser login; issues a rotating refresh token that
  `netsuite-cli` persists automatically. Use this when the integration needs to act as a specific
  user rather than a dedicated integration record.

Both flows — and saved-search execution — share a single NetSuite **Integration record**.
Create it once with everything enabled in the same save: **Setup > Integration > Manage
Integrations > New**, then:

1. Give it a name.
2. Under **Authentication**, check **Token-Based Authentication** and
   **TBA: Authorization Flow**, and set the TBA **Callback URL** to
   `https://localhost:8899/callback` (used by saved-search auth; see
   [Saved searches (SOAP)](#saved-searches-soap)). The port must match any
   custom `--port` you pass to `account add`/`account soap-auth` (8899 is the
   default).
3. Under **OAuth 2.0**, enable the grant(s) you need — **Client Credentials
   (Machine to Machine) Grant** for `--flow m2m` and/or **Authorization Code
   Grant** for `--flow auth-code` — plus the **REST Web Services** and
   **RESTlets** scopes.
4. Save, then copy **both** values NetSuite shows you: the **Client ID** (every
   `account add` call needs it via `--client-id`) and the **Client Secret**
   (NetSuite's TBA screens — and this CLI's saved-search prompts — call the same
   pair the **consumer key** and **consumer secret**).
   **The secret is displayed only this once** — it is exactly what saved-search
   (TBA) auth asks for later, so store both in your password manager now.
   Recovering a lost secret means Reset Credentials, which rotates the Client ID
   and breaks every M2M certificate mapping.

`saved-search run` uses a third, separate authentication mechanism — Token-Based Authentication
(TBA) for SuiteTalk SOAP, not one of the two OAuth 2.0 grants above — enabled on the same
integration record by the checklist above; see [Saved searches (SOAP)](#saved-searches-soap).

### M2M (client credentials)

1. On the integration record from the setup checklist above (Client Credentials grant + scopes
   already enabled), note the **Client ID**.
2. Generate a certificate/key pair. NetSuite accepts RSA (3072/4096-bit, signed with RSA-PSS) or
   EC (P-256/384/521) keys, with a maximum validity of 2 years (`-days 730`):

   ```bash
   # EC P-256 (recommended — smaller assertions, faster to sign)
   openssl req -new -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 \
     -pkeyopt ec_param_enc:named_curve \
     -keyout key.pem -out cert.pem -nodes -days 730

   # RSA-PSS 4096, if EC isn't acceptable in your environment
   openssl req -new -x509 -newkey rsa:4096 -keyout key.pem -out cert.pem \
     -nodes -days 730 -sha256 -sigopt rsa_padding_mode:pss -sigopt rsa_pss_saltlen:-1
   ```

   **On Windows, use an EC P-256 key** (already the recommended default everywhere): `account add
   --flow m2m` stores credentials in Windows Credential Manager, which rejects blobs over ~2560
   bytes (UTF-16-encoded), and a serialized RSA-4096 PEM comfortably exceeds that. `netsuite-cli`
   detects this and fails with a usage error rather than a cryptic keychain error; EC P-256 keys
   stay well under the limit on every platform.

   Both commands prompt for a subject (organization/common name, etc.) — any values are fine,
   NetSuite doesn't validate them. `key.pem` never leaves your machine; only `cert.pem` is
   uploaded.
3. Go to **Setup > Integration > Manage Authentication > OAuth 2.0 Client Credentials (M2M)
   Setup**, create a mapping between the integration's application, an entity (employee) and
   role, and upload `cert.pem`. Once saved, NetSuite shows a **Certificate ID** for that
   mapping — this is the JWT `kid` and goes to the CLI as `--cert-id`.
4. Register the account:

   ```bash
   netsuite-cli account add prod \
     --account-id 1234567 \
     --flow m2m \
     --client-id <integration Client ID> \
     --cert-id <Certificate ID from step 3> \
     --key ./key.pem
   ```

   `--account-id` is NetSuite's raw account ID (production accounts have no suffix; sandboxes
   look like `1234567_SB1`, release preview like `1234567_RP`, etc — visible in the account's
   URL or **Setup > Company > Company Information**).

### Auth-code (authorization code + PKCE)

1. On the integration record from the setup checklist above (Authorization Code Grant + REST Web
   Services/RESTlets scopes already enabled), check **Public Client** (no client secret — the CLI
   authenticates with PKCE instead), add the **SuiteAnalytics Workbook** scope (not part of the
   checklist's baseline, but needed for this flow), and set the **Redirect URI** to
   `https://localhost:8899/callback` (or `https://localhost:<port>/callback` if you'll pass a
   custom `--port` to `account add`/`account test --reauth`). Save and note the **Client ID**.
2. Register the account — this opens your default browser for a one-time login:

   ```bash
   netsuite-cli account add dev --account-id 1234567_SB1 --flow auth-code --client-id <Client ID>
   ```

   The CLI runs a short-lived local HTTPS listener on `localhost:8899` to catch the OAuth
   redirect, using a throwaway self-signed certificate (it has to be HTTPS — NetSuite rejects
   plain `http://` redirect URIs). **Your browser will show a certificate-warning page for
   `localhost`** — this is expected; proceed past it (e.g. "Advanced > Proceed to localhost") to
   complete the login. The listener exits as soon as it catches the redirect.
3. If the machine running `netsuite-cli` can't open a browser or receive the loopback redirect
   (headless box, SSH session, container), use `--paste` instead: it prints the login URL for you
   to open elsewhere, then waits for you to paste the full redirect URL back into the terminal:

   ```bash
   netsuite-cli account add dev --account-id 1234567_SB1 --flow auth-code --client-id <Client ID> --paste
   ```

Refresh tokens from this flow rotate on every use (NetSuite issues a new one each refresh,
default 48h validity, 168h max rotation window) and `netsuite-cli` persists the new one
automatically — you should never need to re-run `account add` unless the rotation window lapses,
in which case re-run it or use `account test --reauth`.

**Concurrent use:** because refresh tokens are one-time-use and rotate on every refresh, avoid
running multiple `netsuite-cli` commands concurrently against the same `auth-code` account when
its cached access token has expired — if two invocations race to refresh at once, only one
rotated refresh token wins and the loser's account is left needing re-authentication. `m2m`
accounts have no such constraint (each request is signed independently, with nothing that
rotates or can be raced), so they're the better choice when running commands in parallel, e.g.
from multiple agents.

## Usage

### Accounts and switching

Every command that talks to NetSuite resolves which account to use in this order:

1. `--account <alias>` flag
2. `NETSUITE_ACCOUNT` environment variable
3. the configured default account (`netsuite-cli config get default_account`, changed with
   `netsuite-cli account set-default <alias>`)

The first account you `account add` automatically becomes the default. Credentials are never
stored in the config file — only the alias, NetSuite account ID, and flow type live in
`config.toml`; OAuth secrets live in the OS keychain (Keychain on macOS, Credential Manager on
Windows).

```bash
netsuite-cli --account sandbox record list customer --q 'email CONTAIN "@acme.com"'
NETSUITE_ACCOUNT=sandbox netsuite-cli record list customer
netsuite-cli record list customer   # uses the configured default account
```

### `account` — manage stored accounts

```bash
$ netsuite-cli account list --pretty
{
  "accounts": [
    { "alias": "prod", "accountId": "1234567", "flow": "m2m", "default": true },
    { "alias": "sandbox", "accountId": "1234567_SB1", "flow": "auth-code", "default": false }
  ]
}

$ netsuite-cli account test --alias sandbox
{"alias":"sandbox","ok":true}

$ netsuite-cli account set-default sandbox
{"default":"sandbox"}
```

### `record` — CRUD against `record/v1`

```bash
$ netsuite-cli record get customer 1234 --fields companyName,email
{"id":"1234","companyName":"Acme Corp","email":"ap@acme.example","links":[{"rel":"self","href":"https://1234567-sb1.suitetalk.api.netsuite.com/services/rest/record/v1/customer/1234"}]}

$ netsuite-cli record list customer --q 'email CONTAIN "@acme.com"' --pretty
{
  "links": [],
  "count": 1,
  "items": [{ "id": "1234", "links": [{"rel": "self", "href": "..."}] }],
  "hasMore": false,
  "offset": 0,
  "totalResults": 1
}

$ netsuite-cli record create customer --data '{"companyName":"Acme"}'
{"id":"9001","location":"https://1234567-sb1.suitetalk.api.netsuite.com/services/rest/record/v1/customer/9001"}

$ netsuite-cli record update customer 1234 --data '{"email":"new@acme.example"}'
{"updated":true,"id":"1234"}

$ netsuite-cli record upsert customer ACME-001 --data '{"companyName":"Acme"}'
{"upserted":true,"externalId":"ACME-001"}

$ netsuite-cli record delete customer 1234
{"deleted":true,"id":"1234"}
```

Special operations in the same namespace:

```bash
$ netsuite-cli record transform salesOrder 123 invoice
{"id":"9101","location":"https://1234567-sb1.suitetalk.api.netsuite.com/services/rest/record/v1/invoice/9101"}

$ netsuite-cli record transform salesOrder 123 itemFulfillment --form   # preview; creates nothing
$ netsuite-cli record create-form salesOrder --data '{"entity":{"id":107}}'
$ netsuite-cli record edit-form salesOrder 123 --data '{"memo":"rush"}'

$ netsuite-cli record select-options customer --fields entitystatus --q 'entitystatus START_WITH LEAD-'
{"entitystatus":{"_selectOptions":{"items":[{"id":13,"refName":"CUSTOMER-Closed Won"}],"count":1,"offset":0,"hasMore":false,"totalResults":1}}}

$ netsuite-cli record attach customer 660 contact 106 --role -5
{"attached":true,"type":"customer","id":"660","attachedType":"contact","attachedId":"106"}

$ netsuite-cli record detach opportunity 379 file 398
{"detached":true,"type":"opportunity","id":"379","detachedType":"file","detachedId":"398"}
```

`transform` executes the conversion (sales order → invoice, etc.) and returns the
new record like `record create`; add `--form` to preview the target record's
defaulted fields without creating anything. `create-form`/`edit-form` are the
same preview for plain creates and updates. `select-options` returns the valid
dropdown values for a field — pass `--data` with the field values the options
depend on, or an existing record id positional for that record's context. Every
id positional also accepts NetSuite's `eid:<externalId>` form.

`--data` accepts inline JSON, `@path/to/file.json`, or `-` to read from stdin. `record list` and
`suiteql` support `--all` to transparently follow `hasMore` pagination and merge every page's
`items` into one response.

### `suiteql` — run a SuiteQL query

```bash
$ netsuite-cli suiteql "SELECT id, entityid FROM customer WHERE email LIKE '%@acme.com'" --pretty
{
  "links": [],
  "count": 2,
  "items": [
    { "id": "1234", "entityid": "Acme Corp" },
    { "id": "1235", "entityid": "Acme Subsidiary" }
  ],
  "hasMore": false,
  "offset": 0,
  "totalResults": 2
}
```

Column values always come back as strings (NetSuite's own SuiteQL behavior) — cast in the query
(`TO_CHAR`, `TO_NUMBER`, etc.) if you need a specific representation.

### `describe` — discover record types and schemas

```bash
$ netsuite-cli describe --list
{"recordTypes":["account","currency","customer","salesOrder","vendor","..."]}

$ netsuite-cli describe customer --pretty
{
  "type": "object",
  "properties": {
    "companyName": { "type": "string" },
    "email": { "type": "string" }
  }
}
```

`--format openapi` returns the OpenAPI 3.0 shape instead of JSON Schema. Results are cached on
disk per account (`cache_ttl_hours` config key, default 24h); pass `--refresh` to bypass the
cache for one call.

### `restlet` — call a deployed RESTlet

```bash
$ netsuite-cli restlet call --script 482 --deploy 1 --method GET --param customerId=1234
{"customerId":"1234","balance":420.5}
```

### `system` — system/v1 endpoints

```bash
$ netsuite-cli system server-time
{"serverTime":"2026-07-14T16:21:00.000Z"}

$ netsuite-cli system governance-limits
{"type":"accountLimit","accountConcurrencyLimit":25,"accountUnallocatedConcurrencyLimit":10}
```

### `raw` — arbitrary REST request

```bash
$ netsuite-cli raw GET /services/rest/record/v1/customer/1234
{"id":"1234","companyName":"Acme Corp","email":"ap@acme.example"}
```

Escape hatch for anything not covered by a dedicated subcommand — repeatable `--query key=value`
and `--header 'Name: value'`, plus `--data` for the body.

### `job` — asynchronous requests (`Prefer: respond-async`)

```bash
$ netsuite-cli job submit POST /services/rest/record/v1/customer --data '{"companyName":"Acme"}'
{"jobId":"9001","location":"/services/rest/async/v1/job/9001","status":202}

$ netsuite-cli job status 9001
{"completed":true,"id":"9001","progress":100,"task":{"links":[{"rel":"self","href":"..."}]}}

$ netsuite-cli job result 9001
{"id":"9002","location":"https://1234567-sb1.suitetalk.api.netsuite.com/services/rest/record/v1/customer/9002"}
```

`job result` without `--task` only works when the job has exactly one task; otherwise it lists
the task IDs so you can pass `--task` explicitly. Pass `--idempotency-key <uuid>` to `job submit`
to make retries safe.

### `update` — self-update

```bash
$ netsuite-cli update --check
{"current":"0.1.0","latest":"0.2.0","updateAvailable":true}
```

### `config` — persisted CLI settings

```bash
$ netsuite-cli config get
{"default_account":"prod","cache_ttl_hours":24}

$ netsuite-cli config set cache_ttl_hours 48
{"cache_ttl_hours":48}
```

## Saved searches (SOAP)

> **NetSuite is sunsetting SOAP web services.** Release 2028.2 disables the SOAP endpoints
> entirely, and no NetSuite account can create a *new* TBA/SOAP integration once it's on release
> 2027.1 or later. If `saved-search run` is or might become part of your workflow, enable
> Token-Based Authentication on your integration record **now** — not when you actually need it.

`netsuite-cli saved-search run` executes an existing saved search over NetSuite's legacy SuiteTalk
SOAP web services (there is no REST equivalent for running a saved search) and returns its rows as
JSON. It needs its own one-time setup, separate from the M2M/auth-code flows above, because SOAP
authenticates with Token-Based Authentication (TBA), not OAuth 2.0.

### Integration-record setup

Already done if you followed the [NetSuite setup](#netsuite-setup) checklist — the record has TBA
enabled and you captured the consumer key/secret at creation (the same pair as the Client
ID/Client Secret; TBA just names them differently). On an interactive terminal,
`account add` also offers to chain straight into SOAP setup right after adding an account, so you
mint the token while that secret is still fresh; answer the prompt (or just run `account soap-auth
<alias>` later) whenever you're ready.

### The Reset Credentials trap (recovery path)

Relevant if your integration record predates the checklist above and either doesn't have TBA
enabled yet, or its consumer secret was never captured. The secret is shown **exactly once**, at
the moment TBA is enabled and the record is saved. If it wasn't captured then:

- **Do not click Reset Credentials** to get a fresh one. Reset Credentials rotates the
  integration's client ID, which breaks every M2M certificate mapping and every configured
  `client_id` that points at this record — not just the TBA/SOAP piece.
- The safe fix is a **second integration record**, used only for TBA/SOAP, so resetting or
  recreating its credentials never touches the M2M integration.

### Per-account auth

```bash
netsuite-cli account soap-auth <alias> [--port 8899] [--paste]
```

Or skip the explicit step: just run `saved-search run`. On first use, if no SOAP token is stored
for the account and the CLI is attached to an interactive terminal, it prompts for the integration
record's **consumer key** (visible) and **consumer secret** (hidden) — the Client ID/Client Secret
you captured at record creation, under their TBA names. It stores both in the OS keychain, then
opens the browser for the TBA consent flow: the same three-step authorization `soap-auth` runs
explicitly. The resulting token never expires, so this normally happens once per
account, ever. Set `NETSUITE_CLI_TBA_CONSUMER_KEY` / `NETSUITE_CLI_TBA_CONSUMER_SECRET` to supply
the consumer pair without a prompt (CI, scripted first run). Running non-interactively with no
stored token and no env vars fails with an `auth` error (exit `3`) naming
`account soap-auth <alias>`.

**Sandbox refresh caveat:** refreshing a sandbox from production invalidates every token stored for
it, SOAP TBA tokens included — re-run `account soap-auth <alias>` afterward.

### Usage

```bash
$ netsuite-cli saved-search run 57 --type transaction
{"items":[...],"count":1000,"totalRecords":4820,"totalPages":5,"pageIndex":1,"hasMore":true}

$ netsuite-cli saved-search run customsearch_example --type customrecord --all
{"items":[...],"count":4820,"totalRecords":4820,"totalPages":5,"pageIndex":5,"hasMore":false}
```

`<id>` is the saved search's numeric internal id or its `customsearch_*` script id. `--type` is
required — the record type the search is defined against (`transaction`, `customer`,
`customrecord`, etc.; see `src/soap/search_types.rs` for the full list of 95 supported types, or
just run it — an unknown type's error lists every valid one). `--limit` sets the SOAP page size,
`5`–`1000` (default `1000`, the max). `--all` transparently pages through every result via
`searchMoreWithId` and merges them into one response, same `--all` semantics as `record list` and
`suiteql`. The output shape is `{"items", "count", "totalRecords", "totalPages", "pageIndex",
"hasMore"}` — `count` is the number of rows in `items` (equal to `totalRecords` once `--all` has
fetched everything).

## Output contract

stdout carries **only** JSON — one compact value per invocation by default, or indented with the
global `--pretty` flag. Nothing else (no log lines, no progress text) is written to stdout;
interactive prompts during `account add`/`account test --reauth` go to stderr so stdout stays
parseable even mid-login. `--help`/`--version` are the one documented exception: they print
human-readable text to stdout and exit `0`; every other invocation error (bad flag, missing
argument, etc.) is JSON on stderr like any other failure below.

On failure, stderr gets exactly one JSON object and the process exits non-zero:

| `kind`    | Meaning                                    | Extra fields              | Exit code |
| --------- | ------------------------------------------- | -------------------------- | :-------: |
| `api`     | NetSuite REST API returned an error response | `status`, `details` (array of `{detail, "o:errorCode"}`) | 1 |
| `usage`   | Bad CLI invocation (missing flag, invalid JSON, unresolved account, etc.) | — | 2 |
| `auth`    | Credential/token problem (missing, expired refresh, keychain failure) | — | 3 |
| `network` | Transport failure (DNS, TLS, timeout, connection refused) | — | 4 |

Every error object also has `message`. Success is always exit code `0`.

```bash
$ netsuite-cli record get customer 999999; echo "exit: $?"
{"kind":"api","status":404,"message":"Record not found: ...","details":[{"detail":"...","o:errorCode":"RCRD_DSNT_EXIST"}]}
exit: 1

$ netsuite-cli describe; echo "exit: $?"
{"kind":"usage","message":"describe requires either a record type or --list, e.g. `netsuite-cli describe --list` or `netsuite-cli describe customer`"}
exit: 2
```

## Updating

```bash
netsuite-cli update --check   # report whether a newer release exists; installs nothing
netsuite-cli update           # download and install the latest release in place
```

## Development

```bash
cargo build
cargo test
cargo run -- --help
```

`cargo test` runs the full suite against `wiremock` — no real network calls, no real keychain.
Before committing, run the same gate CI runs:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

### Live smoke tests

`tests/live_smoke.rs` exercises a real NetSuite sandbox end to end (real OAuth, real keychain
entry, real HTTP) using an account alias you've already registered with `account add`. It's
excluded from the default `cargo test` run (`#[ignore]`d) and only compiled in CI, never
executed there:

```bash
NETSUITE_LIVE_ALIAS=<alias> cargo test --test live_smoke -- --ignored
```

## Reference documentation

- [NetSuite REST Web Services help](https://docs.oracle.com/en/cloud/saas/netsuite/ns-online-help/book_1559132836.html)
- [REST API Browser (record/v1, 2026.1)](https://system.netsuite.com/help/helpcenter/en_US/APIs/REST_API_Browser/record/v1/2026.1/index.html)
- Each endpoint's metadata is retrievable via the REST metadata catalog described in the help
  documentation (`netsuite-cli describe`).
