//! Synchronous account lifecycle operations; callers offload blocking implementations.
use crate::domain::{
    account::{AccountCatalog, AccountSnapshot, AccountStatus, Credential, SelectionError},
    account_usage::{ResetAttempt, UsageObservation},
};
use anyhow::Result;
use serde_json::Value;
use std::{collections::HashSet, sync::Arc};

pub trait InferenceAccountRepository: Send + Sync {
    fn list(&self) -> Result<AccountCatalog>;
    /// Validate credentials and atomically select before returning the account snapshot.
    fn select_for_provider_at(
        &self,
        provider: &str,
        excluded: &HashSet<String>,
        time: u64,
    ) -> Result<AccountSnapshot, SelectionError>;
    fn record_usage(&self, id: &str, generation: &str, usage: Value) -> Result<()>;
    fn mark_quota_at(&self, id: &str, generation: &str, reset_at: u64, time: u64) -> Result<()>;
    fn mark_needs_login(&self, id: &str, generation: &str) -> Result<()>;
}

pub trait AccountCommandRepository: Send + Sync {
    type Ticket: Send + 'static;

    fn begin_login(&self, provider: &str) -> Result<Self::Ticket>;
    fn finish_login(
        &self,
        ticket: Self::Ticket,
        name: &str,
        credential: Credential,
    ) -> Result<AccountSnapshot>;
    fn remove(&self, id: &str) -> Result<()>;
}

/// Shared ownership of a repository's reset lock, released only after the last clone drops.
/// The resource remains opaque so application workflows cannot unlock it prematurely.
#[derive(Clone)]
pub struct ResetGuard {
    _resource: Arc<dyn Send + Sync>,
}

impl ResetGuard {
    pub fn new(resource: impl Send + Sync + 'static) -> Self {
        Self {
            _resource: Arc::new(resource),
        }
    }
}

pub trait AccountUsageRepository: Send + Sync {
    fn list(&self) -> Result<AccountCatalog>;
    fn record_usage_observed(
        &self,
        id: &str,
        generation: &str,
        usage: Value,
        observation: UsageObservation,
    ) -> Result<bool>;
    fn reconcile_refreshed_quota(
        &self,
        observed: &AccountSnapshot,
    ) -> Result<Option<AccountStatus>>;
    fn reset_attempt(&self, id: &str) -> Result<Option<ResetAttempt>>;
    fn save_reset_attempt(&self, attempt: &ResetAttempt) -> Result<()>;
    fn reconcile_quota_reset(&self, attempt: &ResetAttempt, allowed: bool) -> Result<bool>;
    fn try_quota_reset_lock(&self, id: &str) -> Result<Option<ResetGuard>>;
}
