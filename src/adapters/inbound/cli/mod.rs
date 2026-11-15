//! Resource/action CLI parsing and presentation, with hidden legacy account shortcuts.
mod usage;
use crate::{
    accounts::{AccountStatus, now},
    application::account_commands,
    bootstrap,
};
use anyhow::{Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    version,
    about = "Local coding-agent proxy with ChatGPT/Codex account failover"
)]
struct Cli {
    /// Private state/config directory; defaults to $XDG_STATE_HOME/llm-proxy.
    #[arg(long, global = true, env = "LLM_PROXY_STATE_DIR", value_name = "PATH")]
    state_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Legacy shortcut for accounts status.
    #[command(hide = true)]
    Status(StatusArgs),
    /// Legacy shortcut for accounts add.
    #[command(hide = true)]
    Login(AddArgs),
    /// Legacy shortcut for accounts reset-quota.
    #[command(hide = true)]
    ResetQuota(ResetQuotaArgs),
    /// Sign in, inspect or remove subscription accounts.
    Accounts {
        #[command(subcommand)]
        command: AccountsCommand,
    },
    /// Serve the client API; defaults to loopback only.
    Serve {
        /// Provider model used for unmapped client aliases; overrides config.toml.
        #[arg(long, value_name = "MODEL_ID")]
        default_model: Option<String>,
        /// Listening IP; use 0.0.0.0 inside a container, not on a public host.
        #[arg(long, value_name = "IP_ADDRESS")]
        listen_address: Option<std::net::IpAddr>,
        /// Local API listening port; overrides config.toml.
        #[arg(long, value_name = "PORT")]
        listen_port: Option<u16>,
    },
}
#[derive(Subcommand)]
enum AccountsCommand {
    /// Add or renew an account for an explicit provider (currently codex only).
    Add(AddArgs),
    /// Show cached account usage offline, or explicitly refresh from the provider.
    Status(StatusArgs),
    /// Preview or redeem a BANKED reset; does not buy prepaid credits.
    ResetQuota(ResetQuotaArgs),
    /// Legacy Codex login; use accounts add codex instead.
    #[command(hide = true)]
    Login {
        #[arg(long, value_name = "ACCOUNT_NAME")]
        name: String,
        #[arg(long, value_enum, default_value = "browser", value_name = "METHOD")]
        login_method: LoginMethod,
    },
    /// Legacy account listing; use accounts status for usage information.
    #[command(hide = true)]
    List {
        #[arg(long, value_enum, default_value = "table", value_name = "FORMAT")]
        output_format: OutputFormat,
    },
    /// Remove an account by full UUID or unique prefix (at least 8 characters).
    Remove {
        #[arg(value_name = "ACCOUNT_ID_OR_PREFIX")]
        id: String,
    },
}
#[derive(Args)]
struct StatusArgs {
    /// Exact provider identifier; omit to show all existing accounts.
    provider: Option<String>,
    #[arg(long)]
    refresh: bool,
    #[arg(long, value_enum, default_value = "table", value_name = "FORMAT")]
    output_format: OutputFormat,
}
#[derive(Args)]
struct AddArgs {
    #[arg(value_parser = login_provider)]
    provider: String,
    #[arg(long, value_name = "ACCOUNT_NAME")]
    name: String,
    #[arg(long, value_enum, default_value = "browser", value_name = "METHOD")]
    login_method: LoginMethod,
}
#[derive(Args)]
struct ResetQuotaArgs {
    provider: String,
    #[arg(value_name = "ACCOUNT_ID", value_parser = account_id)]
    id: String,
    /// Confirm consuming a banked reset; without this flag, only preview.
    #[arg(long)]
    yes: bool,
    /// Explicitly reuse a prior request ID; never automatically retries a mutation.
    #[arg(long, value_name = "REQUEST_ID")]
    retry: Option<String>,
}
#[derive(Clone, Copy, ValueEnum)]
enum LoginMethod {
    Browser,
    DeviceCode,
}
#[derive(Clone, Copy, ValueEnum)]
enum OutputFormat {
    Table,
    Json,
}

fn login_provider(value: &str) -> std::result::Result<String, String> {
    if value == "codex" {
        Ok(value.to_owned())
    } else {
        Err("Unsupported login provider; currently only codex is supported".into())
    }
}

