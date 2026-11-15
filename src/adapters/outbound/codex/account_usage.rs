//! Bounded Codex account telemetry and explicitly requested reset-credit redemption.
//!
//! Wire fields follow openai/codex@ee344ca9458351a6ea102795fa73110ad3ede5dc,
//! backend-client/src/{types.rs,client/rate_limit_resets.rs}. No spending/Reserve opt-in.
use crate::{
    accounts::{Credential, Store},
    authentication::{AuthClient, RefreshError},
    domain::account::AccountIdentity,
    domain::account_usage::{
        Metric, PrepaidCredits, ResetCredits, ResetDispatchError, ResetOutcome, UsageCapabilities,
        UsageData, UsageError, UsageRead, UsageWindow,
    },
    ports::account_usage::AccountUsageProvider,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use futures_util::{StreamExt, future::BoxFuture};
use reqwest::{Method, Request, header};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

const MAX_BODY: usize = 256 * 1024;
const MAX_ITEMS: usize = 128;
const MAX_STRING: usize = 4096;

pub struct CodexAccountUsage {
    live: Option<Live>,
}
struct Live {
    store: Store,
    endpoint: String,
    auth: AuthClient,
    client: reqwest::Client,
}
impl CodexAccountUsage {
    /// Decode persisted snapshots without constructing an HTTP or authentication client.
    pub fn cached() -> Self {
        Self { live: None }
    }

    pub fn with_auth(store: Store, endpoint: String, auth: AuthClient) -> Result<Self, UsageError> {
        crate::config::validate_endpoint(&endpoint).map_err(|_| UsageError::Protocol)?;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|_| UsageError::Transport)?;
        Ok(Self {
            live: Some(Live {
                store,
                endpoint: endpoint.trim_end_matches('/').to_owned(),
                auth,
                client,
            }),
        })
    }
}
impl AccountUsageProvider for CodexAccountUsage {
    fn name(&self) -> &'static str {
        "codex"
    }
    fn capabilities(&self) -> UsageCapabilities {
        UsageCapabilities {
            live_read: true,
            reset: true,
        }
    }
    fn decode_cached(&self, snapshot: &Value) -> Result<UsageData, UsageError> {
        decode(snapshot).map(|(_, data)| data)
    }
    fn read(&self, account: AccountIdentity) -> BoxFuture<'_, Result<UsageRead, UsageError>> {
        Box::pin(async move {
            let live = self.live.as_ref().ok_or(UsageError::Unsupported)?;
            let credential = live.credential(&account).await?;
            let request = live.request(Method::GET, "usage", &credential)?;
            let value = live.execute(request).await?;
            verify_identity(&value, &credential)?;
            let (mut snapshot, mut data) = decode(&value)?;
            let count = snapshot
                .rate_limit_reset_credits
                .as_ref()
                .and_then(|credits| credits.available_count);
            let mut warning = None;
            if count != Some(0) {
                let details = async {
                    let request =
                        live.request(Method::GET, "rate-limit-reset-credits", &credential)?;
                    let value = live.execute(request).await?;
                    verify_identity(&value, &credential)?;
                    decode_details(&value)
                }
                .await;
                match details {
                    Ok(credits) => {
                        snapshot.rate_limit_reset_credits = Some(credits.clone());
                        data.reset_credits = Metric::Known(credits.normalized());
                    }
                    Err(error) => warning = Some(error),
                }
            }
            Ok(UsageRead {
                snapshot: serde_json::to_value(snapshot).map_err(|_| UsageError::Protocol)?,
                data,
                warning,
            })
        })
    }
    fn reset(
        &self,
        account: AccountIdentity,
        request_id: String,
    ) -> BoxFuture<'_, Result<ResetOutcome, ResetDispatchError>> {
        Box::pin(async move {
            let prepare = async {
                let live = self.live.as_ref().ok_or(UsageError::Unsupported)?;
                uuid::Uuid::parse_str(&request_id).map_err(|_| UsageError::Protocol)?;
                let credential = live.credential(&account).await?;
                let mut request = live.request(
                    Method::POST,
                    "rate-limit-reset-credits/consume",
                    &credential,
                )?;
                request.headers_mut().insert(
                    header::CONTENT_TYPE,
                    header::HeaderValue::from_static("application/json"),
                );
                *request.body_mut() = Some(
                    serde_json::to_vec(&serde_json::json!({"redeem_request_id": request_id}))
                        .map_err(|_| UsageError::Protocol)?
                        .into(),
                );
                Ok::<_, UsageError>((live, request, credential))
            }
            .await;
            let (live, request, credential) = prepare.map_err(|error| ResetDispatchError {
                error,
                dispatched: false,
            })?;
            // Once execute is entered, even a transport/401 error is an uncertain dispatch.
            // The durable caller owns reconciliation; neither reqwest nor this code retries.
            let result = async {
                let value = live.execute(request).await?;
                verify_identity(&value, &credential)?;
                decode_outcome(&value)
            }
            .await;
            result.map_err(|error| ResetDispatchError {
                error,
                dispatched: true,
            })
        })
    }
    fn shutdown(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {
            if let Some(live) = &self.live {
                live.auth.shutdown().await;
            }
        })
    }
}
impl Live {
    async fn credential(&self, account: &AccountIdentity) -> Result<Credential, UsageError> {
        if account.provider != "codex" {
            return Err(UsageError::ProviderMismatch);
        }
        let credential = self
            .auth
            .refresh_for_generation(&self.store, &account.id, &account.generation, None)
            .await
            .map_err(|error| match error {
                RefreshError::AccountChanged => UsageError::AccountChanged,
                RefreshError::Storage(_) => UsageError::Storage,
                RefreshError::Revoked => UsageError::Authentication,
                RefreshError::Other(_) => UsageError::Transport,
            })?;
        if credential.generation != account.generation
            || credential.subject != account.subject
            || credential.account_id != account.workspace
        {
            return Err(UsageError::AccountChanged);
        }
        // Refresh publication and external account mutations can race. Check generation
        // again immediately before preparing the provider request, without a network lock.
        let store = self.store.clone();
        let id = account.id.clone();
        let generation = account.generation.clone();
        let current =
            tokio::task::spawn_blocking(move || store.lease_for_generation(&id, &generation))
                .await
                .map_err(|_| UsageError::Storage)?
                .map_err(|_| UsageError::Storage)?;
        if current.is_none() {
            return Err(UsageError::AccountChanged);
        }
        Ok(credential)
    }
    fn request(
        &self,
        method: Method,
        operation: &str,
        credential: &Credential,
    ) -> Result<Request, UsageError> {
        if credential.access_token.is_empty()
            || credential.access_token.len() > 32 * 1024
            || credential.account_id.is_empty()
            || credential.account_id.len() > 256
        {
            return Err(UsageError::Authentication);
        }
        let mut authorization =
            header::HeaderValue::from_str(&format!("Bearer {}", credential.access_token))
                .map_err(|_| UsageError::Authentication)?;
        authorization.set_sensitive(true);
        let workspace = header::HeaderValue::from_str(&credential.account_id)
            .map_err(|_| UsageError::Authentication)?;
        self.client
            .request(method, format!("{}/wham/{operation}", self.endpoint))
            .header(header::AUTHORIZATION, authorization)
            .header("ChatGPT-Account-Id", workspace)
            .header(header::ACCEPT, "application/json")
            .build()
            .map_err(|_| UsageError::Protocol)
    }
    async fn execute(&self, request: Request) -> Result<Value, UsageError> {
        let response = self
            .client
            .execute(request)
            .await
            .map_err(|_| UsageError::Transport)?;
        if !response.status().is_success() {
            return Err(if response.status() == reqwest::StatusCode::UNAUTHORIZED {
                UsageError::Authentication
            } else {
                UsageError::Upstream
            });
        }
        if response
            .content_length()
            .is_some_and(|len| len > MAX_BODY as u64)
        {
            return Err(UsageError::Protocol);
        }
        let mut stream = response.bytes_stream();
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| UsageError::Transport)?;
            if chunk.len() > MAX_BODY.saturating_sub(bytes.len()) {
                return Err(UsageError::Protocol);
            }
            bytes.extend_from_slice(&chunk);
        }
        let value = serde_json::from_slice(&bytes).map_err(|_| UsageError::Protocol)?;
        validate_value(&value)?;
        Ok(value)
    }
}

