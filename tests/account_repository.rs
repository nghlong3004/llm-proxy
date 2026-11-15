//! Repository injection and filesystem forwarding contracts; no live provider traffic.
use futures_util::future::BoxFuture;
use llm_proxy::{
    application::{account_commands, account_usage::AccountUsageService},
    domain::{
        account::{AccountCatalog, AccountIdentity, AccountSnapshot, AccountStatus, Credential},
        account_usage::{
            Metric, ResetAttempt, ResetAttemptState, ResetCredits, ResetDispatchError,
            ResetOutcome, UsageCapabilities, UsageData, UsageError, UsageObservation, UsageRead,
        },
    },
    ports::{
        account_repository::{AccountCommandRepository, AccountUsageRepository, ResetGuard},
        account_usage::AccountUsageProvider,
    },
};
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tokio::sync::Notify;

#[test]
fn filesystem_repository_selection_still_validates_credentials_before_projecting_snapshot() {
    use llm_proxy::{
        accounts::Store, domain::account::SelectionError,
        ports::account_repository::InferenceAccountRepository,
    };
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path()).unwrap();
    let account = store
        .login(
            "synthetic",
            Credential {
                access_token: "synthetic-access".into(),
                refresh_token: "synthetic-refresh".into(),
                id_token: None,
                expires_at: u64::MAX,
                subject: "synthetic-subject".into(),
                account_id: "synthetic-workspace".into(),
                generation: String::new(),
            },
        )
        .unwrap();
    let selected =
        InferenceAccountRepository::select_for_provider_at(&store, "codex", &Default::default(), 0)
            .unwrap();
    assert_eq!(selected.identity(), account.identity());
    std::fs::remove_file(
        directory
            .path()
            .join("credentials")
            .join(format!("{}-{}.json", account.id, account.generation,)),
    )
    .unwrap();
    assert!(matches!(
        InferenceAccountRepository::select_for_provider_at(&store, "codex", &Default::default(), 0,),
        Err(SelectionError::Storage(_)),
    ));
}

