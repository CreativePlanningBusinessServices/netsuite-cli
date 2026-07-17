use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand};

use crate::builtin;
use crate::commands::describe::MetadataFormat;
use crate::commands::{
    self, account, cert, config_cmd, describe, job, raw, record, restlet, suiteql, system, update,
};
use crate::config::{AuthFlow, Config};
use crate::context::context_for;
use crate::error::CliError;
use crate::output;
use crate::secrets::{AccountSecrets, KeyringStore, SecretStore, TbaSecrets};

#[derive(Parser)]
#[command(
    name = "netsuite-cli",
    version,
    about = "NetSuite REST API CLI for AI agents",
    propagate_version = true,
    color = clap::ColorChoice::Never
)]
pub struct Cli {
    /// Account alias (falls back to $NETSUITE_ACCOUNT, then the configured default)
    #[arg(long, global = true)]
    pub account: Option<String>,
    /// Pretty-print JSON output
    #[arg(long, global = true)]
    pub pretty: bool,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Manage stored account credentials and aliases
    Account {
        #[command(subcommand)]
        action: AccountAction,
    },
    /// Record CRUD against record/v1 (record types are plain strings, e.g. customer)
    Record {
        #[command(subcommand)]
        action: RecordAction,
    },
    /// Run a SuiteQL query (POST query/v1/suiteql with Prefer: transient)
    #[command(
        after_help = "Example: netsuite-cli suiteql \"SELECT id, entityid FROM customer\" --all"
    )]
    Suiteql {
        query: String,
        #[arg(long)]
        limit: Option<u64>,
        #[arg(long)]
        offset: Option<u64>,
        #[arg(long)]
        all: bool,
    },
    /// Discover record types and per-type schemas from this account's metadata catalog
    #[command(
        after_help = "Examples:\n  netsuite-cli describe --list\n  netsuite-cli describe customer\n  netsuite-cli describe salesOrder --format openapi --refresh"
    )]
    Describe {
        record_type: Option<String>,
        #[arg(long)]
        list: bool,
        #[arg(long, value_enum, default_value = "schema")]
        format: MetadataFormat,
        #[arg(long)]
        refresh: bool,
    },
    /// Call a deployed RESTlet (script + deploy id)
    Restlet {
        #[command(subcommand)]
        action: RestletAction,
    },
    /// Send an arbitrary request to any NetSuite REST endpoint
    #[command(
        after_help = "Examples:\n  netsuite-cli raw GET /services/rest/record/v1/customer/1\n  netsuite-cli raw POST /services/rest/record/v1/customer --data '{\"companyName\":\"Acme\"}'"
    )]
    Raw {
        #[arg(value_enum, ignore_case = true)]
        method: HttpMethodArg,
        path: String,
        /// Repeatable key=value query parameter
        #[arg(long = "query")]
        query: Vec<String>,
        /// Repeatable 'Name: value' header
        #[arg(long = "header")]
        header: Vec<String>,
        #[arg(long)]
        data: Option<String>,
    },
    /// Submit and track asynchronous (Prefer: respond-async) requests
    Job {
        #[command(subcommand)]
        action: JobAction,
    },
    /// System-level endpoints (system/v1): server time, governance limits
    System {
        #[command(subcommand)]
        action: SystemAction,
    },
    /// Check for or install the latest release from GitHub
    #[command(after_help = "Examples:\n  netsuite-cli update --check\n  netsuite-cli update")]
    Update {
        /// Only report whether a newer release is available; do not install it
        #[arg(long)]
        check: bool,
        /// Do not refresh the bundled agent skill after updating the binary
        #[arg(long)]
        no_skill: bool,
    },
    /// Install or refresh the bundled agent skill (SKILL.md) into your Claude skills dir
    #[command(after_help = "Examples:\n  netsuite-cli skill install\n  \
netsuite-cli skill install --dir ~/.config/claude/skills/netsuite-cli")]
    Skill {
        #[command(subcommand)]
        action: SkillAction,
    },
    /// Get or set persisted CLI configuration
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Execute a saved search over SuiteTalk SOAP (requires a TBA token; see account soap-auth)
    #[command(
        after_help = "Examples:\n  netsuite-cli saved-search run 57 --type transaction\n  netsuite-cli saved-search run customsearch_example --type customrecord --all\n\nNOTE: NetSuite removes SOAP web services in release 2028.2; this command stops working then."
    )]
    SavedSearch {
        #[command(subcommand)]
        action: SavedSearchAction,
    },
}

#[derive(Subcommand)]
pub enum SavedSearchAction {
    /// Run a saved search and return its rows as JSON
    Run {
        /// Saved search id (numeric internal id or script id, e.g. customsearch_example)
        id: String,
        /// Record type the saved search is defined against (e.g. transaction, customer)
        #[arg(long = "type")]
        record_type: String,
        /// Page size (SOAP searchPreferences bounds apply); defaults to the max page size
        #[arg(long)]
        limit: Option<u64>,
        /// Fetch every page instead of just the first
        #[arg(long)]
        all: bool,
    },
}

#[derive(Subcommand)]
pub enum ConfigAction {
    /// Print a config value, or the whole effective config when no key is given
    #[command(
        after_help = "Examples:\n  netsuite-cli config get\n  netsuite-cli config get cache_ttl_hours"
    )]
    Get { key: Option<String> },
    /// Persist a config value
    #[command(
        after_help = "Examples:\n  netsuite-cli config set default_account prod\n  netsuite-cli config set cache_ttl_hours 48"
    )]
    Set { key: String, value: String },
}

#[derive(Subcommand)]
pub enum SystemAction {
    /// Current NetSuite server time in UTC (no permissions required)
    #[command(after_help = "Example: netsuite-cli system server-time")]
    ServerTime,
    /// Concurrency limit allocation for this account and integration
    #[command(after_help = "Example: netsuite-cli system governance-limits")]
    GovernanceLimits,
}

#[derive(Subcommand)]
pub enum SkillAction {
    /// Write the embedded SKILL.md to your Claude skills dir (skips a symlinked/repo-tracked copy)
    Install {
        /// Target dir (default: $CLAUDE_CONFIG_DIR or ~/.claude, under skills/netsuite-cli)
        #[arg(long)]
        dir: Option<PathBuf>,
    },
}

