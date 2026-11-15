//! Allowlisted JSON compatibility and bounded, terminal-safe usage presentation.
use crate::{
    accounts::{AccountStatus, Registry},
    domain::account_usage::{
        AccountUsageView, Metric, ResetOutcome, ResetReport, StatusReport, UsageSource,
    },
};
use serde::Serialize;
use std::collections::BTreeMap;

/// Labels are data, never terminal commands; also hide bidi overrides and separators.
pub(super) fn terminal(value: &str) -> String {
    let mut chars = value.chars();
    let mut safe: String = chars
        .by_ref()
        .take(160)
        .map(|c| {
            if c.is_control() || matches!(c, '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}') {
                ' '
            } else {
                c
            }
        })
        .collect();
    if chars.next().is_some() {
        safe.push_str("...");
    }
    safe
}

pub(super) fn status_table(report: &StatusReport, refresh: bool, now: u64) -> String {
    if report.accounts.is_empty() {
        return "No accounts for this selection. Run: llm-proxy accounts add codex --name personal\n"
            .into();
    }
    let mut groups: BTreeMap<&str, Vec<&AccountUsageView>> = BTreeMap::new();
    for account in &report.accounts {
        groups.entry(&account.provider).or_default().push(account);
    }
    let mut text = String::new();
    for (provider, accounts) in groups {
        text.push_str(&format!("Provider: {}\n", terminal(provider)));
        for account in accounts {
            text.push_str(&format!(
                "{} {}  {}\n  Status: {}\n",
                if account.active {
                    "* active"
                } else {
                    "  account"
                },
                terminal(&account.id),
                terminal(&account.name),
                account_status(&account.status, now),
            ));
            let source = match account.observation.source {
                UsageSource::Inference => "inference",
                UsageSource::Live => "live",
                UsageSource::LegacyUnknown => "legacy/unknown",
            };
            let age = match account.observation.observed_at {
                Some(at) if at <= now => format!("{}s ago", now - at),
                _ => "unknown age".into(),
            };
            let freshness = if refresh
                && account.refresh_error.is_none()
                && account.observation.source == UsageSource::Live
            {
                "refreshed"
            } else {
                "cached / possibly stale"
            };
            text.push_str(&format!(
                "  Observation: {freshness}; source={source}; {age}\n"
            ));
            text.push_str(&format!(
                "  Capabilities: live-read={}, banked-reset={}\n",
                account.capabilities.live_read, account.capabilities.reset,
            ));
            match &account.usage.windows {
                Metric::Known(windows) if windows.is_empty() => {
                    text.push_str("  Windows: none reported\n");
                }
                Metric::Known(windows) => {
                    for window in windows.iter().take(64) {
                        let used = window
                            .used_percent
                            .filter(|v| v.is_finite())
                            .map_or_else(|| "unknown".into(), |v| format!("{v:.1}%"));
                        let duration = window.duration_seconds.map_or_else(
                            || "unknown duration".into(),
                            |seconds| format!("{seconds}s duration"),
                        );
                        let reset = window.reset_at.map_or_else(
                            || "unknown reset time".into(),
                            |at| {
                                if at > now {
                                    format!("resets in {}s (Unix {at})", at - now)
                                } else {
                                    format!("reset time passed (Unix {at})")
                                }
                            },
                        );
                        text.push_str(&format!(
                            "  Window: {} [{}; scope={}]: used {used}; {duration}; {reset}\n",
                            terminal(&window.label),
                            terminal(&window.id),
                            terminal(&window.scope),
                        ));
                    }
                    if windows.len() > 64 {
                        text.push_str("  Further windows omitted; use --output-format json.\n");
                    }
                }
                Metric::Unknown => text.push_str("  Windows: unknown\n"),
                Metric::Unsupported => text.push_str("  Windows: unsupported\n"),
            }
            match &account.usage.prepaid_credits {
                Metric::Known(credits) => text.push_str(&format!(
                    "  Prepaid credits: balance={}; has-credits={}; unlimited={}; spend-blocked={}\n",
                    metric(&credits.balance),
                    optional_bool(credits.has_credits),
                    optional_bool(credits.unlimited),
                    optional_bool(credits.spend_blocked),
                )),
                Metric::Unknown => text.push_str("  Prepaid credits: unknown\n"),
                Metric::Unsupported => text.push_str("  Prepaid credits: unsupported\n"),
            }
            match &account.usage.reset_credits {
                Metric::Known(credits) => text.push_str(&format!(
                    "  BANKED resets: {}; earliest expiry={}\n",
                    metric(&credits.available),
                    credits
                        .earliest_expiry
                        .as_deref()
                        .map(terminal)
                        .unwrap_or_else(|| "unknown".into()),
                )),
                Metric::Unknown => text.push_str("  BANKED resets: unknown\n"),
                Metric::Unsupported => text.push_str("  BANKED resets: unsupported\n"),
            }
            text.push_str(&format!(
                "  Ordinary usage allowed: {}\n",
                optional_bool(account.usage.ordinary_usage_allowed),
            ));
            if let Some(error) = account.refresh_error {
                text.push_str(&format!(
                    "  Refresh FAILED: {error}. Cached data above was preserved.\n"
                ));
            }
            if let Some(warning) = account.warning {
                text.push_str(&format!("  Warning: {warning}\n"));
            }
            if let Some(id) = &account.pending_reset_id {
                text.push_str(&format!(
                    "  Unresolved reset request: {}; do not start a new reset.\n",
                    terminal(id),
                ));
            }
        }
    }
    text
}

