//! Per-request routing contract, independent of transport and concrete providers.
use crate::domain::{GenerationRequest, error::ProxyError};

/// Immutable destination and resolved provider options retained across account failover.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestRoute {
    pub provider: String,
    pub model: String,
    pub options: RouteOptions,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RouteOptions {
    Codex(CodexOptions),
    Standard,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodexOptions {
    pub responses_lite: bool,
    pub service_tier: Option<ServiceTier>,
    pub search_model: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceTier {
    Priority,
    Ultrafast,
}
impl ServiceTier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Priority => "priority",
            Self::Ultrafast => "ultrafast",
        }
    }
}

pub trait RequestRoutingPolicy: Send + Sync {
    fn route(&self, request: &GenerationRequest) -> Result<RequestRoute, ProxyError>;
}