/// Methods accepted by `raw` and `job submit` — NetSuite's REST endpoints (record/v1, async/v1,
/// etc.) accept PATCH for partial updates, including as the method behind an async job submit.
#[derive(Clone, Copy, clap::ValueEnum)]
pub enum HttpMethodArg {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

impl HttpMethodArg {
    fn to_method(self) -> reqwest::Method {
        match self {
            HttpMethodArg::Get => reqwest::Method::GET,
            HttpMethodArg::Post => reqwest::Method::POST,
            HttpMethodArg::Put => reqwest::Method::PUT,
            HttpMethodArg::Patch => reqwest::Method::PATCH,
            HttpMethodArg::Delete => reqwest::Method::DELETE,
        }
    }
}

/// Methods accepted by `restlet call` — NetSuite RESTlets only ever dispatch to
/// onRequest/GET/PUT/POST/DELETE entry points; there is no onPatch, so PATCH is intentionally
/// not offered here (unlike `HttpMethodArg`, which backs `raw`/`job submit`).
#[derive(Clone, Copy, clap::ValueEnum)]
pub enum RestletMethodArg {
    Get,
    Post,
    Put,
    Delete,
}

impl RestletMethodArg {
    fn to_method(self) -> reqwest::Method {
        match self {
            RestletMethodArg::Get => reqwest::Method::GET,
            RestletMethodArg::Post => reqwest::Method::POST,
            RestletMethodArg::Put => reqwest::Method::PUT,
            RestletMethodArg::Delete => reqwest::Method::DELETE,
        }
    }
}

#[derive(Subcommand)]
pub enum RestletAction {
    #[command(
        after_help = "Examples:\n  netsuite-cli restlet call --script 482 --deploy 1 --method GET --param customerId=42\n  netsuite-cli restlet call --script 482 --deploy 1 --method POST --data '{\"foo\":\"bar\"}'"
    )]
    Call {
        #[arg(long)]
        script: String,
        #[arg(long)]
        deploy: String,
        /// Required; no default, to avoid surprising an agent about side effects
        #[arg(long, value_enum, ignore_case = true)]
        method: RestletMethodArg,
        /// Repeatable key=value param sent as a query parameter; RESTlets read these
        /// via request.parameters for GET/DELETE (use --data for POST/PUT bodies)
        #[arg(long = "param")]
        param: Vec<String>,
        #[arg(long)]
        data: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum JobAction {
    #[command(
        after_help = "Examples:\n  netsuite-cli job submit POST /services/rest/record/v1/customer --data '{\"companyName\":\"Acme\"}' --idempotency-key 8f14e-uuid\n  # Batch record-collection write (<=100 records): --header sets the collection content-type, --query carries ?ids= for GET/DELETE\n  netsuite-cli job submit POST /services/rest/record/v1/customer --header 'Content-Type: application/vnd.oracle.resource+json; type=collection' --data '{\"items\":[{\"companyName\":\"A\"},{\"companyName\":\"B\"}]}'"
    )]
    Submit {
        #[arg(value_enum, ignore_case = true)]
        method: HttpMethodArg,
        path: String,
        /// Repeatable key=value query parameter (e.g. ids=1,2,3 for a batch GET/DELETE)
        #[arg(long = "query")]
        query: Vec<String>,
        /// Repeatable 'Name: value' header. `Prefer: respond-async` is always sent;
        /// use this for the batch collection content-type or other custom headers.
        #[arg(long = "header")]
        header: Vec<String>,
        #[arg(long)]
        data: Option<String>,
        #[arg(long = "idempotency-key")]
        idempotency_key: Option<String>,
    },
    #[command(after_help = "Example: netsuite-cli job status 9001")]
    Status { job_id: String },
    #[command(after_help = "Example: netsuite-cli job tasks 9001")]
    Tasks { job_id: String },
    #[command(
        after_help = "Example: netsuite-cli job result 9001 --task 9001.1\nWithout --task, the job's tasks are listed and used only if there is exactly one."
    )]
    Result {
        job_id: String,
        #[arg(long)]
        task: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum RecordAction {
    #[command(
        after_help = "Examples:\n  netsuite-cli record get customer 1234 --fields companyName,email\n  netsuite-cli record get customer 1234 --sub addressbook/24/addressbookaddress"
    )]
    Get {
        record_type: String,
        id: String,
        #[arg(long)]
        fields: Option<String>,
        #[arg(long)]
        expand_sub_resources: bool,
        /// Sublist line or subrecord path appended to the record URL
        /// (e.g. addressbook/24 or addressbook/24/addressbookaddress)
        #[arg(long)]
        sub: Option<String>,
    },
    #[command(
        after_help = "Example: netsuite-cli record list customer --q 'email CONTAIN \"@acme.com\"' --all"
    )]
    List {
        record_type: String,
        #[arg(long)]
        q: Option<String>,
        #[arg(long)]
        limit: Option<u64>,
        #[arg(long)]
        offset: Option<u64>,
        #[arg(long)]
        all: bool,
    },
    #[command(
        after_help = "Examples:\n  netsuite-cli record create customer --data '{\"companyName\":\"Acme\"}'\n  netsuite-cli record create salesOrder --data @order.json --replace item"
    )]
    Create {
        record_type: String,
        #[arg(long)]
        data: String,
        /// Comma-separated sublists to replace instead of merging with form defaults
        #[arg(long)]
        replace: Option<String>,
    },
    #[command(
        after_help = "Example: netsuite-cli record update customer 1234 --data @patch.json --replace addressBook"
    )]
    Update {
        record_type: String,
        id: String,
        #[arg(long)]
        data: String,
        #[arg(long)]
        replace: Option<String>,
    },
    #[command(
        after_help = "Example: netsuite-cli record upsert customer ACME-001 --data '{\"companyName\":\"Acme\"}'"
    )]
    Upsert {
        record_type: String,
        external_id: String,
        #[arg(long)]
        data: String,
    },
    #[command(after_help = "Example: netsuite-cli record delete customer 1234")]
    Delete { record_type: String, id: String },
    /// Attach a record (e.g. a contact or file) to another record
    #[command(
        after_help = "Examples:\n  netsuite-cli record attach customer 660 contact 106 --role -5\n  netsuite-cli record attach opportunity 379 file 398\n\nThe first pair is the record being attached TO; the second pair is what gets attached."
    )]
    Attach {
        record_type: String,
        id: String,
        attach_type: String,
        attach_id: String,
        /// Contact role id for the attachment (accepts negative ids like -5)
        #[arg(long, allow_hyphen_values = true)]
        role: Option<String>,
    },
    /// Detach a previously attached record
    #[command(after_help = "Example: netsuite-cli record detach opportunity 379 file 398")]
    Detach {
        record_type: String,
        id: String,
        detach_type: String,
        detach_id: String,
    },
    /// Turn one record into another (e.g. salesOrder -> invoice); creates the target record
    #[command(
        after_help = "Examples:\n  netsuite-cli record transform salesOrder 123 invoice\n  netsuite-cli record transform salesOrder 123 itemFulfillment --form --fields item --expand-sub-resources"
    )]
    Transform {
        source_type: String,
        source_id: String,
        target_type: String,
        /// Field overrides applied to the transformed record
        #[arg(long)]
        data: Option<String>,
        /// Return the create-form preview instead of executing (creates nothing)
        #[arg(long)]
        form: bool,
        /// Limit the --form preview to these fields
        #[arg(long, requires = "form")]
        fields: Option<String>,
        /// Expand sublists/subrecords in the --form preview
        #[arg(long, requires = "form")]
        expand_sub_resources: bool,
    },
    /// Preview a new record's defaulted fields without creating it
    #[command(
        after_help = "Example: netsuite-cli record create-form salesOrder --data '{\"entity\":{\"id\":107}}'"
    )]
    CreateForm {
        record_type: String,
        /// Field values the form defaults should take into account
        #[arg(long)]
        data: Option<String>,
        /// Limit the preview to these fields
        #[arg(long)]
        fields: Option<String>,
        /// Expand sublists/subrecords in the preview
        #[arg(long)]
        expand_sub_resources: bool,
    },
    /// Preview an update's effect on an existing record without saving it
    #[command(
        after_help = "Example: netsuite-cli record edit-form salesOrder 123 --data '{\"memo\":\"rush\"}'"
    )]
    EditForm {
        record_type: String,
        id: String,
        /// Field values the previewed update would apply
        #[arg(long)]
        data: Option<String>,
        /// Limit the preview to these fields
        #[arg(long)]
        fields: Option<String>,
        /// Expand sublists/subrecords in the preview
        #[arg(long)]
        expand_sub_resources: bool,
    },
    /// Valid dropdown (select) values for record fields
    #[command(
        after_help = "Examples:\n  netsuite-cli record select-options customer --fields entitystatus,location\n  netsuite-cli record select-options customer --fields entitystatus --q 'entitystatus START_WITH LEAD-'\n  netsuite-cli record select-options salesOrder 123 --fields item --data '{\"subsidiary\":{\"id\":1}}'"
    )]
    SelectOptions {
        record_type: String,
        /// Existing record id: evaluates options in that record's context (uses PATCH)
        id: Option<String>,
        /// Comma-separated field names to fetch options for (sublist fields as line.field)
        #[arg(long)]
        fields: String,
        /// Filter, e.g. 'entitystatus START_WITH LEAD-' (operators: CONTAIN, IS, START_WITH)
        #[arg(long)]
        q: Option<String>,
        #[arg(long)]
        limit: Option<u64>,
        #[arg(long)]
        offset: Option<u64>,
        /// Values for fields the requested options depend on
        #[arg(long)]
        data: Option<String>,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum AccountFlowArg {
    M2m,
    AuthCode,
}

#[derive(Subcommand)]
pub enum AccountAction {
    /// Add or overwrite an account alias; the first account added becomes the default
    #[command(
        after_help = "Examples:\n  netsuite-cli account add prod --account-id 1234567 --flow m2m --client-id CID --cert-id KID --key ./netsuite-m2m.pem\n  netsuite-cli account add dev --account-id 1234567_SB1 --flow auth-code --client-id CID --port 8899\n  netsuite-cli account add dev --account-id 1234567_SB1 --flow auth-code   # built-in client ID\n\nBootstrap M2M without NetSuite UI setup: add an auth-code account (built-in client ID), then `account cert generate` + `account cert upload`, then re-add with --flow m2m --cert-id <returned id>."
    )]
    Add {
        alias: String,
        #[arg(long = "account-id")]
        account_id: String,
        #[arg(long, value_enum)]
        flow: AccountFlowArg,
        /// Integration record Client ID; omit to use the client ID built into this binary
        /// (builds compiled with NETSUITE_CLI_BUILTIN_CLIENT_ID)
        #[arg(long = "client-id")]
        client_id: Option<String>,
        /// Required for --flow m2m
        #[arg(long = "cert-id")]
        cert_id: Option<String>,
        /// Required for --flow m2m
        #[arg(long)]
        key: Option<PathBuf>,
        /// Loopback listener port for --flow auth-code and the chained SOAP setup (must match
        /// the integration record's TBA callback URL)
        #[arg(long, default_value_t = 8899)]
        port: u16,
        /// Paste the redirect URL instead of running the loopback listener — applies to
        /// --flow auth-code and the chained SOAP setup
        #[arg(long)]
        paste: bool,
    },
    /// List configured account aliases (never prints secrets)
    List,
    /// Change which alias is used when --account/$NETSUITE_ACCOUNT is not given
    SetDefault { alias: String },
    /// Remove an account alias and its stored secrets
    Remove { alias: String },
    /// Verify stored credentials by calling the metadata catalog
    Test {
        #[arg(long)]
        alias: Option<String>,
        /// Re-run the interactive login before testing (auth-code accounts only)
        #[arg(long)]
        reauth: bool,
        #[arg(long, default_value_t = 8899)]
        port: u16,
        #[arg(long)]
        paste: bool,
    },
    /// Mint a never-expiring SOAP (TBA) token for an account via browser consent
    #[command(
        after_help = "Example: netsuite-cli account soap-auth demo\n\nRequires TBA + \
        'TBA: Authorization Flow' enabled on the integration record, with callback URL \
        https://localhost:8899/callback"
    )]
    SoapAuth {
        alias: String,
        /// Loopback listener port (must match the integration record's TBA callback URL)
        #[arg(long, default_value_t = 8899)]
        port: u16,
        /// Paste the redirect URL instead of running the loopback listener
        #[arg(long)]
        paste: bool,
    },
    /// Generate M2M certificates and manage them via NetSuite's certificate rotation API
    Cert {
        #[command(subcommand)]
        action: CertAction,
    },
}

