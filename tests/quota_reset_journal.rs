use futures_util::future::BoxFuture;
use llm_proxy::{
    accounts::{Account, AccountStatus, Credential, Store, now},
    application::account_usage::AccountUsageService,
    domain::account_usage::*,
    ports::account_usage::AccountUsageProvider,
};
use std::sync::Arc;
use tokio::sync::Notify;

fn account(store: &Store) -> Account {
    store
        .login(
            "synthetic",
            Credential {
                access_token: "synthetic-access".into(),
                refresh_token: "synthetic-refresh".into(),
                id_token: None,
                expires_at: now() + 3600,
                subject: "synthetic-user".into(),
                account_id: "synthetic-workspace".into(),
                generation: String::new(),
            },
        )
        .unwrap()
}

fn attempt(account: &Account) -> ResetAttempt {
    ResetAttempt {
        provider: account.provider.clone(),
        account_id: account.id.clone(),
        generation: account.generation.clone(),
        request_id: uuid::Uuid::new_v4().to_string(),
        status_revision: account.status_revision,
        created_at: now(),
        state: ResetAttemptState::Confirmed(ResetOutcome::Reset { windows_reset: 2 }),
    }
}

#[test]
fn journal_is_private_and_confirmed_reset_never_clears_needs_login() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let a = account(&store);
    store.mark_needs_login(&a.id, &a.generation).unwrap();
    let current = store.list().unwrap().accounts.remove(0);
    let attempt = attempt(&current);
    let _guard = store.try_quota_reset_lock(&a.id).unwrap().unwrap();
    store.save_reset_attempt(&attempt).unwrap();
    assert!(!store.reconcile_quota_reset(&attempt, true).unwrap());
    assert_eq!(
        store.list().unwrap().accounts[0].status,
        AccountStatus::NeedsLogin
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.path().join(format!("quota-reset-{}.json", a.id));
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
}

#[test]
fn stale_reset_and_usage_never_modify_a_new_login_generation() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let a = account(&store);
    let attempt = attempt(&a);
    let b = account(&store);
    assert_eq!(a.id, b.id);
    assert_ne!(a.generation, b.generation);
    store
        .mark_quota_at(&b.id, &b.generation, now() + 300, now())
        .unwrap();
    assert!(!store.reconcile_quota_reset(&attempt, true).unwrap());
    assert!(
        !store
            .record_usage_observed(
                &a.id,
                &a.generation,
                serde_json::json!({"old": true}),
                UsageObservation::default()
            )
            .unwrap()
    );
    let current = store.list().unwrap().accounts.remove(0);
    assert!(matches!(
        current.status,
        AccountStatus::QuotaExhausted { .. }
    ));
    assert!(current.usage.is_none());
}

struct HangingReset {
    dispatched: Notify,
}
impl AccountUsageProvider for HangingReset {
    fn name(&self) -> &'static str {
        "codex"
    }
    fn capabilities(&self) -> UsageCapabilities {
        UsageCapabilities {
            live_read: true,
            reset: true,
        }
    }
    fn decode_cached(&self, _: &serde_json::Value) -> Result<UsageData, UsageError> {
        Ok(UsageData::default())
    }
    fn read(&self, _: Account) -> BoxFuture<'_, Result<UsageRead, UsageError>> {
        Box::pin(async {
            Ok(UsageRead {
                snapshot: serde_json::json!({}),
                data: UsageData {
                    reset_credits: Metric::Known(ResetCredits {
                        available: Metric::Known(1),
                        earliest_expiry: None,
                    }),
                    ..UsageData::default()
                },
                warning: None,
            })
        })
    }
    fn reset(
        &self,
        _: Account,
        _: String,
    ) -> BoxFuture<'_, Result<ResetOutcome, ResetDispatchError>> {
        Box::pin(async {
            self.dispatched.notify_one();
            std::future::pending().await
        })
    }
}

#[tokio::test]
async fn cancelled_dispatched_reset_leaves_pending_key_and_blocks_new_redemption() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let a = account(&store);
    let fake = Arc::new(HangingReset {
        dispatched: Notify::new(),
    });
    let service = Arc::new(AccountUsageService::new(store.clone(), vec![fake.clone()]));
    let task_service = service.clone();
    let id = a.id.clone();
    let task = tokio::spawn(async move { task_service.reset("codex", &id, true, None).await });
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        fake.dispatched.notified(),
    )
    .await
    .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    let journal = store.reset_attempt(&a.id).unwrap().unwrap();
    assert_eq!(journal.state, ResetAttemptState::Pending);
    let report = service.reset("codex", &a.id, true, None).await.unwrap();
    assert_eq!(report.error, Some(UsageError::PendingReset));
    assert_eq!(report.request_id, Some(journal.request_id));
    let regenerated = account(&store);
    assert!(matches!(
        service
            .reset("codex", &regenerated.id, true, report.request_id.as_deref())
            .await,
        Err(UsageError::InvalidRetry)
    ));
}
