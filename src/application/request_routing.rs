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
        let model = if review {
            self.config.auto_review.model.as_deref()
        } else {
            None
        }
        .unwrap_or(&request.model);
        self.config
            .resolve_route(
                model,
                if review {
                    self.config.auto_review.provider.as_deref()
                } else {
                    None
                },
            )
            .map_err(|error| ProxyError::invalid(error.to_string()).with_code("unknown_model"))
    }
}