#[derive(Subcommand)]
pub enum CertAction {
    /// Generate an EC P-256 private key + self-signed certificate for M2M auth
    #[command(
        after_help = "Example: netsuite-cli account cert generate\n\nWrites netsuite-m2m-key.pem \
        (keep private; never uploaded) and netsuite-m2m-cert.pem (upload this)."
    )]
    Generate {
        /// Output path for the private key PEM (never leaves this machine)
        #[arg(long = "key-out", default_value = "netsuite-m2m-key.pem")]
        key_out: PathBuf,
        /// Output path for the certificate PEM (what `cert upload` sends to NetSuite)
        #[arg(long = "cert-out", default_value = "netsuite-m2m-cert.pem")]
        cert_out: PathBuf,
        /// Validity in days; NetSuite's ceiling is 730 (two years)
        #[arg(long, default_value_t = 730)]
        days: u64,
        /// Certificate subject common name (NetSuite does not validate it)
        #[arg(long = "common-name", default_value = "netsuite-cli")]
        common_name: String,
        /// Overwrite existing output files
        #[arg(long)]
        force: bool,
    },
    /// List the certificates registered for an integration
    #[command(after_help = "Example: netsuite-cli account cert list --account bootstrap")]
    List {
        /// Integration Client ID (defaults to the selected account's, then the built-in one)
        #[arg(long = "client-id")]
        client_id: Option<String>,
    },
    /// Upload a certificate, creating the M2M entity/role mapping; returns the certificate id
    #[command(
        after_help = "Example: netsuite-cli account cert upload --cert netsuite-m2m-cert.pem \
        --account bootstrap\n\nThe returned certificateId is the --cert-id for \
        `account add --flow m2m`. Requires the 'Manage own OAuth 2.0 Client Credentials \
        certificates' permission on the logged-in role."
    )]
    Upload {
        /// Certificate PEM to upload (the certificate, never the private key)
        #[arg(long)]
        cert: PathBuf,
        /// Entity (user) internal id to map; defaults to the id captured at auth-code login
        #[arg(long, allow_hyphen_values = true)]
        entity: Option<String>,
        /// Role internal id to map; defaults to the id captured at auth-code login
        #[arg(long, allow_hyphen_values = true)]
        role: Option<String>,
        /// Integration Client ID (defaults to the selected account's, then the built-in one)
        #[arg(long = "client-id")]
        client_id: Option<String>,
    },
    /// Revoke a certificate by its certificate id
    #[command(
        after_help = "Example: netsuite-cli account cert revoke NPMnRyPg-WDWhPiAbisjKiH0fqnBpjOZ367wDTe0pqA"
    )]
    Revoke {
        certificate_id: String,
        /// Integration Client ID (defaults to the selected account's, then the built-in one)
        #[arg(long = "client-id")]
        client_id: Option<String>,
    },
}