fn account_status(status: &AccountStatus, now: u64) -> String {
    match status {
        AccountStatus::Available => "available".into(),
        AccountStatus::NeedsLogin => "sign in again".into(),
        AccountStatus::QuotaExhausted { reset_at } if *reset_at <= now => {
            "local cooldown elapsed; provider availability not yet confirmed".into()
        }
        AccountStatus::QuotaExhausted { reset_at } => format!(
            "quota exhausted; scheduled reset in {}s (Unix {reset_at})",
            reset_at.saturating_sub(now),
        ),
    }
}

fn optional_bool(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "yes",
        Some(false) => "no",
        None => "unknown",
    }
}

fn metric(value: &Metric<impl std::fmt::Display>) -> String {
    match value {
        Metric::Known(value) => terminal(&value.to_string()),
        Metric::Unknown => "unknown".into(),
        Metric::Unsupported => "unsupported".into(),
    }
}

pub(super) fn reset_table(report: &ResetReport) -> String {
    let mut text = format!(
        "BANKED quota reset: provider={} account={}\nAvailable banked resets: {}\n\
         This consumes a banked reset, not a credit purchase or a scheduled quota reset.\n",
        terminal(&report.provider),
        terminal(&report.account_id),
        metric(&report.available),
    );
    if report.preview {
        text.push_str("PREVIEW ONLY: no reset requested. Add --yes to confirm redemption.\n");
    }
    if let Some(id) = &report.request_id {
        text.push_str(&format!("Request ID: {}\n", terminal(id)));
    }
    if report.outcome_unknown {
        text.push_str(
            "UNCERTAIN OUTCOME: a banked reset may have been consumed. Do not start a new request.\n",
        );
        if let Some(id) = &report.request_id {
            text.push_str(&format!(
                "Reconcile/retry only the SAME request ID: llm-proxy accounts reset-quota {} {} --yes --retry {}\n",
                terminal(&report.provider),
                terminal(&report.account_id),
                terminal(id),
            ));
        }
        text.push_str("Provider idempotency retention is not guaranteed; there is no automatic mutation retry.\n");
    }
    if let Some(outcome) = &report.outcome {
        let outcome = match outcome {
            ResetOutcome::Reset { windows_reset } => {
                format!("reset (windows_reset={windows_reset})")
            }
            ResetOutcome::NothingToReset => "nothing_to_reset".into(),
            ResetOutcome::NoCredit => "no_credit".into(),
            ResetOutcome::AlreadyRedeemed => "already_redeemed".into(),
        };
        text.push_str(&format!("Confirmed outcome: {outcome}\n"));
    }
    if !report.preview {
        text.push_str(&format!(
            "Reconciled with account state: {}\n",
            if report.reconciled { "yes" } else { "no" },
        ));
    }
    if let Some(error) = report.error {
        text.push_str(&format!("Reset/reconciliation error: {error}\n"));
    }
    if let Some(error) = report.refresh_error {
        text.push_str(&format!("Follow-up refresh FAILED: {error}\n"));
    }
    text
}