fn account_id(value: &str) -> std::result::Result<String, String> {
    uuid::Uuid::parse_str(value)
        .map(|_| value.to_owned())
        .map_err(|_| "Invalid account ID; expected a UUID".into())
}

impl From<LoginMethod> for bootstrap::LoginMethod {
    fn from(method: LoginMethod) -> Self {
        match method {
            LoginMethod::Browser => Self::Browser,
            LoginMethod::DeviceCode => Self::DeviceCode,
        }
    }
}

pub async fn run() -> Result<()> {
    let cli = Cli::parse();
    let store = bootstrap::open_store(cli.state_dir).await?;
    match cli.command {
        Command::Status(StatusArgs {
            provider,
            refresh,
            output_format,
        })
        | Command::Accounts {
            command:
                AccountsCommand::Status(StatusArgs {
                    provider,
                    refresh,
                    output_format,
                }),
        } => {
            let report = bootstrap::account_status(store, provider, refresh).await?;
            match output_format {
                OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&report)?),
                OutputFormat::Table => print!("{}", usage::status_table(&report, refresh, now())),
            }
            if report.has_errors {
                bail!("Account status includes errors; cached data is preserved above");
            }
        }
        Command::Login(AddArgs {
            provider,
            name,
            login_method,
        })
        | Command::Accounts {
            command:
                AccountsCommand::Add(AddArgs {
                    provider,
                    name,
                    login_method,
                }),
        } => {
            let account =
                bootstrap::login_provider(store, provider, name, login_method.into()).await?;
            println!(
                "Signed in: {} ({})",
                usage::terminal(&account.name),
                usage::terminal(&account.id)
            );
        }
        Command::ResetQuota(ResetQuotaArgs {
            provider,
            id,
            yes,
            retry,
        })
        | Command::Accounts {
            command:
                AccountsCommand::ResetQuota(ResetQuotaArgs {
                    provider,
                    id,
                    yes,
                    retry,
                }),
        } => {
            if retry.is_some() {
                eprintln!(
                    "Explicit retry reuses the same request ID; provider idempotency retention is not guaranteed."
                );
            }
            let report = bootstrap::reset_quota(store, provider, id, yes, retry).await?;
            print!("{}", usage::reset_table(&report));
            if report.error.is_some() || report.refresh_error.is_some() || report.outcome_unknown {
                bail!("Quota reset needs attention; see the outcome and request ID above");
            }
        }
        Command::Accounts {
            command: AccountsCommand::Login { name, login_method },
        } => {
            let account = bootstrap::login(store, name, login_method.into()).await?;
            println!(
                "Signed in: {} ({})",
                usage::terminal(&account.name),
                usage::terminal(&account.id)
            );
        }
        Command::Accounts {
            command: AccountsCommand::List { output_format },
        } => {
            let registry = account_commands::list(store).await?;
            if matches!(output_format, OutputFormat::Json) {
                println!("{}", usage::legacy_json(&registry)?);
            } else if registry.accounts.is_empty() {
                println!("No accounts. Run: llm-proxy accounts add codex --name personal");
            } else {
                println!("  ACCOUNT ID                            NAME                  STATUS");
                for account in registry.accounts {
                    let active = if registry.active.get(&account.provider) == Some(&account.id) {
                        "*"
                    } else {
                        " "
                    };
                    let status = match account.status {
                        AccountStatus::Available => "available".into(),
                        AccountStatus::NeedsLogin => "sign in again".into(),
                        AccountStatus::QuotaExhausted { reset_at } if reset_at <= now() => {
                            "quota reset; available next request".into()
                        }
                        AccountStatus::QuotaExhausted { reset_at } => format!(
                            "quota exhausted; retry in {}s (Unix {reset_at})",
                            reset_at.saturating_sub(now())
                        ),
                    };
                    println!(
                        "{active} {}  {:<20}  {status}",
                        usage::terminal(&account.id),
                        usage::terminal(&account.name)
                    );
                }
            }
        }
        Command::Accounts {
            command: AccountsCommand::Remove { id },
        } => {
            account_commands::remove(store, id).await?;
            println!(
                "Account removed. Existing requests may finish; new requests use another account."
            );
        }
        Command::Serve {
            default_model,
            listen_address,
            listen_port,
        } => {
            bootstrap::serve(store, default_model, listen_address, listen_port).await?;
        }
    }
    Ok(())
}
