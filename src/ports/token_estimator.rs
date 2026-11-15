//! Local token estimation contract; application controls blocking-job ownership.
use crate::domain::GenerationRequest;
pub trait TokenEstimator: Send + Sync {
    fn estimate(&self, request: &GenerationRequest) -> u64;
}
