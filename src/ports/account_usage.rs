//! Optional account telemetry capabilities, separate from inference and HTTP.
use crate::{
    accounts::Account,
    domain::account_usage::{
        ResetDispatchError, ResetOutcome, UsageCapabilities, UsageData, UsageError, UsageRead,
    },
};
use futures_util::future::BoxFuture;
use serde_json::Value;

pub trait AccountUsageProvider: Send + Sync {
    fn name(&self) -> &'static str;
    fn capabilities(&self) -> UsageCapabilities;
    fn decode_cached(&self, snapshot: &Value) -> Result<UsageData, UsageError>;
    fn read(&self, _account: Account) -> BoxFuture<'_, Result<UsageRead, UsageError>> {
        Box::pin(async { Err(UsageError::Unsupported) })
    }
    fn reset(
        &self,
        _account: Account,
        _request_id: String,
    ) -> BoxFuture<'_, Result<ResetOutcome, ResetDispatchError>> {
        Box::pin(async {
            Err(ResetDispatchError {
                error: UsageError::Unsupported,
                dispatched: false,
            })
        })
    }
    fn shutdown(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
}
