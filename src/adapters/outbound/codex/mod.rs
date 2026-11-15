//! Codex provider: native wire protocol, independent transport and error classification.
pub mod client;
pub mod quota;
pub mod reasoning;
pub mod request;
mod schema;
pub mod search;
pub mod sse;
pub mod stream;

/// Tokenizer implementation injected by the composition root.
pub struct CodexTokenEstimator;
impl crate::ports::token_estimator::TokenEstimator for CodexTokenEstimator {
    fn estimate(&self, input: &crate::domain::GenerationRequest) -> u64 {
        request::estimate_tokens(input)
    }
}
