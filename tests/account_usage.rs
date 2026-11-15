use futures_util::future::BoxFuture;
use llm_proxy::{
    accounts::{Account, AccountStatus, Credential, Store, now},
    application::account_usage::AccountUsageService,
    domain::account_usage::*,
    ports::account_usage::AccountUsageProvider,
};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

fn login(store: &Store, name: &str) -> Account {
    store
        .login(
            name,
            Credential {
                access_token: "synthetic-access".into(),
                refresh_token: "synthetic-refresh".into(),
                id_token: None,
                expires_at: now() + 3600,
                subject: name.into(),
                account_id: "synthetic-workspace".into(),
                generation: String::new(),
            },
        )
        .unwrap()
}

fn reading(count: Metric<u64>, allowed: Option<bool>) -> UsageRead {
    let data = UsageData {
        windows: Metric::Known(vec![UsageWindow {
            id: "custom-window".into(),
            label: "Custom duration".into(),
            scope: "account".into(),
            duration_seconds: Some(123),
            used_percent: Some(100.0),
            reset_at: Some(now() + 600),
        }]),
        reset_credits: Metric::Known(ResetCredits {
            available: count,
            earliest_expiry: None,
        }),
        ordinary_usage_allowed: allowed,
        ..UsageData::default()
    };
    UsageRead {
        snapshot: serde_json::to_value(&data).unwrap(),
        data,
        warning: None,
    }
}

#[tokio::test]
async fn failed_retry_preparation_does_not_erase_an_earlier_uncertain_dispatch() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let account = login(&store, "personal");
    let fake = Arc::new(Fake::new(
        vec![Ok(reading(Metric::Known(2), Some(false)))],
        vec![
            Err(ResetDispatchError {
                error: UsageError::Transport,
                dispatched: true,
            }),
            Err(ResetDispatchError {
                error: UsageError::Authentication,
                dispatched: false,
            }),
        ],
    ));
    let service = AccountUsageService::new(store.clone(), vec![fake.clone()]);
    let first = service
        .reset("codex", &account.id, true, None)
        .await
        .unwrap();
    let key = first.request_id.unwrap();
    let retry = service
        .reset("codex", &account.id, true, Some(&key))
        .await
        .unwrap();
    assert!(retry.outcome_unknown);
    assert_eq!(
        store.reset_attempt(&account.id).unwrap().unwrap().state,
        ResetAttemptState::Unknown
    );
    let new_attempt = service
        .reset("codex", &account.id, true, None)
        .await
        .unwrap();
    assert_eq!(new_attempt.error, Some(UsageError::PendingReset));
    assert_eq!(*fake.keys.lock().unwrap(), vec![key.clone(), key]);
}

#[tokio::test]
async fn manual_refresh_recovers_external_quota_reset_without_changing_active_account() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let a = login(&store, "a");
    let b = login(&store, "b");
    store
        .mark_quota_at(&a.id, &a.generation, now() + 3600, now())
        .unwrap();
    let revision = store.list().unwrap().accounts[0].status_revision;
    let fake = Arc::new(Fake::new(
        vec![
            // Accounts refresh concurrently; both synthetic replies confirm availability.
            Ok(reading(Metric::Known(0), Some(true))),
            Ok(reading(Metric::Known(0), Some(true))),
        ],
        vec![],
    ));
    let service = AccountUsageService::new(store.clone(), vec![fake.clone()]);
    let report = service.status(Some("codex"), true).await.unwrap();
    assert!(!report.has_errors);
    assert_eq!(report.accounts[0].status, AccountStatus::Available);
    let registry = store.list().unwrap();
    assert_eq!(registry.accounts[0].status, AccountStatus::Available);
    assert_eq!(registry.accounts[0].status_revision, revision + 1);
    assert_eq!(registry.active["codex"], b.id);
    assert!(!report.accounts[0].active);
    assert!(fake.keys.lock().unwrap().is_empty());
    // The synthetic window remains at 100%; authoritative allowed, not percentage, recovers it.
    assert!(
        matches!(&report.accounts[0].usage.windows, Metric::Known(windows) if windows[0].used_percent == Some(100.0))
    );
}

