//! Adapter pattern: translate a client protocol to/from the shared generation contract.
pub mod anthropic;
use crate::{
    domain::error::ProxyError,
    domain::{GenerationEvent, GenerationOutput, GenerationRequest},
};
use serde_json::Value;
pub trait ProtocolAdapter: Send + Sync {
    fn decode(&self, value: Value) -> Result<GenerationRequest, ProxyError>;
    fn encode_event(&self, event: GenerationEvent) -> Vec<(String, Value)>;
    fn encode_response(&self, response: GenerationOutput) -> Value;
    fn encode_error(&self, error: &ProxyError) -> Value;
    fn encode_stream_error(&self, error: &ProxyError) -> Vec<(String, Value)>;
}
pub mod sse;
