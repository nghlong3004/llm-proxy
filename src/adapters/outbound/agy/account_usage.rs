use crate::{
    domain::account_usage::{UsageCapabilities, UsageData, UsageError},
    ports::account_usage::AccountUsageProvider,
};
use serde_json::Value;

/// AGY does not yet expose a verified account telemetry or quota-reset contract.
pub struct AgyAccountUsage;

impl AccountUsageProvider for AgyAccountUsage {
    fn name(&self) -> &'static str {
        "agy"
    }

    fn capabilities(&self) -> UsageCapabilities {
        UsageCapabilities::default()
    }

    fn empty_usage(&self) -> UsageData {
        UsageData::unsupported()
    }

    fn decode_cached(&self, _snapshot: &Value) -> Result<UsageData, UsageError> {
        Ok(UsageData::unsupported())
    }
}
