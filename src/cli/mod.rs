//! CLI convention: resource/action commands; kebab-case flags mirror config keys.
use crate::{
    accounts::{AccountStatus, Store, now},
    authentication::AuthClient,
    config::{self, Config},
    providers::{ProviderRegistry, codex::client::Codex},
    server::{AppState, router},
};
use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use std::{path::PathBuf, sync::Arc};

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
    let directory = cli.state_dir.map(Ok).unwrap_or_else(config::state_dir)?;
    let store = tokio::task::spawn_blocking(move || Store::open(directory)).await??;
    match cli.command {
        Command::Accounts {
            command: AccountsCommand::Login { name, login_method },
        } => {
            anyhow::ensure!(
                !name.trim().is_empty() && name.len() <= 200,
                "Account name must contain 1–200 bytes"
            );
            let login_store = store.clone();
            let ticket =
                tokio::task::spawn_blocking(move || login_store.begin_login("codex")).await??;
            let auth = AuthClient::new()?;
            let credential = match login_method {
                LoginMethod::Browser => auth.browser_login().await?,
                LoginMethod::DeviceCode => auth.device_login().await?,
            };
            let account =
                tokio::task::spawn_blocking(move || store.finish_login(ticket, &name, credential))
                    .await??;
            println!("Signed in: {} ({})", account.name, account.id);
        }
        Command::Accounts {
            command: AccountsCommand::List { output_format },
        } => {
            let registry = tokio::task::spawn_blocking(move || store.list()).await??;
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
            tokio::task::spawn_blocking(move || store.remove(&id)).await??;
            println!(
                "Account removed. Existing requests may finish; new requests use another account."
            );
        }
        Command::Serve {
            default_model,
            listen_address,
            listen_port,
        } => {
            let config_store = store.clone();
            let mut config =
                tokio::task::spawn_blocking(move || Config::load(config_store.root())).await??;
            if let Some(model) = default_model {
                config.default_model = Some(model);
            }
            if let Some(address) = listen_address {
                config.listen_address = address;
            }
            if let Some(port) = listen_port {
                config.listen_port = port;
            }
            config.validate()?;
            let log_directory = store.root().to_owned();
            let log_level = config.log_level.clone();
            let _log_guard = tokio::task::spawn_blocking(move || {
                crate::logging::initialize(&log_directory, &log_level)
            })
            .await??;
            let mut providers = ProviderRegistry::default();
            let provider_store = store.clone();
            let provider_config = Arc::new(config.clone());
            let codex =
                tokio::task::spawn_blocking(move || Codex::new(provider_store, provider_config))
                    .await??;
            providers.register(Arc::new(codex));
            providers.get(&config.default_provider)?;
            if let Some(provider) = &config.auto_review.provider {
                providers.get(provider)?;
            }
            let address = std::net::SocketAddr::new(config.listen_address, config.listen_port);
            let state = AppState::new(store, config, providers);
            let shutdown = state.clone();
            let providers = state.providers.clone();
            let listener = tokio::net::TcpListener::bind(address)
                .await
                .with_context(|| format!("Cannot listen on {address}"))?;
            println!("llm-proxy listening at http://{address}");
            tracing::info!(listen_address = %address.ip(), listen_port = address.port(), provider = %state.config.default_provider,
                default_model = ?state.config.default_model, event = "proxy_started");
            axum::serve(listener, router(state))
                .with_graceful_shutdown(async move {
                    if tokio::signal::ctrl_c().await.is_ok() {
                        shutdown.shutdown();
                    }
                })
                .await?;
            providers.shutdown().await;
            tracing::info!(event = "proxy_stopped");
        }
    }
    Ok(())
}
