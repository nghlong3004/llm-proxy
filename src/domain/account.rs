//! Provider-neutral account snapshots, identity and lifecycle inputs.
use crate::domain::account_usage::UsageObservation;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Serialize, Deserialize)]
pub struct Credential {
    pub access_token: String,
    pub refresh_token: String,
    pub id_token: Option<String>,
    pub expires_at: u64,
    pub subject: String,
    pub account_id: String,
    pub generation: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AccountStatus {
    #[default]
    Available,
    QuotaExhausted {
        reset_at: u64,
    },
    NeedsLogin,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccountSnapshot {
    #[serde(alias = "backend")]
    pub provider: String,
    pub id: String,
    #[serde(alias = "label")]
    pub name: String,
    pub subject: String,
    pub workspace: String,
    pub generation: String,
    pub status: AccountStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_observation: Option<UsageObservation>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub status_revision: u64,
}

impl AccountSnapshot {
    /// Only the verified identity needed at a provider boundary, not cached account state.
    pub fn identity(&self) -> AccountIdentity {
        AccountIdentity {
            provider: self.provider.clone(),
            id: self.id.clone(),
            subject: self.subject.clone(),
            workspace: self.workspace.clone(),
            generation: self.generation.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountIdentity {
    pub provider: String,
    pub id: String,
    pub subject: String,
    pub workspace: String,
    pub generation: String,
}

/// Account metadata without persistence schema or operation bookkeeping.
#[derive(Clone, Debug, Default)]
pub struct AccountCatalog {
    pub accounts: Vec<AccountSnapshot>,
    pub active: BTreeMap<String, String>,
}

/// Reserved before OAuth starts so an older login cannot undo a newer action.
pub struct LoginTicket {
    pub(crate) provider: String,
    pub(crate) sequence: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum SelectionError {
    #[error(
        "No signed-in account is available. Run: llm-proxy accounts add PROVIDER --name NAME (codex or agy)"
    )]
    NoAccounts,
    #[error("All accounts have exhausted their quota; retry in {retry_after} seconds")]
    Exhausted { retry_after: u64 },
    #[error("Cannot read account storage")]
    Storage(#[source] anyhow::Error),
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}
