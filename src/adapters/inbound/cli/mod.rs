//! CLI convention: resource/action commands; kebab-case flags mirror config keys.
use crate::{
    accounts::{AccountStatus, now},
    application::account_commands,
    bootstrap,
};
use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
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
    /// Add or renew a proxy-owned ChatGPT account.
    Login {
        #[arg(long, value_name = "ACCOUNT_NAME")]
        name: String,
        #[arg(long, value_enum, default_value = "browser", value_name = "METHOD")]
        login_method: LoginMethod,
    },
    /// Show account status without printing credentials.
    List {
        #[arg(long, value_enum, default_value = "table", value_name = "FORMAT")]
        output_format: OutputFormat,
    },
    /// Remove an account; existing requests may finish.
    Remove {
        #[arg(value_name = "ACCOUNT_ID")]
        id: String,
    },
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

pub async fn run() -> Result<()> {
    let cli = Cli::parse();
    let store = bootstrap::open_store(cli.state_dir).await?;
    match cli.command {
        Command::Accounts {
            command: AccountsCommand::Login { name, login_method },
        } => {
            let method = match login_method {
                LoginMethod::Browser => bootstrap::LoginMethod::Browser,
                LoginMethod::DeviceCode => bootstrap::LoginMethod::DeviceCode,
            };
            let account = bootstrap::login(store, name, method).await?;
            println!("Signed in: {} ({})", account.name, account.id);
        }
        Command::Accounts {
            command: AccountsCommand::List { output_format },
        } => {
            let registry = account_commands::list(store).await?;
            if matches!(output_format, OutputFormat::Json) {
                println!("{}", serde_json::to_string_pretty(&registry)?);
            } else if registry.accounts.is_empty() {
                println!("No accounts. Run: llm-proxy accounts login --name personal");
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
                    println!("{active} {}  {:<20}  {status}", account.id, account.name);
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
