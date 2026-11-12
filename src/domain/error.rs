//! Errors preserve retry classification without retaining credential-bearing bodies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FailureKind {
    AccountChanged,
    Quota { reset_at: u64 },
    Authentication,
    RateLimit,
    Permission,
    InvalidRequest,
    Upstream,
    Transport,
    Protocol,
    NoAccounts,
    Storage,
}
#[derive(Clone, Debug, thiserror::Error)]
#[error("{message}")]
pub struct ProxyError {
    /// Stable, static diagnostic label: safe to log without payloads or error text.
    pub code: &'static str,
    pub status: u16,
    pub kind: FailureKind,
    pub message: String,
    pub retry_after: Option<u64>,
    pub upstream_status: Option<u16>,
    /// Only recognized wire field names; never retain an upstream-supplied value.
    pub upstream_parameter: Option<&'static str>,
}
impl ProxyError {
    pub fn new(status: u16, kind: FailureKind, message: impl Into<String>) -> Self {
        let code = match kind {
            FailureKind::InvalidRequest => "invalid_request",
            FailureKind::AccountChanged => "account_changed",
            FailureKind::Quota { .. } => "subscription_quota_exhausted",
            FailureKind::Authentication => "account_authentication_failed",
            FailureKind::RateLimit => "rate_limited",
            FailureKind::Permission => "upstream_permission_denied",
            FailureKind::Upstream => "upstream_error",
            FailureKind::Transport => "upstream_transport_error",
            FailureKind::Protocol => "upstream_protocol_error",
            FailureKind::NoAccounts => "no_usable_account",
            FailureKind::Storage => "account_storage_error",
        };
        Self {
            code,
            status,
            kind,
            message: message.into(),
            retry_after: None,
            upstream_status: None,
            upstream_parameter: None,
        }
    }
    pub fn with_code(mut self, code: &'static str) -> Self {
        self.code = code;
        self
    }
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(400, FailureKind::InvalidRequest, message)
    }
    pub fn protocol(message: impl Into<String>) -> Self {
        Self::new(502, FailureKind::Protocol, message)
    }
    pub fn storage() -> Self {
        Self::new(
            500,
            FailureKind::Storage,
            "Cannot access proxy account storage",
        )
    }
    pub fn transport() -> Self {
        Self::new(
            502,
            FailureKind::Transport,
            "Provider connection failed; this request was not automatically replayed",
        )
    }
    pub fn is_quota(&self) -> bool {
        matches!(self.kind, FailureKind::Quota { .. })
    }
    pub fn quota_reset(&self) -> Option<u64> {
        match self.kind {
            FailureKind::Quota { reset_at } => Some(reset_at),
            _ => None,
        }
    }
}
impl From<crate::accounts::SelectionError> for ProxyError {
    fn from(error: crate::accounts::SelectionError) -> Self {
        match error {
            crate::accounts::SelectionError::Exhausted { retry_after } => Self {
                code: "subscription_quota_exhausted",
                status: 429,
                kind: FailureKind::Quota {
                    reset_at: crate::accounts::now().saturating_add(retry_after),
                },
                message: "All signed-in accounts have exhausted their quota".into(),
                retry_after: Some(retry_after),
                upstream_status: None,
                upstream_parameter: None,
            },
            crate::accounts::SelectionError::NoAccounts => Self::new(
                503,
                FailureKind::NoAccounts,
                "No usable account. Run: llm-proxy accounts login --name NAME",
            ),
            crate::accounts::SelectionError::Storage(_) => Self::storage(),
        }
    }
}
