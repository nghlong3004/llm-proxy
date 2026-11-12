//! Recognize explicit subscription exhaustion; an HTTP 429 alone is insufficient.
use crate::domain::error::{FailureKind, ProxyError};
use axum::http::HeaderMap;
use serde_json::Value;

pub fn classify(status: u16, body: &Value, headers: &HeaderMap, now: u64) -> Option<ProxyError> {
    let event = body.get("type").and_then(Value::as_str).unwrap_or("");
    let error = body
        .get("error")
        .or_else(|| body.pointer("/response/error"))
        .unwrap_or(body);
    let code = error
        .get("code")
        .and_then(Value::as_str)
        .filter(|code| !code.is_empty())
        .or_else(|| error.get("type").and_then(Value::as_str))
        .unwrap_or("");
    let rate_limits = body.get("rate_limits");
    let explicit_limit = event == "codex.rate_limits"
        && rate_limits
            .and_then(|r| r.get("limit_reached"))
            .and_then(Value::as_bool)
            == Some(true)
        && body
            .pointer("/credits/has_credits")
            .and_then(Value::as_bool)
            != Some(true)
        && body.pointer("/credits/unlimited").and_then(Value::as_bool) != Some(true);
    let hard_quota = explicit_limit
        || matches!(
            code,
            "usage_limit_reached"
                | "quota_exhausted"
                | "insufficient_quota"
                | "workspace_budget_exceeded"
        );
    let reported_status = body
        .get("status_code")
        .or_else(|| error.get("status_code"))
        .and_then(Value::as_u64)
        .and_then(|s| u16::try_from(s).ok())
        .unwrap_or(status);
    if hard_quota {
        let reset_at = quota_reset(error, rate_limits, headers, now);
        return Some(ProxyError {
            code: "subscription_quota_exhausted",
            status: 429,
            kind: FailureKind::Quota { reset_at },
            message: "Codex subscription quota exhausted".into(),
            retry_after: Some(reset_at.saturating_sub(now).max(1)),
            upstream_status: Some(status),
            upstream_parameter: None,
        });
    }
    if event == "codex.rate_limits" {
        return None;
    }
    let failed = reported_status >= 400
        || matches!(event, "error" | "response.failed" | "response.incomplete");
    if !failed {
        return None;
    }
    // A normal max-output boundary is translated to stop_reason=max_tokens.
    if event == "response.incomplete"
        && body
            .pointer("/response/incomplete_details/reason")
            .and_then(Value::as_str)
            == Some("max_output_tokens")
    {
        return None;
    }
    let (status, kind, message) = match reported_status {
        401 => (
            401,
            FailureKind::Authentication,
            "Codex rejected the account credential",
        ),
        403 => (
            403,
            FailureKind::Permission,
            "Codex denied access to this model or operation",
        ),
        429 => (
            429,
            FailureKind::RateLimit,
            "Codex is temporarily rate limited; account was not marked exhausted",
        ),
        400 | 404 | 413 | 422 => (
            400,
            FailureKind::InvalidRequest,
            "Codex rejected the request or model; check model access and proxy configuration",
        ),
        _ => (
            502,
            FailureKind::Upstream,
            "Codex reported an upstream error; request was not automatically replayed",
        ),
    };
    let retry_after = headers
        .get("retry-after")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.parse::<u64>().ok());
    let mut error = ProxyError::new(status, kind, message);
    error.retry_after = retry_after;
    error.upstream_status = Some(reported_status);
    error.upstream_parameter = safe_parameter(
        body.get("error")
            .or_else(|| body.pointer("/response/error"))
            .unwrap_or(body)
            .get("param")
            .and_then(Value::as_str),
    );
    if error.kind == FailureKind::InvalidRequest {
        let detail = body
            .get("error")
            .or_else(|| body.pointer("/response/error"))
            .unwrap_or(body)
            .get("message")
            .or_else(|| body.get("detail"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let (diagnostic, safe_message) = validation_diagnostic(code, detail);
        error.code = diagnostic;
        error.message = safe_message.into();
    }
    Some(error)
}

// Read error.message/detail like the reference client, but retain only static diagnoses.
fn validation_diagnostic(code: &str, detail: &str) -> (&'static str, &'static str) {
    match code {
        "model_not_found" | "model_not_supported" | "unsupported_model" => (
            "upstream_model_not_available",
            "Codex cannot serve the selected model for this account",
        ),
        "invalid_function_parameters" | "invalid_json_schema" => (
            "upstream_invalid_tool_schema",
            "Codex rejected the tool or structured-output schema",
        ),
        "unsupported_parameter" | "unknown_parameter" => (
            "upstream_unsupported_parameter",
            "Codex rejected an unsupported request parameter",
        ),
        _ if detail.contains("reasoning.context") => (
            "upstream_invalid_reasoning_context",
            "Codex rejected reasoning.context; check the Responses lane contract",
        ),
        _ if detail.contains("Invalid schema") || detail.contains("invalid schema") => (
            "upstream_invalid_tool_schema",
            "Codex rejected the tool or structured-output schema",
        ),
        _ => (
            "upstream_invalid_request",
            "Codex rejected the request or model; inspect upstream_status and error_code",
        ),
    }
}

fn safe_parameter(raw: Option<&str>) -> Option<&'static str> {
    let raw = raw?;
    [
        "reasoning.context",
        "reasoning.effort",
        "reasoning.summary",
        "parallel_tool_calls",
        "tool_choice",
        "instructions",
        "model",
        "service_tier",
        "text.format",
        "tools",
        "input",
    ]
    .into_iter()
    .find(|&known| {
        raw == known
            || raw
                .strip_prefix(known)
                .is_some_and(|rest| rest.starts_with('.') || rest.starts_with('['))
    })
}
fn quota_reset(error: &Value, limits: Option<&Value>, headers: &HeaderMap, now: u64) -> u64 {
    let mut resets = vec![];
    for key in ["resets_at", "reset_at"] {
        if let Some(value) = error.get(key).and_then(Value::as_u64).filter(|v| *v > now) {
            resets.push(value);
        }
    }
    if let Some(seconds) = error.get("resets_in_seconds").and_then(Value::as_u64) {
        resets.push(now.saturating_add(seconds));
    }
    for (name, header) in [
        ("primary", "x-codex-primary"),
        ("secondary", "x-codex-secondary"),
    ] {
        if let Some(window) = limits.and_then(|limits| limits.get(name))
            && window
                .get("used_percent")
                .and_then(Value::as_f64)
                .is_some_and(|p| p >= 100.0)
            && let Some(reset) = window
                .get("reset_at")
                .or_else(|| window.get("resets_at"))
                .and_then(Value::as_u64)
        {
            resets.push(reset);
        }
        let used = headers
            .get(format!("{header}-used-percent"))
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<f64>().ok());
        if used.is_some_and(|p| p >= 100.0) {
            if let Some(reset) = headers
                .get(format!("{header}-reset-at"))
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
            {
                resets.push(reset);
            } else if let Some(seconds) = headers
                .get(format!("{header}-reset-after-seconds"))
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
            {
                resets.push(now.saturating_add(seconds));
            }
        }
    }
    if resets.is_empty()
        && let Some(seconds) = headers
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
    {
        resets.push(now.saturating_add(seconds));
    }
    resets
        .into_iter()
        .max()
        .unwrap_or_else(|| now.saturating_add(60))
        .max(now.saturating_add(1))
}