// Typed reserialization is the cache allowlist: identity, upsell, error messages,
// reset-credit IDs/titles/descriptions and all unknown JSON are deliberately discarded.
#[derive(Default, Deserialize, Serialize)]
struct Snapshot {
    // Keep a recognized null field when all metrics are unknown so the cache round-trips.
    rate_limit: Option<Limits>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rate_limits: Option<Limits>,
    #[serde(skip_serializing_if = "Option::is_none")]
    additional_rate_limits: Option<Vec<AdditionalLimits>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    credits: Option<Credits>,
    #[serde(skip_serializing_if = "Option::is_none")]
    spend_control: Option<SpendControl>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rate_limit_reset_credits: Option<ResetSummary>,
}
#[derive(Deserialize, Serialize)]
struct AdditionalLimits {
    #[serde(skip_serializing_if = "Option::is_none")]
    rate_limit: Option<Limits>,
}
#[derive(Deserialize, Serialize)]
struct Limits {
    #[serde(skip_serializing_if = "Option::is_none")]
    allowed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit_reached: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    primary_window: Option<Window>,
    #[serde(skip_serializing_if = "Option::is_none")]
    secondary_window: Option<Window>,
    #[serde(skip_serializing_if = "Option::is_none")]
    primary: Option<Window>,
    #[serde(skip_serializing_if = "Option::is_none")]
    secondary: Option<Window>,
}
#[derive(Deserialize, Serialize)]
struct Window {
    #[serde(skip_serializing_if = "Option::is_none")]
    used_percent: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit_window_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit_window_minutes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reset_after_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reset_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resets_at: Option<u64>,
}
#[derive(Deserialize, Serialize)]
struct Credits {
    #[serde(skip_serializing_if = "Option::is_none")]
    has_credits: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    unlimited: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    balance: Option<String>,
}
#[derive(Deserialize, Serialize)]
struct SpendControl {
    #[serde(skip_serializing_if = "Option::is_none")]
    reached: Option<bool>,
}
#[derive(Clone, Deserialize, Serialize)]
struct ResetSummary {
    #[serde(skip_serializing_if = "Option::is_none")]
    available_count: Option<u64>,
    // Adapter cache extension; only validated RFC3339 values enter this field.
    #[serde(skip_serializing_if = "Option::is_none")]
    earliest_expiry: Option<String>,
}
impl ResetSummary {
    fn normalized(&self) -> ResetCredits {
        ResetCredits {
            available: self.available_count.map(Metric::Known).unwrap_or_default(),
            earliest_expiry: self.earliest_expiry.clone(),
        }
    }
}
fn decode(value: &Value) -> Result<(Snapshot, UsageData), UsageError> {
    validate_value(value)?;
    if !value.is_object()
        || ![
            "rate_limit",
            "rate_limits",
            "additional_rate_limits",
            "credits",
            "spend_control",
            "rate_limit_reset_credits",
            "plan_type",
        ]
        .iter()
        .any(|key| value.get(key).is_some())
    {
        return Err(UsageError::Protocol);
    }
    let snapshot: Snapshot =
        serde_json::from_value(value.clone()).map_err(|_| UsageError::Protocol)?;
    let limits = snapshot
        .rate_limit
        .as_ref()
        .or(snapshot.rate_limits.as_ref());
    let mut windows = Vec::new();
    if let Some(limits) = limits {
        append_windows(&mut windows, limits, "codex", "Codex")?;
    }
    if let Some(additional) = &snapshot.additional_rate_limits {
        if additional.len() > 32 {
            return Err(UsageError::Protocol);
        }
        for (index, entry) in additional.iter().enumerate() {
            if let Some(limits) = &entry.rate_limit {
                append_windows(
                    &mut windows,
                    limits,
                    &format!("additional_{index}"),
                    &format!("Additional {}", index + 1),
                )?;
            }
        }
    }
    let prepaid_credits = match (&snapshot.credits, &snapshot.spend_control) {
        (None, None) => Metric::Unknown,
        (credits, spend) => {
            let balance = credits
                .as_ref()
                .and_then(|credits| credits.balance.as_ref());
            if balance.is_some_and(|balance| !decimal(balance)) {
                return Err(UsageError::Protocol);
            }
            Metric::Known(PrepaidCredits {
                balance: balance.cloned().map(Metric::Known).unwrap_or_default(),
                has_credits: credits.as_ref().and_then(|credits| credits.has_credits),
                unlimited: credits.as_ref().and_then(|credits| credits.unlimited),
                spend_blocked: spend.as_ref().and_then(|spend| spend.reached),
            })
        }
    };
    if let Some(expiry) = snapshot
        .rate_limit_reset_credits
        .as_ref()
        .and_then(|credits| credits.earliest_expiry.as_ref())
    {
        rfc3339_timestamp(expiry)?;
    }
    let data = UsageData {
        windows: if limits.is_some() || snapshot.additional_rate_limits.is_some() {
            Metric::Known(windows)
        } else {
            Metric::Unknown
        },
        prepaid_credits,
        reset_credits: snapshot
            .rate_limit_reset_credits
            .as_ref()
            .map(|credits| Metric::Known(credits.normalized()))
            .unwrap_or_default(),
        // Never infer admission/exhaustion from percentages or credit balance.
        ordinary_usage_allowed: limits.and_then(|limits| limits.allowed),
    };
    Ok((snapshot, data))
}
fn append_windows(
    windows: &mut Vec<UsageWindow>,
    limits: &Limits,
    scope: &str,
    label: &str,
) -> Result<(), UsageError> {
    for (name, window) in [
        (
            "primary",
            limits.primary_window.as_ref().or(limits.primary.as_ref()),
        ),
        (
            "secondary",
            limits
                .secondary_window
                .as_ref()
                .or(limits.secondary.as_ref()),
        ),
    ] {
        if let Some(window) = window {
            if window
                .used_percent
                .is_some_and(|percent| !percent.is_finite() || percent < 0.0)
            {
                return Err(UsageError::Protocol);
            }
            let minutes = window
                .limit_window_minutes
                .map(|minutes| minutes.checked_mul(60).ok_or(UsageError::Protocol))
                .transpose()?;
            windows.push(UsageWindow {
                id: format!("{scope}.{name}"),
                label: format!("{label} {name}"),
                scope: scope.to_owned(),
                duration_seconds: window.limit_window_seconds.or(minutes),
                used_percent: window.used_percent,
                // Relative reset seconds cannot safely become a timestamp on cached reads.
                reset_at: window.reset_at.or(window.resets_at),
            });
        }
    }
    Ok(())
}
fn decimal(value: &str) -> bool {
    if value.is_empty() || value.len() > 128 {
        return false;
    }
    let unsigned = value.strip_prefix('-').unwrap_or(value);
    let mut parts = unsigned.split('.');
    let whole = parts.next().unwrap_or_default();
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    digits(whole) && parts.next().is_none_or(digits) && parts.next().is_none()
}
fn decode_details(value: &Value) -> Result<ResetSummary, UsageError> {
    #[derive(Deserialize)]
    struct Details {
        available_count: u64,
        credits: Vec<Credit>,
    }
    #[derive(Deserialize)]
    struct Credit {
        status: String,
        expires_at: Option<String>,
    }
    let details: Details =
        serde_json::from_value(value.clone()).map_err(|_| UsageError::Protocol)?;
    let mut earliest: Option<(ExpiryTimestamp, String)> = None;
    for credit in details.credits {
        if let Some(expiry) = credit.expires_at {
            let timestamp = rfc3339_timestamp(&expiry)?;
            if credit.status == "available"
                && earliest
                    .as_ref()
                    .is_none_or(|(previous, _)| timestamp < *previous)
            {
                earliest = Some((timestamp, expiry));
            }
        }
    }
    Ok(ResetSummary {
        available_count: Some(details.available_count),
        earliest_expiry: earliest.map(|(_, expiry)| expiry),
    })
}
fn decode_outcome(value: &Value) -> Result<ResetOutcome, UsageError> {
    let windows_reset = match value.get("windows_reset") {
        None => 0,
        Some(value) => value.as_u64().ok_or(UsageError::Protocol)?,
    };
    match value.get("code").and_then(Value::as_str) {
        Some("reset") => Ok(ResetOutcome::Reset { windows_reset }),
        Some("nothing_to_reset") => Ok(ResetOutcome::NothingToReset),
        Some("no_credit") => Ok(ResetOutcome::NoCredit),
        Some("already_redeemed") => Ok(ResetOutcome::AlreadyRedeemed),
        _ => Err(UsageError::Protocol),
    }
}
fn verify_identity(value: &Value, credential: &Credential) -> Result<(), UsageError> {
    if let Some(account_id) = value.get("account_id").filter(|value| !value.is_null())
        && account_id.as_str() != Some(credential.account_id.as_str())
    {
        return Err(UsageError::Protocol);
    }
    // OAuth subject and ChatGPT user IDs can be different namespaces. Only compare
    // user_id when the trusted OAuth token explicitly equates that namespace to subject.
    let equivalent_user = || {
        let mut parts = credential.access_token.split('.');
        parts.next()?;
        let payload = parts.next()?;
        if payload.len() > 32 * 1024 || parts.next().is_none() || parts.next().is_some() {
            return None;
        }
        let bytes = URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
        let claims: Value = serde_json::from_slice(&bytes).ok()?;
        let user = claims
            .pointer("/https:~1~1api.openai.com~1auth/chatgpt_user_id")?
            .as_str()?;
        (user == credential.subject).then_some(())
    };
    if equivalent_user().is_some()
        && let Some(user_id) = value.get("user_id").filter(|value| !value.is_null())
        && user_id.as_str() != Some(credential.subject.as_str())
    {
        return Err(UsageError::Protocol);
    }
    Ok(())
}
fn validate_value(value: &Value) -> Result<(), UsageError> {
    fn visit(
        value: &Value,
        depth: usize,
        nodes: &mut usize,
        bytes: &mut usize,
    ) -> Result<(), UsageError> {
        *nodes += 1;
        if depth > 32 || *nodes > 4096 || *bytes > MAX_BODY {
            return Err(UsageError::Protocol);
        }
        match value {
            Value::String(value) => {
                if value.len() > MAX_STRING {
                    return Err(UsageError::Protocol);
                }
                *bytes += value.len();
            }
            Value::Array(items) => {
                if items.len() > MAX_ITEMS {
                    return Err(UsageError::Protocol);
                }
                for item in items {
                    visit(item, depth + 1, nodes, bytes)?;
                }
            }
            Value::Object(fields) => {
                if fields.len() > MAX_ITEMS {
                    return Err(UsageError::Protocol);
                }
                for (key, value) in fields {
                    if key.len() > MAX_STRING {
                        return Err(UsageError::Protocol);
                    }
                    *bytes += key.len();
                    visit(value, depth + 1, nodes, bytes)?;
                }
            }
            _ => *bytes += 8,
        }
        if *bytes > MAX_BODY {
            return Err(UsageError::Protocol);
        }
        Ok(())
    }
    visit(value, 0, &mut 0, &mut 0)
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct ExpiryTimestamp {
    seconds: i64,
    fraction: String,
}

// Strict, bounded RFC3339 validation and UTC ordering without an additional dependency.
fn rfc3339_timestamp(value: &str) -> Result<ExpiryTimestamp, UsageError> {
    let invalid = || UsageError::Protocol;
    let bytes = value.as_bytes();
    if !(20..=40).contains(&bytes.len()) || !value.is_ascii() {
        return Err(invalid());
    }
    for (index, separator) in [(4, b'-'), (7, b'-'), (10, b'T'), (13, b':'), (16, b':')] {
        if bytes[index].to_ascii_uppercase() != separator {
            return Err(invalid());
        }
    }
    let number = |start: usize, end: usize| -> Result<i64, UsageError> {
        let part = value.get(start..end).ok_or_else(invalid)?;
        if !part.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(invalid());
        }
        part.parse().map_err(|_| invalid())
    };
    let year = number(0, 4)?;
    let month = number(5, 7)?;
    let day = number(8, 10)?;
    let hour = number(11, 13)?;
    let minute = number(14, 16)?;
    let second = number(17, 19)?;
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        2 => {
            if leap {
                29
            } else {
                28
            }
        }
        4 | 6 | 9 | 11 => 30,
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        _ => return Err(invalid()),
    };
    if year == 0 || !(1..=days).contains(&day) || hour > 23 || minute > 59 || second > 59 {
        return Err(invalid());
    }
    let mut zone = 19;
    let mut fraction = String::new();
    if bytes.get(zone) == Some(&b'.') {
        zone += 1;
        let start = zone;
        while bytes.get(zone).is_some_and(u8::is_ascii_digit) {
            zone += 1;
        }
        if zone == start {
            return Err(invalid());
        }
        fraction = value[start..zone].trim_end_matches('0').to_owned();
    }
    let offset = match bytes.get(zone) {
        Some(b'Z' | b'z') if zone + 1 == bytes.len() => 0,
        Some(sign @ (b'+' | b'-')) if zone + 6 == bytes.len() && bytes[zone + 3] == b':' => {
            let hours = number(zone + 1, zone + 3)?;
            let minutes = number(zone + 4, zone + 6)?;
            if hours > 23 || minutes > 59 {
                return Err(invalid());
            }
            (hours * 3600 + minutes * 60) * if *sign == b'+' { 1 } else { -1 }
        }
        _ => return Err(invalid()),
    };
    // Gregorian civil date to Unix days (400-year eras).
    let year = year - i64::from(month <= 2);
    let era = year / 400;
    let year_of_era = year - era * 400;
    let shifted_month = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    Ok(ExpiryTimestamp {
        seconds: (era * 146097 + day_of_era - 719468) * 86400 + hour * 3600 + minute * 60 + second
            - offset,
        fraction,
    })
}
