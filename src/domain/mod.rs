//! Provider-neutral conversation, generation, accumulation and failure contracts.
pub mod account;
pub mod account_usage;
pub mod accumulator;
pub mod error;
mod generation;
pub mod time;
pub use generation::*;
