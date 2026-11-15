//! Composition root for concrete adapters and the proxy process lifecycle.
use crate::{
    accounts::{Account, Store},
    adapters::{
        inbound::http::{AppState, anthropic::Anthropic, router},
        outbound::{
            agy::client::Agy,
            codex::{CodexTokenEstimator, client::Codex},
        },
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
    login_provider(store, "codex".into(), name, method).await
}

pub async fn login_provider(
    store: Store,
    provider: String,
    name: String,
    method: LoginMethod,
) -> Result<Account> {
    anyhow::ensure!(
        matches!(provider.as_str(), "codex" | "agy"),
        "Unsupported login provider; available: codex, agy"
    );
    anyhow::ensure!(
        provider != "agy" || matches!(method, LoginMethod::Browser),
        "Antigravity does not support device-code login; use accounts add agy --name NAME"
    );
    let agy = provider == "agy";
    account_commands::login_for(store, provider, name, async move {
        if agy {
            return AuthClient::agy()?.agy_browser_login().await;
        }
        let auth = AuthClient::new()?;
        match method {
            LoginMethod::Browser => auth.browser_login().await,
            LoginMethod::DeviceCode => auth.device_login().await,
        }
    })
    .await
}

async fn account_usage_service(
    store: Store,
    live: bool,
) -> std::result::Result<
    crate::application::account_usage::AccountUsageService,
    crate::domain::account_usage::UsageError,
> {
    use crate::{
        adapters::outbound::codex::account_usage::CodexAccountUsage,
        application::account_usage::AccountUsageService, domain::account_usage::UsageError,
    };
    let implementation = if live {
        let client_store = store.clone();
        tokio::task::spawn_blocking(move || {
            let config =
                Config::load(client_store.root()).map_err(|_| UsageError::Configuration)?;
            config::validate_endpoint(&config.codex_account_endpoint)
                .map_err(|_| UsageError::Configuration)?;
            let auth = AuthClient::new().map_err(|_| UsageError::Configuration)?;
            CodexAccountUsage::with_auth(client_store, config.codex_account_endpoint, auth)
        })
        .await
        .map_err(|_| UsageError::Configuration)??
    } else {
        CodexAccountUsage::cached()
    };
    Ok(AccountUsageService::new(
        store,
        vec![
            Arc::new(implementation),
            Arc::new(crate::adapters::outbound::agy::account_usage::AgyAccountUsage),
        ],
    ))
}

pub async fn account_status(
    store: Store,
    provider: Option<String>,
    refresh: bool,
) -> std::result::Result<
    crate::domain::account_usage::StatusReport,
    crate::domain::account_usage::UsageError,
> {
    use crate::domain::account_usage::UsageError;
    // AGY has no live usage capability; filtering it must not initialize an
    // unrelated Codex client or depend on Codex configuration.
    let codex_live = refresh && provider.as_deref() != Some("agy");
    let service = account_usage_service(store, codex_live).await?;
    let result = tokio::select! {
        result = service.status(provider.as_deref(), refresh) => result,
        _ = tokio::signal::ctrl_c() => Err(UsageError::Interrupted),
    };
    service.shutdown().await;
    result
}

pub async fn reset_quota(
    store: Store,
    provider: String,
    id: String,
    confirm: bool,
    retry: Option<String>,
) -> std::result::Result<
    crate::domain::account_usage::ResetReport,
    crate::domain::account_usage::UsageError,
> {
    use crate::domain::account_usage::UsageError;
    if provider != "codex" {
        return Err(UsageError::Unsupported);
    }
    let service = account_usage_service(store, true).await?;
    let result = tokio::select! {
        result = service.reset(&provider, &id, confirm, retry.as_deref()) => result,
        _ = tokio::signal::ctrl_c() => Err(UsageError::Interrupted),
    };
    service.shutdown().await;
    result
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
    let provider_store = store.clone();
    let provider_config = Arc::new(config.clone());
    let agy =
        tokio::task::spawn_blocking(move || Agy::new(provider_store, provider_config)).await??;
    providers.register(Arc::new(agy));
    providers.get(&config.default_provider)?;
    for policy in config.models.values() {
        if let Some(provider) = &policy.provider {
            providers.get(provider)?;
        }
    }
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
