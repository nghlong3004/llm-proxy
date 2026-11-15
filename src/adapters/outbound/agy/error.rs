//! Only definite credential rejection permits authentication retry/failover.
use crate::domain::error::{FailureKind, ProxyError};
use serde_json::Value;

pub fn classify(status: u16, value: &Value) -> Option<ProxyError> {
    let error = value
        .get("error")
        .or_else(|| value.pointer("/response/error"));
    if status < 400 && error.is_none() {
        return None;
    }
    let status = if status >= 400 {
        status
    } else {
        error
            .and_then(|error| error.get("code"))
            .and_then(Value::as_u64)
            .and_then(|status| u16::try_from(status).ok())
            .filter(|status| (400..600).contains(status))
            .unwrap_or(502)
    };
    let (public_status, kind, message) = match status {
        401 => (
            401,
            FailureKind::Authentication,
            "AGY rejected the account credential",
        ),
        403 => (403, FailureKind::Permission, "AGY denied this request"),
        400 | 404 | 405 | 422 => (
            400,
            FailureKind::InvalidRequest,
            "AGY rejected the request or model",
        ),
        // RESOURCE_EXHAUSTED, generic 429 and retry hints do not prove subscription exhaustion.
        429 => (
            429,
            FailureKind::RateLimit,
            "AGY temporarily rate limited the request",
        ),
        _ => (502, FailureKind::Upstream, "AGY returned an upstream error"),
    };
    let mut error = ProxyError::new(public_status, kind, message);
    error.upstream_status = Some(status);
    Some(error)
}
