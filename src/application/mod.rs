//! Transport-independent use cases; concrete implementations enter only through ports.
pub mod account_commands;
mod account_failover;
pub mod account_usage;
pub mod request_routing;

use crate::{
    config::Config,
    domain::{
        EventStream, GenerationEvent, GenerationRequest, RequestContext, RequestPurpose,
        account::AccountStatus, error::ProxyError, time::now,
    },
    ports::{
        account_repository::InferenceAccountRepository, llm_provider::ProviderRegistry,
        request_routing::RequestRoutingPolicy, token_estimator::TokenEstimator,
    },
};
use futures_util::StreamExt;
use request_routing::ConfiguredRequestRoutingPolicy;
use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, watch};

/// Keeps admitted work bounded while blocking jobs outlive their HTTP waiter.
pub type WorkPermit = Arc<OwnedSemaphorePermit>;

#[derive(Clone)]
pub struct Application {
    pub store: Arc<dyn InferenceAccountRepository>,
    pub config: Arc<Config>,
    pub providers: ProviderRegistry,
    pub request_policy: Arc<dyn RequestRoutingPolicy>,
    token_estimator: Arc<dyn TokenEstimator>,
    shutdown: watch::Sender<bool>,
}
#[derive(serde::Serialize)]
pub struct HealthStatus {
    pub status: &'static str,
    pub ready: bool,
    pub accounts: usize,
    pub usable_accounts: usize,
    pub provider: String,
}
impl Application {
    pub fn new<R: InferenceAccountRepository + 'static>(
        store: R,
        config: Config,
        providers: ProviderRegistry,
        token_estimator: Arc<dyn TokenEstimator>,
    ) -> Self {
        let config = Arc::new(config);
        let request_policy = Arc::new(ConfiguredRequestRoutingPolicy::new(config.clone()));
        let (shutdown, _) = watch::channel(false);
        Self {
            store: Arc::new(store),
            config,
            providers,
            request_policy,
            token_estimator,
            shutdown,
        }
    }
    pub fn shutdown(&self) {
        self.shutdown.send_replace(true);
    }
    /// Complete preflight before returning a lazy stream, preserving pre-SSE errors.
    pub async fn generate(
        &self,
        request: GenerationRequest,
        mut context: RequestContext,
        permit: WorkPermit,
    ) -> Result<EventStream, ProxyError> {
        self.config
            .recognize_model(&request.model)
            .map_err(|error| ProxyError::invalid(error.to_string()).with_code("unknown_model"))?;
        let route = Arc::new(self.request_policy.route(&request)?);
        let provider = self.providers.get(&route.provider)?;
        provider.validate_request(&request, &route)?;
        let requested_model = request.model.clone();
        tracing::debug!(
            request_id = %context.request_id,
            request_purpose = request.purpose.as_str(),
            requested_model = %requested_model,
            routed_model = %route.model,
            provider = %route.provider,
            event = "request_route_selected"
        );
        if request.purpose == RequestPurpose::ActionReview {
            context.session_id = None;
        }
        let store = self.store.clone();
        let provider_name = route.provider.clone();
        let preparation_permit = permit.clone();
        let account = tokio::task::spawn_blocking(move || {
            let _permit = preparation_permit;
            store.select_for_provider_at(&provider_name, &std::collections::HashSet::new(), now())
        })
        .await
        .map_err(|_| ProxyError::storage())??;
        Ok(account_failover::generate(
            self.clone(),
            Arc::new(request),
            context,
            account,
            permit,
            route,
        )
        .map(move |event| {
            event.map(|event| match event {
                GenerationEvent::Start {
                    id, input_tokens, ..
                } => GenerationEvent::Start {
                    id,
                    input_tokens,
                    model: requested_model.clone(),
                },
                event => event,
            })
        })
        .boxed())
    }
    pub async fn count_tokens(
        &self,
        request: GenerationRequest,
        permit: WorkPermit,
    ) -> Result<u64, ProxyError> {
        self.config
            .recognize_model(&request.model)
            .map_err(|error| ProxyError::invalid(error.to_string()).with_code("unknown_model"))?;
        let estimator = self.token_estimator.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            estimator.estimate(&request)
        })
        .await
        .map_err(|_| ProxyError::protocol("Token estimator failed"))
    }
    pub async fn health(&self) -> Result<HealthStatus, ProxyError> {
        let store = self.store.clone();
        let registry = tokio::task::spawn_blocking(move || store.list())
            .await
            .map_err(|_| ProxyError::storage())?
            .map_err(|_| ProxyError::storage())?;
        let usable = registry.accounts.iter()
            .filter(|account| account.provider == self.config.default_provider)
            .filter(|account| matches!(account.status, AccountStatus::Available)
                || matches!(account.status, AccountStatus::QuotaExhausted { reset_at } if reset_at <= now()))
            .count();
        Ok(HealthStatus {
            status: "ok",
            ready: usable > 0 && self.providers.get(&self.config.default_provider).is_ok(),
            accounts: registry.accounts.len(),
            usable_accounts: usable,
            provider: self.config.default_provider.clone(),
        })
    }
}
