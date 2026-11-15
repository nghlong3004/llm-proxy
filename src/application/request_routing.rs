//! Request-routing Strategy: choose a provider/model without protocol or transport knowledge.
use crate::{
    config::Config,
    domain::{GenerationRequest, RequestPurpose, error::ProxyError},
};
use std::sync::Arc;

use crate::ports::request_routing::{RequestRoute, RequestRoutingPolicy};

/// Configuration-driven routing, independent of the concrete provider implementations.
pub struct ConfiguredRequestRoutingPolicy {
    config: Arc<Config>,
}
impl ConfiguredRequestRoutingPolicy {
    pub fn new(config: Arc<Config>) -> Self {
        Self { config }
    }
}
impl RequestRoutingPolicy for ConfiguredRequestRoutingPolicy {
    fn route(&self, request: &GenerationRequest) -> Result<RequestRoute, ProxyError> {
        let review = request.purpose == RequestPurpose::ActionReview;
        let provider = if review {
            self.config.auto_review.provider.as_deref()
        } else {
            None
        }
        .unwrap_or(&self.config.default_provider);
        let model = if review {
            self.config.auto_review.model.as_deref()
        } else {
            None
        }
        .unwrap_or(&request.model);
        if provider.trim().is_empty() || model.trim().is_empty() {
            return Err(
                ProxyError::invalid("Request route provider and model must not be empty")
                    .with_code("invalid_request_route"),
            );
        }
        Ok(RequestRoute {
            provider: provider.to_owned(),
            model: model.to_owned(),
        })
    }
}
