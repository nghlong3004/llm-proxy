//! Strategy pattern: routing selects a provider without depending on its protocol.
use crate::{
    accounts::Account,
    domain::error::ProxyError,
    domain::{EventStream, GenerationRequest, RequestContext},
};
use futures_util::future::BoxFuture;
use std::{collections::BTreeMap, sync::Arc};
pub trait Provider: Send + Sync {
    fn name(&self) -> &'static str;
    fn shutdown(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
    fn generate(
        &self,
        request: Arc<GenerationRequest>,
        account: Account,
        context: RequestContext,
    ) -> BoxFuture<'static, Result<EventStream, ProxyError>>;
}
#[derive(Clone, Default)]
pub struct ProviderRegistry {
    providers: BTreeMap<String, Arc<dyn Provider>>,
}
impl ProviderRegistry {
    pub fn register(&mut self, provider: Arc<dyn Provider>) {
        self.providers.insert(provider.name().into(), provider);
    }
    pub async fn shutdown(&self) {
        futures_util::future::join_all(self.providers.values().map(|provider| provider.shutdown()))
            .await;
    }
    pub fn get(&self, name: &str) -> Result<Arc<dyn Provider>, ProxyError> {
        self.providers
            .get(name)
            .cloned()
            .ok_or_else(|| ProxyError::invalid(format!("Provider '{name}' is not registered")))
    }
}

pub mod codex;
