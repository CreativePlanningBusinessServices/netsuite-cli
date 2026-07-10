use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand};

use crate::commands::describe::MetadataFormat;
use crate::commands::{self, account, describe, record, suiteql};
use crate::config::{AuthFlow, Config};
use crate::context::context_for;
use crate::error::CliError;
use crate::output;
use crate::secrets::{AccountSecrets, KeyringStore, SecretStore};

/// Task 12 will make this configurable; hardcoded for now.
const METADATA_CACHE_TTL: Duration = Duration::from_secs(24 * 3600);

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

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum AccountFlowArg {
    M2m,
    AuthCode,
}

#[derive(Subcommand)]
pub enum AccountAction {
    /// Add or overwrite an account alias; the first account added becomes the default
    #[command(
        after_help = "Examples:\n  netsuite-cli account add prod --account-id 1234567 --flow m2m --client-id CID --cert-id KID --key ./netsuite-m2m.pem\n  netsuite-cli account add dev --account-id 1234567_SB1 --flow auth-code --client-id CID --port 8899"
    )]
    Add {
        alias: String,
        #[arg(long = "account-id")]
        account_id: String,
        #[arg(long, value_enum)]
        flow: AccountFlowArg,
        /// Required for both flows
        #[arg(long = "client-id")]
        client_id: Option<String>,
        /// Required for --flow m2m
        #[arg(long = "cert-id")]
        cert_id: Option<String>,
        /// Required for --flow m2m
        #[arg(long)]
        key: Option<PathBuf>,
        /// Loopback listener port for --flow auth-code
        #[arg(long, default_value_t = 8899)]
        port: u16,
        /// For --flow auth-code: paste the redirect URL instead of running the loopback listener
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
        Command::Account { action } => dispatch_account(cli, action).await,
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
            describe::describe_type(
                &context.client,
                record_type,
                *format,
                &cache_dir,
                *refresh,
                METADATA_CACHE_TTL,
            )
            .await
        }
    }
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
                    require_flag(client_id.as_deref(), "--flow m2m requires --client-id")?;
                let cert_id = require_flag(cert_id.as_deref(), "--flow m2m requires --cert-id")?;
                let key = key.as_deref().ok_or_else(|| {
                    CliError::Usage("account add --flow m2m requires --key".into())
                })?;
                account::add_m2m(
                    &config_path,
                    store.as_ref(),
                    alias,
                    account_id,
                    client_id,
                    cert_id,
                    key,
                )
            }
            AccountFlowArg::AuthCode => {
                let client_id = require_flag(
                    client_id.as_deref(),
                    "--flow auth-code requires --client-id",
                )?;
                account::add_auth_code(
                    &config_path,
                    store.clone(),
                    alias,
                    account_id,
                    client_id,
                    *port,
                    *paste,
                )
                .await
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
    }
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
}