pub async fn cli_main() -> i32 {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(clap_error) => return handle_clap_error(&clap_error),
    };
    match dispatch(&cli).await {
        Ok(result) => {
            output::print_json(&result, cli.pretty);
            0
        }
        Err(error) => {
            output::print_error(&error);
            error.exit_code() as i32
        }
    }
}

/// clap's own help/version/error rendering is human-readable text, not our stdout-JSON /
/// stderr-JSON contract. `--help`/`--version` are a documented exception (human text on stdout,
/// exit 0, matching what every other CLI does); every other parse failure (bad flag, missing
/// required arg, invalid enum value, etc.) is folded into the same `CliError::Usage` envelope
/// every other invocation error goes through, so an agent never has to special-case clap's output.
fn handle_clap_error(clap_error: &clap::Error) -> i32 {
    use clap::error::ErrorKind;
    match clap_error.kind() {
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => {
            print!("{clap_error}");
            0
        }
        _ => {
            let usage_error = CliError::Usage(clap_error.render().to_string().trim().to_string());
            output::print_error(&usage_error);
            usage_error.exit_code() as i32
        }
    }
}

async fn dispatch(cli: &Cli) -> Result<serde_json::Value, CliError> {
    match &cli.command {
        Command::Account { action } => dispatch_account(cli, action).await,
        Command::Record { action } => {
            let context = context_for(cli.account.as_deref())?;
            match action {
                RecordAction::Get {
                    record_type,
                    id,
                    fields,
                    expand_sub_resources,
                    sub,
                } => {
                    record::get(
                        &context.client,
                        record_type,
                        id,
                        fields.clone(),
                        *expand_sub_resources,
                        sub.clone(),
                    )
                    .await
                }
                RecordAction::List {
                    record_type,
                    q,
                    limit,
                    offset,
                    all,
                } => {
                    record::list(
                        &context.client,
                        record_type,
                        q.clone(),
                        *limit,
                        *offset,
                        *all,
                    )
                    .await
                }
                RecordAction::Create {
                    record_type,
                    data,
                    replace,
                } => {
                    record::create(
                        &context.client,
                        record_type,
                        commands::read_data_arg(data)?,
                        replace.clone(),
                    )
                    .await
                }
                RecordAction::Update {
                    record_type,
                    id,
                    data,
                    replace,
                } => {
                    record::update(
                        &context.client,
                        record_type,
                        id,
                        commands::read_data_arg(data)?,
                        replace.clone(),
                    )
                    .await
                }
                RecordAction::Upsert {
                    record_type,
                    external_id,
                    data,
                } => {
                    record::upsert(
                        &context.client,
                        record_type,
                        external_id,
                        commands::read_data_arg(data)?,
                    )
                    .await
                }
                RecordAction::Delete { record_type, id } => {
                    record::delete(&context.client, record_type, id).await
                }
                RecordAction::Attach {
                    record_type,
                    id,
                    attach_type,
                    attach_id,
                    role,
                } => {
                    record::attach(
                        &context.client,
                        record_type,
                        id,
                        attach_type,
                        attach_id,
                        role.clone(),
                    )
                    .await
                }
                RecordAction::Detach {
                    record_type,
                    id,
                    detach_type,
                    detach_id,
                } => record::detach(&context.client, record_type, id, detach_type, detach_id).await,
                RecordAction::Transform {
                    source_type,
                    source_id,
                    target_type,
                    data,
                    form,
                    fields,
                    expand_sub_resources,
                } => {
                    let body = data.as_deref().map(commands::read_data_arg).transpose()?;
                    record::transform(
                        &context.client,
                        source_type,
                        source_id,
                        target_type,
                        body,
                        *form,
                        fields.clone(),
                        *expand_sub_resources,
                    )
                    .await
                }
                RecordAction::CreateForm {
                    record_type,
                    data,
                    fields,
                    expand_sub_resources,
                } => {
                    let body = data.as_deref().map(commands::read_data_arg).transpose()?;
                    record::create_form(
                        &context.client,
                        record_type,
                        body,
                        fields.clone(),
                        *expand_sub_resources,
                    )
                    .await
                }
                RecordAction::EditForm {
                    record_type,
                    id,
                    data,
                    fields,
                    expand_sub_resources,
                } => {
                    let body = data.as_deref().map(commands::read_data_arg).transpose()?;
                    record::edit_form(
                        &context.client,
                        record_type,
                        id,
                        body,
                        fields.clone(),
                        *expand_sub_resources,
                    )
                    .await
                }
                RecordAction::SelectOptions {
                    record_type,
                    id,
                    fields,
                    q,
                    limit,
                    offset,
                    data,
                } => {
                    let body = data.as_deref().map(commands::read_data_arg).transpose()?;
                    record::select_options(
                        &context.client,
                        record_type,
                        id.as_deref(),
                        fields,
                        q.clone(),
                        *limit,
                        *offset,
                        body,
                    )
                    .await
                }
            }
        }
        Command::Suiteql {
            query,
            limit,
            offset,
            all,
        } => {
            let context = context_for(cli.account.as_deref())?;
            suiteql::run(&context.client, query, *limit, *offset, *all).await
        }
        Command::Describe {
            record_type,
            list,
            format,
            refresh,
        } => {
            if !*list && record_type.is_none() {
                return Err(CliError::Usage(
                    "describe requires either a record type or --list, e.g. `netsuite-cli describe --list` or `netsuite-cli describe customer`".into(),
                ));
            }
            let context = context_for(cli.account.as_deref())?;
            if *list {
                return describe::list_types(&context.client).await;
            }
            let record_type = record_type.as_ref().expect("checked above");
            let cache_dir = crate::config::default_cache_dir()
                .join("metadata")
                .join(&context.alias);
            let config = Config::load(&crate::config::default_config_path())?;
            let cache_ttl = Duration::from_secs(config.cache_ttl_hours.unwrap_or(24) * 3600);
            describe::describe_type(
                &context.client,
                record_type,
                *format,
                &cache_dir,
                *refresh,
                cache_ttl,
            )
            .await
        }
        Command::Restlet { action } => {
            let context = context_for(cli.account.as_deref())?;
            match action {
                RestletAction::Call {
                    script,
                    deploy,
                    method,
                    param,
                    data,
                } => {
                    let params = commands::parse_key_value_pairs(param, "--param")?;
                    let body = data.as_deref().map(commands::read_data_arg).transpose()?;
                    restlet::call(
                        &context.client,
                        &context.restlet_base,
                        script,
                        deploy,
                        method.to_method(),
                        &params,
                        body,
                    )
                    .await
                }
            }
        }
        Command::Raw {
            method,
            path,
            query,
            header,
            data,
        } => {
            let context = context_for(cli.account.as_deref())?;
            let query_pairs = commands::parse_key_value_pairs(query, "--query")?;
            let header_pairs = commands::parse_header_pairs(header)?;
            let body = data.as_deref().map(commands::read_data_arg).transpose()?;
            raw::run(
                &context.client,
                method.to_method(),
                path,
                &query_pairs,
                &header_pairs,
                body,
            )
            .await
        }
        Command::Job { action } => {
            let context = context_for(cli.account.as_deref())?;
            match action {
                JobAction::Submit {
                    method,
                    path,
                    query,
                    header,
                    data,
                    idempotency_key,
                } => {
                    let query_pairs = commands::parse_key_value_pairs(query, "--query")?;
                    let header_pairs = commands::parse_header_pairs(header)?;
                    let body = data.as_deref().map(commands::read_data_arg).transpose()?;
                    job::submit(
                        &context.client,
                        method.to_method(),
                        path,
                        &query_pairs,
                        &header_pairs,
                        body,
                        idempotency_key.clone(),
                    )
                    .await
                }
                JobAction::Status { job_id } => job::status(&context.client, job_id).await,
                JobAction::Tasks { job_id } => job::tasks(&context.client, job_id).await,
                JobAction::Result { job_id, task } => {
                    job::result(&context.client, job_id, task.clone()).await
                }
            }
        }
        Command::System { action } => {
            let context = context_for(cli.account.as_deref())?;
            match action {
                SystemAction::ServerTime => system::server_time(&context.client).await,
                SystemAction::GovernanceLimits => system::governance_limits(&context.client).await,
            }
        }
        Command::Update { check, no_skill } => update::run(*check, *no_skill).await,
        Command::Skill { action } => match action {
            SkillAction::Install { dir } => commands::skill::install(dir.as_deref()),
        },
        Command::Config { action } => {
            let config_path = crate::config::default_config_path();
            match action {
                ConfigAction::Get { key } => config_cmd::get(&config_path, key.as_deref()),
                ConfigAction::Set { key, value } => config_cmd::set(&config_path, key, value),
            }
        }
        Command::SavedSearch { action } => match action {
            SavedSearchAction::Run {
                id,
                record_type,
                limit,
                all,
            } => {
                let config_path = crate::config::default_config_path();
                let config = Config::load(&config_path)?;
                let env_alias = std::env::var("NETSUITE_ACCOUNT").ok();
                let alias = config.resolve_alias(cli.account.as_deref(), env_alias.as_deref())?;
                let account_id = config.accounts[&alias].account_id.clone();
                let store: Arc<dyn SecretStore> = Arc::new(KeyringStore);
                let secrets = ensure_tba_secrets(&alias, store, &config_path, 8899).await?;
                let soap = crate::soap::SoapClient::new(
                    reqwest::Client::new(),
                    &crate::account::rest_base(&account_id),
                    &account_id,
                    secrets,
                )?;
                commands::saved_search::run(&soap, id, record_type, *limit, *all).await
            }
        },
    }
}