// Preserve the original envelope and account fields, not additions to persistence structs.
#[derive(Serialize)]
struct LegacyRegistry<'a> {
    version: u32,
    active: &'a BTreeMap<String, String>,
    accounts: Vec<LegacyAccount<'a>>,
    operation_sequence: u64,
    identity_operations: &'a BTreeMap<String, u64>,
}

#[derive(Serialize)]
struct LegacyAccount<'a> {
    provider: &'a str,
    id: &'a str,
    name: &'a str,
    subject: &'a str,
    workspace: &'a str,
    generation: &'a str,
    status: &'a AccountStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    usage: Option<&'a serde_json::Value>,
}

pub(super) fn legacy_json(registry: &Registry) -> serde_json::Result<String> {
    let accounts = registry
        .accounts
        .iter()
        .map(|account| LegacyAccount {
            provider: &account.provider,
            id: &account.id,
            name: &account.name,
            subject: &account.subject,
            workspace: &account.workspace,
            generation: &account.generation,
            status: &account.status,
            usage: account.usage.as_ref(),
        })
        .collect();
    serde_json::to_string_pretty(&LegacyRegistry {
        version: registry.version,
        active: &registry.active,
        accounts,
        operation_sequence: registry.operation_sequence,
        identity_operations: &registry.identity_operations,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::account_usage::{
        PrepaidCredits, ResetCredits, UsageCapabilities, UsageData, UsageError, UsageObservation,
        UsageWindow,
    };

    fn account() -> AccountUsageView {
        AccountUsageView {
            id: "account".into(),
            name: "personal".into(),
            provider: "provider".into(),
            active: true,
            status: AccountStatus::Available,
            capabilities: UsageCapabilities::default(),
            usage: UsageData::default(),
            observation: UsageObservation::default(),
            refresh_error: None,
            warning: None,
            pending_reset_id: None,
        }
    }

    #[test]
    fn terminal_labels_are_bounded_and_cannot_emit_controls() {
        let text = terminal(&format!("\x1b[2J\r\n\t\u{202e}{}", "a".repeat(500)));
        assert!(!text.chars().any(char::is_control));
        assert!(!text.contains('\u{202e}'));
        assert_eq!(text.chars().count(), 163);
    }

    #[test]
    fn metrics_distinguish_known_zero_unknown_and_unsupported() {
        assert_eq!(metric(&Metric::Known(0)), "0");
        assert_eq!(metric(&Metric::<u64>::Unknown), "unknown");
        assert_eq!(metric(&Metric::<u64>::Unsupported), "unsupported");
    }

    #[test]
    fn status_shows_provider_defined_windows_and_preserves_failed_refresh_data() {
        let mut account = account();
        account.usage = UsageData {
            windows: Metric::Known(vec![UsageWindow {
                id: "monthly".into(),
                label: "Monthly\x1b[2J".into(),
                scope: "workspace".into(),
                duration_seconds: Some(2_592_000),
                used_percent: Some(0.0),
                reset_at: Some(200),
            }]),
            prepaid_credits: Metric::Known(PrepaidCredits {
                balance: Metric::Known("0".into()),
                unlimited: Some(true),
                spend_blocked: Some(false),
                ..PrepaidCredits::default()
            }),
            reset_credits: Metric::Known(ResetCredits {
                available: Metric::Known(0),
                earliest_expiry: Some("2027-01-01".into()),
            }),
            ordinary_usage_allowed: None,
        };
        account.refresh_error = Some(UsageError::Transport);
        let text = status_table(
            &StatusReport {
                accounts: vec![account],
                has_errors: true,
            },
            true,
            100,
        );
        for expected in [
            "Provider: provider",
            "* active",
            "Monthly",
            "used 0.0%",
            "2592000s duration",
            "resets in 100s",
            "balance=0",
            "unlimited=yes",
            "spend-blocked=no",
            "BANKED resets: 0",
            "2027-01-01",
            "possibly stale",
            "unknown age",
            "Refresh FAILED",
            "Cached data above was preserved",
        ] {
            assert!(text.contains(expected), "missing {expected}: {text}");
        }
        assert!(!text.contains('\x1b'));
        assert!(!text.contains("5h"));
    }

    #[test]
    fn missing_windows_are_not_fabricated_and_providers_are_grouped() {
        let first = account();
        let mut other = account();
        other.provider = "another".into();
        other.usage = UsageData::unsupported();
        let text = status_table(
            &StatusReport {
                accounts: vec![first, other],
                has_errors: false,
            },
            false,
            100,
        );
        assert!(text.contains("Provider: another"));
        assert!(text.contains("Provider: provider"));
        assert!(text.contains("Windows: unknown"));
        assert!(text.contains("Windows: unsupported"));
        assert!(!text.contains("0.0%"));
    }

    #[test]
    fn preview_explains_confirmation_without_claiming_a_reset_outcome() {
        let text = reset_table(&ResetReport {
            provider: "codex".into(),
            account_id: "account".into(),
            request_id: None,
            preview: true,
            available: Metric::Known(2),
            outcome: None,
            outcome_unknown: false,
            error: None,
            refresh_error: None,
            reconciled: false,
        });
        assert!(text.contains("PREVIEW ONLY: no reset requested"));
        assert!(text.contains("Available banked resets: 2"));
        assert!(text.contains("not a credit purchase or a scheduled quota reset"));
        assert!(!text.contains("Confirmed outcome"));
    }

    #[test]
    fn confirmed_reset_prints_exact_outcome_request_and_follow_up_failure() {
        let text = reset_table(&ResetReport {
            provider: "codex".into(),
            account_id: "account".into(),
            request_id: Some("request".into()),
            preview: false,
            available: Metric::Unknown,
            outcome: Some(ResetOutcome::Reset { windows_reset: 2 }),
            outcome_unknown: false,
            error: Some(UsageError::AccountChanged),
            refresh_error: Some(UsageError::Transport),
            reconciled: false,
        });
        assert!(text.contains("Confirmed outcome: reset (windows_reset=2)"));
        assert!(text.contains("Request ID: request"));
        assert!(text.contains("Reset/reconciliation error"));
        assert!(text.contains("Follow-up refresh FAILED"));
        assert!(!text.contains("UNCERTAIN OUTCOME"));
    }

    #[test]
    fn uncertain_reset_names_the_same_request_and_never_claims_no_consumption() {
        let text = reset_table(&ResetReport {
            provider: "codex".into(),
            account_id: "account".into(),
            request_id: Some("request".into()),
            preview: false,
            available: Metric::Unknown,
            outcome: None,
            outcome_unknown: true,
            error: Some(UsageError::Transport),
            refresh_error: None,
            reconciled: false,
        });
        assert!(text.contains("UNCERTAIN OUTCOME"));
        assert!(text.contains("may have been consumed"));
        assert!(text.contains("--yes --retry request"));
        assert!(text.contains("idempotency retention is not guaranteed"));
        assert!(!text.contains("no reset requested"));
    }
}
