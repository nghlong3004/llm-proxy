//! Composition root for concrete adapters and the proxy process lifecycle.
use crate::{
    accounts::{Account, Store},
    adapters::{
        inbound::http::{AppState, anthropic::Anthropic, router},
        outbound::codex::{CodexTokenEstimator, client::Codex},
    },
    application::{Application, account_commands},
    authentication::AuthClient,
    config::{self, Config},
    ports::llm_provider::ProviderRegistry,
};
use anyhow::{Context, Result};
use std::{
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    sync::Arc,
};

pub enum LoginMethod {
    Browser,
    DeviceCode,
}

pub async fn open_store(state_dir: Option<PathBuf>) -> Result<Store> {
    let directory = state_dir.map(Ok).unwrap_or_else(config::state_dir)?;
    tokio::task::spawn_blocking(move || Store::open(directory)).await?
}

pub async fn login(store: Store, name: String, method: LoginMethod) -> Result<Account> {
    account_commands::login(store, name, async move {
        let auth = AuthClient::new()?;
        match method {
            LoginMethod::Browser => auth.browser_login().await,
            LoginMethod::DeviceCode => auth.device_login().await,
        }
    })
    .await
}

pub fn http_state(store: Store, config: Config, providers: ProviderRegistry) -> AppState {
    let application = Application::new(store, config, providers, Arc::new(CodexTokenEstimator));
    AppState::new(application, Arc::new(Anthropic))
}

pub async fn serve(
    store: Store,
    default_model: Option<String>,
    listen_address: Option<IpAddr>,
    listen_port: Option<u16>,
) -> Result<()> {
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
    let _log_guard =
        tokio::task::spawn_blocking(move || crate::logging::initialize(&log_directory, &log_level))
            .await??;
    let mut providers = ProviderRegistry::default();
    let provider_store = store.clone();
    let provider_config = Arc::new(config.clone());
    let codex =
        tokio::task::spawn_blocking(move || Codex::new(provider_store, provider_config)).await??;
    providers.register(Arc::new(codex));
    providers.get(&config.default_provider)?;
    if let Some(provider) = &config.auto_review.provider {
        providers.get(provider)?;
    }
    let address = SocketAddr::new(config.listen_address, config.listen_port);
    let state = http_state(store, config, providers);
    let shutdown = state.clone();
    let providers = state.application.providers.clone();
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .with_context(|| format!("Cannot listen on {address}"))?;
    println!("llm-proxy listening at http://{address}");
    tracing::info!(
        listen_address = %address.ip(),
        listen_port = address.port(),
        provider = %state.application.config.default_provider,
        default_model = ?state.application.config.default_model,
        event = "proxy_started"
    );
    axum::serve(listener, router(state))
        .with_graceful_shutdown(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                shutdown.shutdown();
            }
        })
        .await?;
    providers.shutdown().await;
    tracing::info!(event = "proxy_stopped");
    Ok(())
}