#[tokio::test]
async fn manual_refresh_does_not_recover_quota_from_false_missing_or_failed_confirmation() {
    for result in [
        Ok(reading(Metric::Known(2), Some(false))),
        Ok(reading(Metric::Known(2), None)),
        Err(UsageError::Transport),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let a = login(&store, "personal");
        store
            .mark_quota_at(&a.id, &a.generation, now() + 3600, now())
            .unwrap();
        let expected = store.list().unwrap().accounts[0].status.clone();
        let service = AccountUsageService::new(
            store.clone(),
            vec![Arc::new(Fake::new(vec![result], vec![]))],
        );
        let report = service.status(None, true).await.unwrap();
        assert_eq!(report.accounts[0].status, expected);
        assert_eq!(store.list().unwrap().accounts[0].status, expected);
    }
}

#[tokio::test]
async fn manual_refresh_never_clears_needs_login_or_overwrites_newer_quota() {
    for needs_login in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let a = login(&store, "personal");
        let mut fake = Fake::new(vec![Ok(reading(Metric::Known(2), Some(true)))], vec![]);
        if needs_login {
            store.mark_needs_login(&a.id, &a.generation).unwrap();
        } else {
            store
                .mark_quota_at(&a.id, &a.generation, now() + 600, now())
                .unwrap();
            fake.newer_quota_during_read = Some((store.clone(), a));
        }
        let service = AccountUsageService::new(store.clone(), vec![Arc::new(fake)]);
        let report = service.status(None, true).await.unwrap();
        let current = store.list().unwrap().accounts.remove(0);
        assert_ne!(current.status, AccountStatus::Available);
        assert_eq!(report.accounts[0].status, current.status);
    }
}

#[tokio::test]
async fn offline_status_never_recovers_quota_from_cached_allowed_true() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let a = login(&store, "personal");
    store
        .record_usage(
            &a.id,
            &a.generation,
            reading(Metric::Known(1), Some(true)).snapshot,
        )
        .unwrap();
    store
        .mark_quota_at(&a.id, &a.generation, now() + 3600, now())
        .unwrap();
    let service =
        AccountUsageService::new(store.clone(), vec![Arc::new(Fake::new(vec![], vec![]))]);
    let report = service.status(None, false).await.unwrap();
    assert!(matches!(
        report.accounts[0].status,
        AccountStatus::QuotaExhausted { .. }
    ));
    assert!(matches!(
        store.list().unwrap().accounts[0].status,
        AccountStatus::QuotaExhausted { .. }
    ));
}

#[test]
fn manual_quota_recovery_rejects_replaced_or_removed_generations() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let a = login(&store, "personal");
    store
        .mark_quota_at(&a.id, &a.generation, now() + 3600, now())
        .unwrap();
    let observed = store.list().unwrap().accounts.remove(0);
    let newer = login(&store, "personal");
    store
        .mark_quota_at(&newer.id, &newer.generation, now() + 7200, now())
        .unwrap();
    assert!(
        store
            .reconcile_refreshed_quota(&observed)
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        store.list().unwrap().accounts[0].status,
        AccountStatus::QuotaExhausted { .. }
    ));
    store.remove(&newer.id).unwrap();
    assert!(
        store
            .reconcile_refreshed_quota(&observed)
            .unwrap()
            .is_none()
    );
    assert!(store.list().unwrap().accounts.is_empty());
}

