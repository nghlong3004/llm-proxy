//! Per-request routing contract, independent of transport and concrete providers.
use crate::domain::{GenerationRequest, error::ProxyError};

/// Immutable destination retained across account failover.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestRoute {
    pub provider: String,
    pub model: String,
}

pub trait RequestRoutingPolicy: Send + Sync {
    fn route(&self, request: &GenerationRequest) -> Result<RequestRoute, ProxyError>;
}