/// First-run auto-auth for `saved-search run`: reuse a previously minted SOAP token if one is
/// stored, otherwise (only when attached to an interactive terminal) walk the user through
/// `account soap-auth` once and store the resulting token for next time.
async fn ensure_tba_secrets(
    alias: &str,
    store: Arc<dyn SecretStore>,
    config_path: &Path,
    port: u16,
) -> Result<TbaSecrets, CliError> {
    if let Some(secrets) = store.get_tba(alias)?
        && secrets.token_id.is_some()
    {
        return Ok(secrets);
    }
    if !std::io::stdin().is_terminal() {
        return Err(CliError::Auth(format!(
            "no SOAP token for '{alias}'; run `netsuite-cli account soap-auth {alias}` \
             in an interactive terminal first (set NETSUITE_CLI_TBA_CONSUMER_KEY and \
             NETSUITE_CLI_TBA_CONSUMER_SECRET to supply the consumer pair without prompting)"
        )));
    }
    eprintln!("No SOAP token for '{alias}' yet — starting one-time browser authorization…");
    account::soap_auth(config_path, store.clone(), alias, port, false).await?;
    store.get_tba(alias)?.ok_or_else(|| {
        CliError::Auth("SOAP authorization completed but no token was stored".into())
    })
}

/// After a successful `account add`, offer to mint the SOAP (TBA) token right away so new
/// users learn saved searches need it while the integration record's one-time consumer
/// secret is still at hand. Interactive terminals get a yes/no prompt; everything else gets
/// a one-line tip. A chained failure never fails the add — the account is already stored.
async fn offer_soap_setup(
    alias: &str,
    config_path: &Path,
    store: Arc<dyn SecretStore>,
    port: u16,
    paste_mode: bool,
    add_result: serde_json::Value,
) -> serde_json::Value {
    if let Ok(Some(secrets)) = store.get_tba(alias)
        && secrets.token_id.is_some()
    {
        return add_result;
    }
    if !std::io::stdin().is_terminal() {
        eprintln!("{}", soap_auth_tip(alias));
        return add_result;
    }
    eprint!("Saved searches use SOAP/TBA auth. Set it up for '{alias}' now? [y/N] ");
    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_err() || !wants_soap_setup(&answer) {
        eprintln!("{}", soap_auth_tip(alias));
        return add_result;
    }
    match account::soap_auth(config_path, store, alias, port, paste_mode).await {
        Ok(_) => with_soap_token_flag(add_result, true),
        Err(soap_error) => {
            crate::output::print_error(&soap_error);
            eprintln!(
                "account '{alias}' was added; only SOAP setup failed — re-run \
                 `netsuite-cli account soap-auth {alias}`"
            );
            with_soap_token_flag(add_result, false)
        }
    }
}