struct Fake {
    reads: Mutex<VecDeque<Result<UsageRead, UsageError>>>,
    outcomes: Mutex<VecDeque<Result<ResetOutcome, ResetDispatchError>>>,
    keys: Mutex<Vec<String>>,
    newer_quota_after_reset: Option<(Store, Account)>,
    newer_quota_during_read: Option<(Store, Account)>,
    remove_during_read: Option<(Store, Account)>,
}
impl Fake {
    fn new(
        reads: Vec<Result<UsageRead, UsageError>>,
        outcomes: Vec<Result<ResetOutcome, ResetDispatchError>>,
    ) -> Self {
        Self {
            reads: Mutex::new(reads.into()),
            outcomes: Mutex::new(outcomes.into()),
            keys: Mutex::new(vec![]),
            newer_quota_after_reset: None,
            newer_quota_during_read: None,
            remove_during_read: None,
        }
    }
}
impl AccountUsageProvider for Fake {
    fn name(&self) -> &'static str {
        "codex"
    }
    fn capabilities(&self) -> UsageCapabilities {
        UsageCapabilities {
            live_read: true,
            reset: true,
        }
    }
    fn decode_cached(&self, snapshot: &serde_json::Value) -> Result<UsageData, UsageError> {
        serde_json::from_value(snapshot.clone()).map_err(|_| UsageError::Protocol)
    }
    fn read(&self, _: Account) -> BoxFuture<'_, Result<UsageRead, UsageError>> {
        if let Some((store, account)) = &self.remove_during_read {
            store.remove(&account.id).unwrap();
        }
        if let Some((store, account)) = &self.newer_quota_during_read {
            store
                .mark_quota_at(&account.id, &account.generation, now() + 1200, now())
                .unwrap();
        }
        if !self.keys.lock().unwrap().is_empty()
            && let Some((store, account)) = &self.newer_quota_after_reset
        {
            store
                .mark_quota_at(&account.id, &account.generation, now() + 900, now())
                .unwrap();
        }
        let read = self
            .reads
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Err(UsageError::Protocol));
        Box::pin(async move { read })
    }
    fn reset(
        &self,
        _: Account,
        request_id: String,
    ) -> BoxFuture<'_, Result<ResetOutcome, ResetDispatchError>> {
        self.keys.lock().unwrap().push(request_id);
        let result = self.outcomes.lock().unwrap().pop_front().unwrap();
        Box::pin(async move { result })
    }
}

#[tokio::test]
async fn offline_status_filters_exact_provider_and_keeps_unsupported_distinct() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let a = login(&store, "personal");
    let mut credential = store.lease(&a.id).unwrap().credential;
    credential.subject = "future-member".into();
    store.login_for("future", "other", credential).unwrap();
    let service = AccountUsageService::new(store, vec![]);
    let all = service.status(None, false).await.unwrap();
    assert_eq!(all.accounts.len(), 2);
    assert_eq!(all.accounts[1].usage.windows, Metric::Unsupported);
    let filtered = service.status(Some("future"), false).await.unwrap();
    assert_eq!(filtered.accounts.len(), 1);
    assert_eq!(filtered.accounts[0].provider, "future");
    assert!(matches!(
        service.status(Some("typo"), false).await,
        Err(UsageError::UnknownProvider)
    ));
}

#[tokio::test]
async fn refresh_preserves_cached_values_and_reports_failure_without_changing_selection() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let a = login(&store, "personal");
    let old = reading(Metric::Known(0), Some(false));
    store
        .record_usage(&a.id, &a.generation, old.snapshot)
        .unwrap();
    let before = store.list().unwrap().active;
    let fake = Arc::new(Fake::new(vec![Err(UsageError::Transport)], vec![]));
    let service = AccountUsageService::new(store.clone(), vec![fake]);
    let report = service.status(Some("codex"), true).await.unwrap();
    assert!(report.has_errors);
    assert_eq!(
        report.accounts[0].refresh_error,
        Some(UsageError::Transport)
    );
    assert_eq!(report.accounts[0].usage, old.data);
    assert_eq!(store.list().unwrap().active, before);
    assert_eq!(
        store.list().unwrap().accounts[0].status,
        AccountStatus::Available
    );
}