#[derive(Clone, Default)]
struct Commands(Arc<Mutex<Vec<&'static str>>>);

impl AccountCommandRepository for Commands {
    type Ticket = (String, u64);

    fn begin_login(&self, provider: &str) -> anyhow::Result<Self::Ticket> {
        self.0.lock().unwrap().push("begin");
        Ok((provider.into(), 7))
    }

    fn finish_login(
        &self,
        ticket: Self::Ticket,
        name: &str,
        credential: Credential,
    ) -> anyhow::Result<AccountSnapshot> {
        assert_eq!(ticket.1, 7);
        self.0.lock().unwrap().push("finish");
        Ok(AccountSnapshot {
            provider: ticket.0,
            id: "synthetic-account".into(),
            name: name.into(),
            subject: credential.subject,
            workspace: credential.account_id,
            generation: credential.generation,
            status: AccountStatus::Available,
            usage: None,
            usage_observation: None,
            status_revision: 0,
        })
    }

    fn remove(&self, _: &str) -> anyhow::Result<()> {
        self.0.lock().unwrap().push("remove");
        Ok(())
    }
}

#[tokio::test]
async fn login_reserves_repository_owned_ticket_before_polling_authentication() {
    let repository = Commands::default();
    let calls = repository.0.clone();
    let authentication_calls = calls.clone();
    let account = account_commands::login(repository, "synthetic".into(), async move {
        assert_eq!(*authentication_calls.lock().unwrap(), ["begin"]);
        authentication_calls.lock().unwrap().push("authenticate");
        Ok(Credential {
            access_token: "synthetic-access".into(),
            refresh_token: "synthetic-refresh".into(),
            id_token: None,
            expires_at: u64::MAX,
            subject: "synthetic-subject".into(),
            account_id: "synthetic-workspace".into(),
            generation: "synthetic-generation".into(),
        })
    })
    .await
    .unwrap();
    assert_eq!(account.name, "synthetic");
    assert_eq!(*calls.lock().unwrap(), ["begin", "authenticate", "finish"]);
}

#[tokio::test]
async fn invalid_login_name_does_not_begin_repository_operation_or_poll_authentication() {
    let repository = Commands::default();
    let calls = repository.0.clone();
    let result = account_commands::login(repository, " ".into(), async {
        panic!("invalid name must fail before authentication is polled")
    })
    .await;
    assert!(result.is_err());
    assert!(calls.lock().unwrap().is_empty());
}

struct State {
    account: AccountSnapshot,
    locked: AtomicBool,
    journal: Mutex<Option<ResetAttempt>>,
    save_started: Notify,
    save_finished: Notify,
    lock_released: Notify,
    release_save: (Mutex<bool>, Condvar),
    reset_calls: AtomicUsize,
}

#[derive(Clone)]
struct Repository(Arc<State>);

impl Repository {
    fn new() -> Self {
        Self(Arc::new(State {
            account: AccountSnapshot {
                provider: "synthetic".into(),
                id: uuid::Uuid::new_v4().to_string(),
                name: "injected account".into(),
                subject: "synthetic-subject".into(),
                workspace: "synthetic-workspace".into(),
                generation: uuid::Uuid::new_v4().to_string(),
                status: AccountStatus::Available,
                usage: None,
                usage_observation: None,
                status_revision: 0,
            },
            locked: AtomicBool::new(false),
            journal: Mutex::new(None),
            save_started: Notify::new(),
            save_finished: Notify::new(),
            lock_released: Notify::new(),
            release_save: (Mutex::new(false), Condvar::new()),
            reset_calls: AtomicUsize::new(0),
        }))
    }
}

struct LockResource(Arc<State>);
impl Drop for LockResource {
    fn drop(&mut self) {
        self.0.locked.store(false, Ordering::SeqCst);
        self.0.lock_released.notify_one();
    }
}

impl AccountUsageRepository for Repository {
    fn list(&self) -> anyhow::Result<AccountCatalog> {
        Ok(AccountCatalog {
            accounts: vec![self.0.account.clone()],
            active: [("synthetic".into(), self.0.account.id.clone())].into(),
        })
    }

    fn record_usage_observed(
        &self,
        id: &str,
        generation: &str,
        _: serde_json::Value,
        _: UsageObservation,
    ) -> anyhow::Result<bool> {
        assert_eq!(id, self.0.account.id);
        assert_eq!(generation, self.0.account.generation);
        Ok(true)
    }

    fn reconcile_refreshed_quota(
        &self,
        _: &AccountSnapshot,
    ) -> anyhow::Result<Option<AccountStatus>> {
        Ok(Some(AccountStatus::Available))
    }

    fn reset_attempt(&self, _: &str) -> anyhow::Result<Option<ResetAttempt>> {
        Ok(self.0.journal.lock().unwrap().clone())
    }

    fn save_reset_attempt(&self, attempt: &ResetAttempt) -> anyhow::Result<()> {
        self.0.save_started.notify_one();
        let (release, wake) = &self.0.release_save;
        let released = release.lock().unwrap();
        let _released = wake.wait_while(released, |released| !*released).unwrap();
        assert!(self.0.locked.load(Ordering::SeqCst));
        *self.0.journal.lock().unwrap() = Some(attempt.clone());
        self.0.save_finished.notify_one();
        Ok(())
    }

    fn reconcile_quota_reset(&self, _: &ResetAttempt, _: bool) -> anyhow::Result<bool> {
        Ok(true)
    }

    fn try_quota_reset_lock(&self, _: &str) -> anyhow::Result<Option<ResetGuard>> {
        Ok(self
            .0
            .locked
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .ok()
            .map(|_| ResetGuard::new(LockResource(self.0.clone()))))
    }
}

struct Provider(Arc<State>);
impl AccountUsageProvider for Provider {
    fn name(&self) -> &'static str {
        "synthetic"
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

    fn read(&self, account: AccountIdentity) -> BoxFuture<'_, Result<UsageRead, UsageError>> {
        assert_eq!(account, self.0.account.identity());
        Box::pin(async {
            Ok(UsageRead {
                snapshot: serde_json::json!({"synthetic": true}),
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
        _: AccountIdentity,
        _: String,
    ) -> BoxFuture<'_, Result<ResetOutcome, ResetDispatchError>> {
        self.0.reset_calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { panic!("cancelled journal write must prevent reset dispatch") })
    }
}

#[tokio::test]
async fn usage_status_accepts_injected_repository_and_projects_provider_identity() {
    let repository = Repository::new();
    let service = AccountUsageService::new(
        repository.clone(),
        vec![Arc::new(Provider(repository.0.clone()))],
    );
    let report = service.status(Some("synthetic"), true).await.unwrap();
    assert!(!report.has_errors);
    assert_eq!(report.accounts[0].name, "injected account");
    assert!(report.accounts[0].active);
    assert_eq!(
        report.accounts[0].observation.source,
        llm_proxy::domain::account_usage::UsageSource::Live
    );
}

#[tokio::test]
async fn cancelled_reset_retains_opaque_guard_until_blocking_journal_write_finishes() {
    let repository = Repository::new();
    let service = AccountUsageService::new(
        repository.clone(),
        vec![Arc::new(Provider(repository.0.clone()))],
    );
    let id = repository.0.account.id.clone();
    let reset = tokio::spawn(async move { service.reset("synthetic", &id, true, None).await });
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        repository.0.save_started.notified(),
    )
    .await
    .unwrap();
    reset.abort();
    assert!(reset.await.unwrap_err().is_cancelled());
    let retained = repository.0.locked.load(Ordering::SeqCst);
    // Always release the blocking worker before asserting so failures cannot hang runtime shutdown.
    *repository.0.release_save.0.lock().unwrap() = true;
    repository.0.release_save.1.notify_all();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        repository.0.save_finished.notified(),
    )
    .await
    .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        repository.0.lock_released.notified(),
    )
    .await
    .unwrap();
    assert!(retained);
    assert!(!repository.0.locked.load(Ordering::SeqCst));
    assert_eq!(repository.0.reset_calls.load(Ordering::SeqCst), 0);
    assert!(matches!(
        repository.0.journal.lock().unwrap().as_ref().unwrap().state,
        ResetAttemptState::Pending
    ));
}
