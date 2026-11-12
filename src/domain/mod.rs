//! Provider-neutral conversation, generation, accumulation and failure contracts.
pub mod accumulator;
pub mod error;
mod generation;
pub use generation::*;