fn wants_soap_setup(answer: &str) -> bool {
    matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

fn soap_auth_tip(alias: &str) -> String {
    format!(
        "tip: saved searches authenticate separately (SOAP/TBA) — run \
         `netsuite-cli account soap-auth {alias}` when ready (needs the consumer \
         key/secret captured at integration-record creation)"
    )
}

fn with_soap_token_flag(mut add_result: serde_json::Value, stored: bool) -> serde_json::Value {
    if let Some(fields) = add_result.as_object_mut() {
        fields.insert("soapTokenStored".into(), serde_json::Value::Bool(stored));
    }
    add_result
}

async fn dispatch_account(
    cli: &Cli,
    action: &AccountAction,
) -> Result<serde_json::Value, CliError> {
    let config_path = crate::config::default_config_path();
    let store: Arc<dyn SecretStore> = Arc::new(KeyringStore);
    match action {
        AccountAction::Add {
            alias,
            account_id,
            flow,
            client_id,
            cert_id,
            key,
            port,
            paste,
        } => match flow {
            AccountFlowArg::M2m => {
                let client_id =
                    builtin::resolve_client_id(client_id.as_deref(), builtin::builtin_client_id())?;
                let cert_id = require_flag(cert_id.as_deref(), "--flow m2m requires --cert-id")?;
                let key = key.as_deref().ok_or_else(|| {
                    CliError::Usage("account add --flow m2m requires --key".into())
                })?;
                let add_result = account::add_m2m(
                    &config_path,
                    store.as_ref(),
                    alias,
                    account_id,
                    &client_id,
                    cert_id,
                    key,
                )?;
                Ok(offer_soap_setup(
                    alias,
                    &config_path,
                    store.clone(),
                    *port,
                    *paste,
                    add_result,
                )
                .await)
            }
            AccountFlowArg::AuthCode => {
                let client_id =
                    builtin::resolve_client_id(client_id.as_deref(), builtin::builtin_client_id())?;
                let add_result = account::add_auth_code(
                    &config_path,
                    store.clone(),
                    alias,
                    account_id,
                    &client_id,
                    *port,
                    *paste,
                )
                .await?;
                Ok(offer_soap_setup(
                    alias,
                    &config_path,
                    store.clone(),
                    *port,
                    *paste,
                    add_result,
                )
                .await)
            }
        },
        AccountAction::List => account::list(&config_path),
        AccountAction::SetDefault { alias } => account::set_default(&config_path, alias),
        AccountAction::Remove { alias } => account::remove(&config_path, store.as_ref(), alias),
        AccountAction::Test {
            alias,
            reauth,
            port,
            paste,
        } => {
            let config = Config::load(&config_path)?;
            let env_alias = std::env::var("NETSUITE_ACCOUNT").ok();
            let resolved_alias = config.resolve_alias(
                alias.as_deref().or(cli.account.as_deref()),
                env_alias.as_deref(),
            )?;
            if *reauth {
                reauthenticate(
                    &config,
                    &config_path,
                    store.clone(),
                    &resolved_alias,
                    *port,
                    *paste,
                )
                .await?;
            }
            let context = context_for(Some(&resolved_alias))?;
            account::test(&context.client, &resolved_alias).await
        }
        AccountAction::SoapAuth { alias, port, paste } => {
            account::soap_auth(&config_path, store.clone(), alias, *port, *paste).await
        }
        AccountAction::Cert { action } => dispatch_cert(cli, action, store).await,
    }
}

async fn dispatch_cert(
    cli: &Cli,
    action: &CertAction,
    store: Arc<dyn SecretStore>,
) -> Result<serde_json::Value, CliError> {
    match action {
        CertAction::Generate {
            key_out,
            cert_out,
            days,
            common_name,
            force,
        } => cert::generate(key_out, cert_out, *days, common_name, *force),
        CertAction::List { client_id } => {
            let context = context_for(cli.account.as_deref())?;
            let client_id = cert_client_id(store.as_ref(), &context.alias, client_id.as_deref())?;
            cert::list(&context.client, &context.restlet_base, &client_id).await
        }
        CertAction::Upload {
            cert: cert_path,
            entity,
            role,
            client_id,
        } => {
            let context = context_for(cli.account.as_deref())?;
            let entity = resolve_mapping_id(
                "--entity",
                entity.as_deref(),
                context.entity_id.as_deref(),
                &context.alias,
            )?;
            let role = resolve_mapping_id(
                "--role",
                role.as_deref(),
                context.role_id.as_deref(),
                &context.alias,
            )?;
            let client_id = cert_client_id(store.as_ref(), &context.alias, client_id.as_deref())?;
            cert::upload(
                &context.client,
                &context.restlet_base,
                &client_id,
                cert_path,
                &entity,
                &role,
            )
            .await
        }
        CertAction::Revoke {
            certificate_id,
            client_id,
        } => {
            let context = context_for(cli.account.as_deref())?;
            let client_id = cert_client_id(store.as_ref(), &context.alias, client_id.as_deref())?;
            cert::revoke(
                &context.client,
                &context.restlet_base,
                &client_id,
                certificate_id,
            )
            .await
        }
    }
}

/// Client ID for the certificate rotation URL: explicit flag → the id stored with the
/// selected account's credentials → the build's built-in id.
fn cert_client_id(
    store: &dyn SecretStore,
    alias: &str,
    flag: Option<&str>,
) -> Result<String, CliError> {
    if let Some(explicit) = flag {
        return Ok(explicit.to_string());
    }
    if let Some(
        AccountSecrets::M2m { client_id, .. } | AccountSecrets::AuthCode { client_id, .. },
    ) = store.get(alias)?
    {
        return Ok(client_id);
    }
    builtin::resolve_client_id(None, builtin::builtin_client_id())
}

/// The upload mapping needs an entity and role id. Auth-code logins capture both from
/// NetSuite's callback (`account add --flow auth-code` records them), so flags are only needed
/// for M2M accounts, accounts added before capture existed, or to map a different user/role
/// than the one that logged in.
fn resolve_mapping_id(
    flag_name: &str,
    flag: Option<&str>,
    recorded: Option<&str>,
    alias: &str,
) -> Result<String, CliError> {
    flag.or(recorded).map(str::to_string).ok_or_else(|| {
        CliError::Usage(format!(
            "account '{alias}' has no recorded {} id — pass {flag_name} <internal id> \
             (auth-code logins capture the entity/role automatically; M2M accounts always \
             need the flag)",
            flag_name.trim_start_matches("--"),
        ))
    })
}

fn require_flag<'flag>(value: Option<&'flag str>, message: &str) -> Result<&'flag str, CliError> {
    value.ok_or_else(|| CliError::Usage(format!("account add {message}")))
}