#[tokio::test]
async fn reset_preview_reads_offer_but_never_journals_or_dispatches_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let a = login(&store, "personal");
    let fake = Arc::new(Fake::new(vec![Ok(reading(Metric::Known(2), None))], vec![]));
    let service = AccountUsageService::new(store.clone(), vec![fake.clone()]);
    let report = service.reset("codex", &a.id, false, None).await.unwrap();
    assert!(report.preview);
    assert_eq!(report.available, Metric::Known(2));
    assert!(fake.keys.lock().unwrap().is_empty());
    assert!(store.reset_attempt(&a.id).unwrap().is_none());
}

#[tokio::test]
async fn zero_and_unknown_reset_counts_both_block_new_redemptions_without_fabrication() {
    for (count, expected) in [
        (Metric::Known(0), UsageError::NoResetCredits),
        (Metric::Unknown, UsageError::UnknownResetCredits),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let a = login(&store, "personal");
        let fake = Arc::new(Fake::new(vec![Ok(reading(count, None))], vec![]));
        let service = AccountUsageService::new(store.clone(), vec![fake.clone()]);
        assert!(
            matches!(service.reset("codex", &a.id, true, None).await, Err(error) if error == expected)
        );
        assert!(fake.keys.lock().unwrap().is_empty());
        assert!(store.reset_attempt(&a.id).unwrap().is_none());
    }
}

#[tokio::test]
async fn uncertain_redemption_is_not_repeated_without_explicit_same_key_retry() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let a = login(&store, "personal");
    let fake = Arc::new(Fake::new(
        vec![
            Ok(reading(Metric::Known(2), Some(false))),
            Ok(reading(Metric::Known(1), Some(true))),
        ],
        vec![
            Err(ResetDispatchError {
                error: UsageError::Transport,
                dispatched: true,
            }),
            Ok(ResetOutcome::Reset { windows_reset: 2 }),
        ],
    ));
    let service = AccountUsageService::new(store.clone(), vec![fake.clone()]);
    let first = service.reset("codex", &a.id, true, None).await.unwrap();
    assert!(first.outcome_unknown);
    let request_id = first.request_id.unwrap();
    assert_eq!(
        store.reset_attempt(&a.id).unwrap().unwrap().state,
        ResetAttemptState::Unknown
    );
    let blocked = service.reset("codex", &a.id, true, None).await.unwrap();
    assert_eq!(blocked.error, Some(UsageError::PendingReset));
    assert_eq!(fake.keys.lock().unwrap().len(), 1);
    let retry = service
        .reset("codex", &a.id, true, Some(&request_id))
        .await
        .unwrap();
    assert_eq!(
        retry.outcome,
        Some(ResetOutcome::Reset { windows_reset: 2 })
    );
    assert_eq!(
        *fake.keys.lock().unwrap(),
        vec![request_id.clone(), request_id]
    );
}

#[tokio::test]
async fn confirmed_reset_clears_only_captured_quota_without_stealing_active_account() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let a = login(&store, "a");
    let b = login(&store, "b");
    store
        .mark_quota_at(&a.id, &a.generation, now() + 500, now())
        .unwrap();
    let fake = Arc::new(Fake::new(
        vec![
            Ok(reading(Metric::Known(2), Some(false))),
            Ok(reading(Metric::Known(1), Some(true))),
        ],
        vec![Ok(ResetOutcome::Reset { windows_reset: 2 })],
    ));
    let service = AccountUsageService::new(store.clone(), vec![fake]);
    let report = service.reset("codex", &a.id, true, None).await.unwrap();
    assert!(report.reconciled);
    let registry = store.list().unwrap();
    assert_eq!(registry.accounts[0].status, AccountStatus::Available);
    assert_eq!(registry.active["codex"], b.id);
}

