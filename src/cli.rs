use clap::{Parser, Subcommand};

use crate::commands::{self, record, suiteql};
use crate::context::context_for;
use crate::error::CliError;
use crate::output;

#[derive(Parser)]
#[command(
    name = "netsuite-cli",
    version,
    about = "NetSuite REST API CLI for AI agents",
    propagate_version = true
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
}

#[derive(Subcommand)]
pub enum RecordAction {
    #[command(
        after_help = "Example: netsuite-cli record get customer 1234 --fields companyName,email"
    )]
    Get {
        record_type: String,
        id: String,
        #[arg(long)]
        fields: Option<String>,
        #[arg(long)]
        expand_sub_resources: bool,
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
        after_help = "Example: netsuite-cli record create customer --data '{\"companyName\":\"Acme\"}'"
    )]
    Create {
        record_type: String,
        #[arg(long)]
        data: String,
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
}

pub async fn cli_main() -> i32 {
    let cli = Cli::parse();
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

async fn dispatch(cli: &Cli) -> Result<serde_json::Value, CliError> {
    match &cli.command {
        Command::Record { action } => {
            let context = context_for(cli.account.as_deref())?;
            match action {
                RecordAction::Get {
                    record_type,
                    id,
                    fields,
                    expand_sub_resources,
                } => {
                    record::get(
                        &context.client,
                        record_type,
                        id,
                        fields.clone(),
                        *expand_sub_resources,
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
                RecordAction::Create { record_type, data } => {
                    record::create(&context.client, record_type, commands::read_data_arg(data)?)
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
    }
}