async fn reauthenticate(
    config: &Config,
    config_path: &std::path::Path,
    store: Arc<dyn SecretStore>,
    alias: &str,
    port: u16,
    paste: bool,
) -> Result<(), CliError> {
    let entry = &config.accounts[alias];
    let AuthFlow::AuthCode = entry.flow else {
        return Err(CliError::Usage(
            "--reauth is only supported for auth-code accounts".into(),
        ));
    };
    let secrets = store
        .get(alias)?
        .ok_or_else(|| CliError::Auth(format!("no credentials stored for '{alias}'")))?;
    let AccountSecrets::AuthCode { client_id, .. } = secrets else {
        return Err(CliError::Auth(format!(
            "stored credentials for '{alias}' do not match its configured flow"
        )));
    };
    account::add_auth_code(
        config_path,
        store,
        alias,
        &entry.account_id,
        &client_id,
        port,
        paste,
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AccountEntry;
    use crate::secrets::MemoryStore;
    use clap::error::ErrorKind;

    /// `Cli` intentionally has no `Debug` impl (it carries no state worth dumping), so
    /// `Result::unwrap_err` (which requires `T: Debug`) doesn't work here; extract the error by
    /// hand instead.
    fn expect_parse_error(args: &[&str]) -> clap::Error {
        match Cli::try_parse_from(args) {
            Ok(_) => panic!("expected {args:?} to fail parsing"),
            Err(clap_error) => clap_error,
        }
    }

    #[test]
    fn bad_flag_yields_a_usage_kind_error_and_exit_code_two() {
        let clap_error = expect_parse_error(&["netsuite-cli", "--bogus-flag"]);
        assert_ne!(clap_error.kind(), ErrorKind::DisplayHelp);
        assert_ne!(clap_error.kind(), ErrorKind::DisplayVersion);
        assert_eq!(handle_clap_error(&clap_error), 2);
    }

    #[test]
    fn help_flag_yields_display_help_kind_and_exit_code_zero() {
        let clap_error = expect_parse_error(&["netsuite-cli", "--help"]);
        assert_eq!(clap_error.kind(), ErrorKind::DisplayHelp);
        assert_eq!(handle_clap_error(&clap_error), 0);
    }

    #[test]
    fn version_flag_yields_display_version_kind_and_exit_code_zero() {
        let clap_error = expect_parse_error(&["netsuite-cli", "--version"]);
        assert_eq!(clap_error.kind(), ErrorKind::DisplayVersion);
        assert_eq!(handle_clap_error(&clap_error), 0);
    }

    #[test]
    fn http_method_args_are_case_insensitive() {
        Cli::try_parse_from(["netsuite-cli", "raw", "GET", "/x"])
            .expect("uppercase method should parse");
        Cli::try_parse_from(["netsuite-cli", "raw", "get", "/x"])
            .expect("lowercase method should parse");
        Cli::try_parse_from([
            "netsuite-cli",
            "restlet",
            "call",
            "--script",
            "1",
            "--deploy",
            "1",
            "--method",
            "POST",
        ])
        .expect("uppercase restlet method should parse");
    }

    #[test]
    fn bogus_http_method_still_errors() {
        expect_parse_error(&["netsuite-cli", "raw", "BOGUS", "/x"]);
    }

    #[test]
    fn require_flag_returns_value_when_present() {
        assert_eq!(
            require_flag(Some("cid"), "--flow m2m requires --client-id").unwrap(),
            "cid"
        );
    }

    #[test]
    fn require_flag_errors_naming_missing_flag() {
        let error = require_flag(None, "--flow m2m requires --client-id").unwrap_err();
        match error {
            CliError::Usage(message) => assert!(message.contains("--client-id")),
            other => panic!("expected Usage error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_account_missing_key_arm_matches_require_flag_message_shape() {
        // The `--key` check in dispatch_account's M2m arm is hand-rolled rather than going
        // through require_flag; it should still surface a Usage error naming `--key` and
        // matching the same "account add <detail>" message shape require_flag produces.
        let cli = Cli {
            account: None,
            pretty: false,
            command: Command::Account {
                action: AccountAction::List,
            },
        };
        let action = AccountAction::Add {
            alias: "prod".into(),
            account_id: "1234567".into(),
            flow: AccountFlowArg::M2m,
            client_id: Some("CID".into()),
            cert_id: Some("KID".into()),
            key: None,
            port: 8899,
            paste: false,
        };

        // Neither client_id nor cert_id is missing, so this reaches the --key check
        // without ever touching the config file or keyring.
        let error = dispatch_account(&cli, &action).await.unwrap_err();
        match error {
            CliError::Usage(message) => {
                assert!(message.starts_with("account add "));
                assert!(message.contains("--key"));
            }
            other => panic!("expected Usage error, got {other:?}"),
        }
    }

    fn config_with_m2m_alias(alias: &str) -> Config {
        let mut config = Config::default();
        config.accounts.insert(
            alias.to_string(),
            AccountEntry {
                account_id: "1234567".into(),
                flow: AuthFlow::M2m,
                entity_id: None,
                role_id: None,
            },
        );
        config.default_account = Some(alias.to_string());
        config
    }

    #[tokio::test]
    async fn reauthenticate_rejects_m2m_flow_account_with_usage_error() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("config.toml");
        let config = config_with_m2m_alias("prod");
        config.save(&config_path).unwrap();

        let store: Arc<dyn SecretStore> = Arc::new(MemoryStore::default());
        let error = reauthenticate(&config, &config_path, store, "prod", 8899, false)
            .await
            .unwrap_err();

        match error {
            CliError::Usage(message) => {
                let lowercase_message = message.to_lowercase();
                assert!(
                    lowercase_message.contains("reauth") || lowercase_message.contains("auth-code"),
                    "expected message to mention reauth/auth-code, got: {message}"
                );
            }
            other => panic!("expected Usage error, got {other:?}"),
        }
    }

    #[test]
    fn system_subcommands_parse() {
        Cli::try_parse_from(["netsuite-cli", "system", "server-time"]).expect("server-time parses");
        Cli::try_parse_from(["netsuite-cli", "system", "governance-limits"])
            .expect("governance-limits parses");
    }

    #[test]
    fn record_attach_parses_positional_pairs_and_negative_role() {
        Cli::try_parse_from([
            "netsuite-cli",
            "record",
            "attach",
            "customer",
            "660",
            "contact",
            "106",
            "--role",
            "-5",
        ])
        .expect("attach with negative role id should parse");
    }

    #[test]
    fn record_transform_form_flags_require_form() {
        expect_parse_error(&[
            "netsuite-cli",
            "record",
            "transform",
            "salesOrder",
            "1",
            "invoice",
            "--fields",
            "item",
        ]);
        Cli::try_parse_from([
            "netsuite-cli",
            "record",
            "transform",
            "salesOrder",
            "1",
            "invoice",
            "--form",
            "--fields",
            "item",
        ])
        .expect("--fields with --form should parse");
    }

    #[test]
    fn record_form_subcommands_parse_with_kebab_case_names() {
        Cli::try_parse_from(["netsuite-cli", "record", "create-form", "salesOrder"])
            .expect("create-form parses");
        Cli::try_parse_from(["netsuite-cli", "record", "edit-form", "salesOrder", "12"])
            .expect("edit-form parses");
    }

    #[test]
    fn record_select_options_requires_fields_flag() {
        expect_parse_error(&["netsuite-cli", "record", "select-options", "customer"]);
        Cli::try_parse_from([
            "netsuite-cli",
            "record",
            "select-options",
            "customer",
            "--fields",
            "entitystatus",
        ])
        .expect("select-options with --fields parses");
    }

    #[test]
    fn saved_search_run_parses_type_limit_and_all() {
        let cli = Cli::try_parse_from([
            "netsuite-cli",
            "saved-search",
            "run",
            "customsearch_example",
            "--type",
            "transaction",
            "--limit",
            "100",
            "--all",
        ])
        .unwrap();
        let Command::SavedSearch {
            action:
                SavedSearchAction::Run {
                    id,
                    record_type,
                    limit,
                    all,
                },
        } = cli.command
        else {
            panic!("wrong variant")
        };
        assert_eq!(id, "customsearch_example");
        assert_eq!(record_type, "transaction");
        assert_eq!(limit, Some(100));
        assert!(all);
    }

    #[test]
    fn saved_search_run_requires_type() {
        assert!(Cli::try_parse_from(["netsuite-cli", "saved-search", "run", "57"]).is_err());
    }

    #[test]
    fn soap_setup_answer_parser_accepts_only_yes_variants() {
        assert!(wants_soap_setup("y\n"));
        assert!(wants_soap_setup("YES\n"));
        assert!(wants_soap_setup(" Yes "));
        assert!(!wants_soap_setup("\n"));
        assert!(!wants_soap_setup(""));
        assert!(!wants_soap_setup("n\n"));
        assert!(!wants_soap_setup("yep\n"));
    }

    #[test]
    fn soap_auth_tip_names_the_command_and_alias() {
        let tip = soap_auth_tip("demo");
        assert!(tip.contains("netsuite-cli account soap-auth demo"));
        assert!(tip.contains("consumer key/secret"));
    }

    #[test]
    fn soap_token_flag_merges_into_add_result_object() {
        let merged =
            with_soap_token_flag(serde_json::json!({"alias": "demo", "flow": "m2m"}), true);
        assert_eq!(merged["soapTokenStored"], true);
        assert_eq!(merged["alias"], "demo");
        let failed = with_soap_token_flag(serde_json::json!({"alias": "demo"}), false);
        assert_eq!(failed["soapTokenStored"], false);
    }

    #[test]
    fn account_cert_subcommands_parse() {
        Cli::try_parse_from(["netsuite-cli", "account", "cert", "generate"])
            .expect("generate with defaults parses");
        Cli::try_parse_from([
            "netsuite-cli",
            "account",
            "cert",
            "generate",
            "--key-out",
            "/tmp/k.pem",
            "--cert-out",
            "/tmp/c.pem",
            "--days",
            "365",
            "--force",
        ])
        .expect("generate with overrides parses");
        Cli::try_parse_from(["netsuite-cli", "account", "cert", "list"]).expect("list parses");
        Cli::try_parse_from(["netsuite-cli", "account", "cert", "revoke", "CERTID123"])
            .expect("revoke parses");
    }

    #[test]
    fn account_cert_upload_accepts_negative_entity_ids() {
        let cli = Cli::try_parse_from([
            "netsuite-cli",
            "account",
            "cert",
            "upload",
            "--cert",
            "cert.pem",
            "--entity",
            "-5",
            "--role",
            "1000",
        ])
        .expect("upload with negative entity id parses");
        let Command::Account {
            action:
                AccountAction::Cert {
                    action: CertAction::Upload { entity, role, .. },
                },
        } = cli.command
        else {
            panic!("wrong variant")
        };
        assert_eq!(entity.as_deref(), Some("-5"));
        assert_eq!(role.as_deref(), Some("1000"));
    }

    #[test]
    fn cert_client_id_prefers_flag_then_stored_credentials() {
        let store = MemoryStore::default();
        store
            .set(
                "dev",
                &AccountSecrets::AuthCode {
                    client_id: "STOREDCID".into(),
                    refresh_token: None,
                },
            )
            .unwrap();
        assert_eq!(
            cert_client_id(&store, "dev", Some("FLAGCID")).unwrap(),
            "FLAGCID"
        );
        assert_eq!(cert_client_id(&store, "dev", None).unwrap(), "STOREDCID");
    }

    #[test]
    fn resolve_mapping_id_prefers_flag_and_errors_when_nothing_is_recorded() {
        assert_eq!(
            resolve_mapping_id("--entity", Some("7"), Some("9"), "dev").unwrap(),
            "7"
        );
        assert_eq!(
            resolve_mapping_id("--entity", None, Some("9"), "dev").unwrap(),
            "9"
        );
        let error = resolve_mapping_id("--role", None, None, "dev").unwrap_err();
        match error {
            CliError::Usage(message) => {
                assert!(message.contains("--role"));
                assert!(message.contains("auth-code"));
            }
            other => panic!("expected Usage error, got {other:?}"),
        }
    }

    #[test]
    fn skill_install_parses_dir_override() {
        let cli =
            Cli::try_parse_from(["netsuite-cli", "skill", "install", "--dir", "/tmp/x"]).unwrap();
        let Command::Skill {
            action: SkillAction::Install { dir },
        } = cli.command
        else {
            panic!("wrong variant")
        };
        assert_eq!(dir, Some(PathBuf::from("/tmp/x")));
    }

    #[test]
    fn update_parses_no_skill_flag() {
        let cli = Cli::try_parse_from(["netsuite-cli", "update", "--no-skill"]).unwrap();
        let Command::Update { check, no_skill } = cli.command else {
            panic!("wrong variant")
        };
        assert!(!check);
        assert!(no_skill);
    }
}
