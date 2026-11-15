//! Provider-neutral account telemetry and explicitly requested quota-reset results.
use crate::domain::account::AccountStatus;
use serde::{Deserialize, Serialize};

/// A missing or unsupported metric is not a reported zero.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(tag = "state", content = "value", rename_all = "snake_case")]
pub enum Metric<T> {
    Known(T),
    #[default]
    Unknown,
    Unsupported,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct UsageWindow {
    pub id: String,
    pub label: String,
    pub scope: String,
    pub duration_seconds: Option<u64>,
    pub used_percent: Option<f64>,
    pub reset_at: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct PrepaidCredits {
    pub balance: Metric<String>,
    pub has_credits: Option<bool>,
    pub unlimited: Option<bool>,
    pub spend_blocked: Option<bool>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct ResetCredits {
    pub available: Metric<u64>,
    pub earliest_expiry: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct UsageData {
    pub windows: Metric<Vec<UsageWindow>>,
    pub prepaid_credits: Metric<PrepaidCredits>,
    pub reset_credits: Metric<ResetCredits>,
    pub ordinary_usage_allowed: Option<bool>,
}
impl UsageData {
    pub fn unsupported() -> Self {
        Self {
            windows: Metric::Unsupported,
            prepaid_credits: Metric::Unsupported,
            reset_credits: Metric::Unsupported,
            ordinary_usage_allowed: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UsageSource {
    Inference,
    Live,
    #[default]
    LegacyUnknown,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct UsageObservation {
    pub observed_at: Option<u64>,
    pub source: UsageSource,
}

/// Provider support, independent of whether this invocation initialized a live client.
#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct UsageCapabilities {
    pub live_read: bool,
    pub reset: bool,
}

/// Static diagnostics only: upstream bodies and credential error chains are never displayed.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum UsageError {
    #[error("This provider does not support this account operation")]
    Unsupported,
    #[error("Unknown provider; use status without a provider to view existing accounts")]
    UnknownProvider,
    #[error("Account not found")]
    NotFound,
    #[error("Account does not belong to the selected provider")]
    ProviderMismatch,
    #[error("Cannot read or persist account state")]
    Storage,
    #[error("Invalid account-service configuration; check codex_account_endpoint")]
    Configuration,
    #[error("Account was removed or its login changed; run status again")]
    AccountChanged,
    #[error("Account needs authentication; sign in again")]
    Authentication,
    #[error("Account service request timed out or could not connect")]
    Transport,
    #[error("Account service rejected the request or is unavailable")]
    Upstream,
    #[error("Account service returned an invalid or unexpected response")]
    Protocol,
    #[error("Another quota reset is in progress for this account")]
    Busy,
    #[error("Account status work exceeds the supported capacity")]
    Capacity,
    #[error(
        "Operation interrupted; a reset may have completed, run status to find any unresolved request before retrying"
    )]
    Interrupted,
    #[error("No banked reset credits are available")]
    NoResetCredits,
    #[error("Reset-credit availability is unknown; refresh usage before redeeming")]
    UnknownResetCredits,
    #[error("A previous reset has an unresolved outcome; resume its request ID, not a new reset")]
    PendingReset,
    #[error("Retry ID does not match a recorded reset for this account and login")]
    InvalidRetry,
}

/// Native, allowlisted cache data accompanies the normalized public view.
#[derive(Clone, Debug)]
pub struct UsageRead {
    pub snapshot: serde_json::Value,
    pub data: UsageData,
    pub warning: Option<UsageError>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResetOutcome {
    Reset { windows_reset: u64 },
    NothingToReset,
    NoCredit,
    AlreadyRedeemed,
}
impl ResetOutcome {
    pub fn confirms_redemption(&self) -> bool {
        matches!(self, Self::Reset { .. } | Self::AlreadyRedeemed)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ResetDispatchError {
    pub error: UsageError,
    pub dispatched: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "state", content = "outcome", rename_all = "snake_case")]
pub enum ResetAttemptState {
    Pending,
    Unknown,
    NotDispatched,
    Confirmed(ResetOutcome),
}

/// Durable journal metadata contains no tokens and is bound to one account generation.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ResetAttempt {
    pub provider: String,
    pub account_id: String,
    pub generation: String,
    pub request_id: String,
    pub status_revision: u64,
    pub created_at: u64,
    pub state: ResetAttemptState,
}

#[derive(Clone, Debug, Serialize)]
pub struct AccountUsageView {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub active: bool,
    pub status: AccountStatus,
    pub capabilities: UsageCapabilities,
    pub usage: UsageData,
    pub observation: UsageObservation,
    pub refresh_error: Option<UsageError>,
    pub warning: Option<UsageError>,
    pub pending_reset_id: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct StatusReport {
    pub accounts: Vec<AccountUsageView>,
    pub has_errors: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ResetReport {
    pub provider: String,
    pub account_id: String,
    pub request_id: Option<String>,
    pub preview: bool,
    pub available: Metric<u64>,
    pub outcome: Option<ResetOutcome>,
    pub outcome_unknown: bool,
    pub error: Option<UsageError>,
    pub refresh_error: Option<UsageError>,
    pub reconciled: bool,
}