#[tokio::test]
async fn newer_quota_observation_prevents_reset_reconciliation_even_in_same_generation() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let a = login(&store, "personal");
    store
        .mark_quota_at(&a.id, &a.generation, now() + 500, now())
        .unwrap();
    let mut fake = Fake::new(
        vec![
            Ok(reading(Metric::Known(2), Some(false))),
            Ok(reading(Metric::Known(1), Some(true))),
        ],
        vec![Ok(ResetOutcome::Reset { windows_reset: 2 })],
    );
    fake.newer_quota_after_reset = Some((store.clone(), a.clone()));
    let service = AccountUsageService::new(store.clone(), vec![Arc::new(fake)]);
    let report = service.reset("codex", &a.id, true, None).await.unwrap();
    assert!(!report.reconciled);
    assert!(matches!(
        store.list().unwrap().accounts[0].status,
        AccountStatus::QuotaExhausted { .. }
    ));
}

#[tokio::test]
async fn confirmed_redemption_survives_failed_followup_and_retry_only_reconciles() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let a = login(&store, "personal");
    store
        .mark_quota_at(&a.id, &a.generation, now() + 500, now())
        .unwrap();
    let fake = Arc::new(Fake::new(
        vec![
            Ok(reading(Metric::Known(2), Some(false))),
            Err(UsageError::Transport),
            Ok(reading(Metric::Known(1), Some(true))),
        ],
        vec![Ok(ResetOutcome::Reset { windows_reset: 2 })],
    ));
    let service = AccountUsageService::new(store.clone(), vec![fake.clone()]);
    let report = service.reset("codex", &a.id, true, None).await.unwrap();
    assert_eq!(
        report.outcome,
        Some(ResetOutcome::Reset { windows_reset: 2 })
    );
    assert_eq!(report.refresh_error, Some(UsageError::Transport));
    assert!(!report.outcome_unknown);
    let retry = service
        .reset("codex", &a.id, true, report.request_id.as_deref())
        .await
        .unwrap();
    assert!(retry.reconciled);
    assert_eq!(fake.keys.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn refresh_cannot_resurrect_account_removed_while_reading() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let a = login(&store, "personal");
    let mut fake = Fake::new(vec![Ok(reading(Metric::Known(1), Some(true)))], vec![]);
    fake.remove_during_read = Some((store.clone(), a));
    let service = AccountUsageService::new(store.clone(), vec![Arc::new(fake)]);
    let report = service.status(None, true).await.unwrap();
    assert_eq!(
        report.accounts[0].refresh_error,
        Some(UsageError::AccountChanged)
    );
    assert!(store.list().unwrap().accounts.is_empty());
}

#[tokio::test]
async fn reset_lock_excludes_other_invocations_before_remote_reads() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let a = login(&store, "personal");
    let _guard = store.try_quota_reset_lock(&a.id).unwrap().unwrap();
    let fake = Arc::new(Fake::new(vec![], vec![]));
    let service = AccountUsageService::new(store, vec![fake]);
    assert!(matches!(
        service.reset("codex", &a.id, true, None).await,
        Err(UsageError::Busy)
    ));
}

#[test]
fn legacy_usage_is_readable_without_invented_observation_time_and_new_writes_are_atomic() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let a = login(&store, "personal");
    assert!(
        store.list().unwrap().accounts[0]
            .usage_observation
            .is_none()
    );
    let read = reading(Metric::Known(0), None);
    store
        .record_usage(&a.id, &a.generation, read.snapshot.clone())
        .unwrap();
    let first = store.list().unwrap();
    assert_eq!(
        first.accounts[0].usage_observation.as_ref().unwrap().source,
        UsageSource::Inference
    );
    assert!(
        first.accounts[0]
            .usage_observation
            .as_ref()
            .unwrap()
            .observed_at
            .is_some()
    );
    assert!(
        !store
            .record_usage_observed(
                &a.id,
                "removed-generation",
                read.snapshot,
                UsageObservation::default()
            )
            .unwrap()
    );
    assert_eq!(
        store.list().unwrap().accounts[0].usage,
        first.accounts[0].usage
    );
}
