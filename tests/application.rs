//! Exercise application use cases without any transport or concrete LLM adapter.
use futures_util::{StreamExt, future::BoxFuture};
use llm_proxy::{
    accounts::Store,
    application::Application,
    config::{AutoReviewConfig, Config, ModelPolicy},
    domain::{
        account::{
            AccountCatalog, AccountIdentity, AccountSnapshot, AccountStatus, SelectionError,
        },
        error::{FailureKind, ProxyError},
        *,
    },
    ports::{
        account_repository::InferenceAccountRepository,
        llm_provider::{Provider, ProviderRegistry},
        request_routing::RequestRoute,
        token_estimator::TokenEstimator,
    },
};
use std::{
    collections::{BTreeMap, HashSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::sync::Semaphore;

struct SelectionGate {
    entered: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    release: Mutex<std::sync::mpsc::Receiver<()>>,
}

struct MemoryAccounts {
    account: AccountSnapshot,
    gate: Option<Arc<SelectionGate>>,
}
impl MemoryAccounts {
    fn new(gate: Option<Arc<SelectionGate>>) -> Self {
        Self {
            account: AccountSnapshot {
                provider: "fake".into(),
                id: "synthetic-id".into(),
                name: "synthetic".into(),
                subject: "synthetic-user".into(),
                workspace: "synthetic-workspace".into(),
                generation: "synthetic-generation".into(),
                status: AccountStatus::Available,
                usage: None,
                usage_observation: None,
                status_revision: 0,
            },
            gate,
        }
    }
}
impl InferenceAccountRepository for MemoryAccounts {
    fn list(&self) -> anyhow::Result<AccountCatalog> {
        Ok(AccountCatalog {
            accounts: vec![self.account.clone()],
            active: BTreeMap::from([("fake".into(), self.account.id.clone())]),
        })
    }
    fn select_for_provider_at(
        &self,
        provider: &str,
        excluded: &HashSet<String>,
        _time: u64,
    ) -> Result<AccountSnapshot, SelectionError> {
        if let Some(gate) = &self.gate {
            if let Some(sender) = gate.entered.lock().unwrap().take() {
                sender.send(()).unwrap();
            }
            gate.release.lock().unwrap().recv().unwrap();
        }
        if provider != self.account.provider || excluded.contains(&self.account.id) {
            return Err(SelectionError::NoAccounts);
        }
        Ok(self.account.clone())
    }
    fn record_usage(
        &self,
        _id: &str,
        _generation: &str,
        _usage: serde_json::Value,
    ) -> anyhow::Result<()> {
        anyhow::bail!("Unexpected usage mutation in repository probe")
    }
    fn mark_quota_at(
        &self,
        _id: &str,
        _generation: &str,
        _reset: u64,
        _time: u64,
    ) -> anyhow::Result<()> {
        anyhow::bail!("Unexpected quota mutation in repository probe")
    }
    fn mark_needs_login(&self, _id: &str, _generation: &str) -> anyhow::Result<()> {
        anyhow::bail!("Unexpected login mutation in repository probe")
    }
}

struct FixedEstimator(Arc<AtomicUsize>);
impl TokenEstimator for FixedEstimator {
    fn estimate(&self, _request: &GenerationRequest) -> u64 {
        self.0.fetch_add(1, Ordering::SeqCst);
        42
    }
}
#[derive(Clone)]
struct Seen {
    model: String,
    session: Option<String>,
    purpose: RequestPurpose,
}
struct FakeProvider(Arc<Mutex<Vec<Seen>>>);
impl Provider for FakeProvider {
    fn name(&self) -> &'static str {
        "fake"
    }
    fn generate(
        &self,
        request: Arc<GenerationRequest>,
        route: Arc<RequestRoute>,
        account: AccountIdentity,
        context: RequestContext,
    ) -> BoxFuture<'static, Result<EventStream, ProxyError>> {
        assert_eq!(account.provider, "fake");
        self.0.lock().unwrap().push(Seen {
            model: route.model.clone(),
            session: context.session_id,
            purpose: request.purpose,
        });
        Box::pin(async move {
            Ok(futures_util::stream::iter([
                Ok(GenerationEvent::Start {
                    id: "synthetic".into(),
                    model: request.model.clone(),
                    input_tokens: 1,
                }),
                Ok(GenerationEvent::Finish {
                    usage: Usage::default(),
                    reason: StopReason::EndTurn,
                }),
            ])
            .boxed())
        })
    }
}
fn request(purpose: RequestPurpose) -> GenerationRequest {
    GenerationRequest {
        purpose,
        model: "sonnet".into(),
        system: "synthetic".into(),
        messages: vec![],
        tools: vec![],
        tool_choice: ToolChoice::None,
        parallel_tools: false,
        effort: Some(ReasoningEffort::High),
        output_format: None,
        stream: true,
        compact: false,
    }
}
#[tokio::test]
async fn application_routes_reviews_and_counts_through_injected_ports_without_http() {
    let store = MemoryAccounts::new(None);
    let seen = Arc::new(Mutex::new(vec![]));
    let mut providers = ProviderRegistry::default();
    providers.register(Arc::new(FakeProvider(seen.clone())));
    let config = Config {
        default_provider: "fake".into(),
        default_model: Some("main-model".into()),
        auto_review: AutoReviewConfig {
            provider: None,
            model: Some("review-model".into()),
        },
        models: BTreeMap::from([("review-model".into(), ModelPolicy::default())]),
        ..Default::default()
    };
    let estimates = Arc::new(AtomicUsize::new(0));
    let app = Application::new(
        store,
        config,
        providers,
        Arc::new(FixedEstimator(estimates.clone())),
    );
    let permits = Arc::new(Semaphore::new(1));
    for purpose in [RequestPurpose::Conversation, RequestPurpose::ActionReview] {
        let permit = Arc::new(permits.clone().acquire_owned().await.unwrap());
        let context = RequestContext {
            request_id: "synthetic".into(),
            session_id: Some("session".into()),
        };
        let mut events = app
            .generate(request(purpose), context, permit)
            .await
            .unwrap();
        assert!(
            matches!(events.next().await, Some(Ok(GenerationEvent::Start { model, .. })) if model == "sonnet")
        );
        assert!(matches!(
            events.next().await,
            Some(Ok(GenerationEvent::Finish { .. }))
        ));
        assert!(events.next().await.is_none());
        drop(events);
        assert_eq!(permits.available_permits(), 1);
    }
    let permit = Arc::new(permits.clone().acquire_owned().await.unwrap());
    assert_eq!(
        app.count_tokens(request(RequestPurpose::ActionReview), permit)
            .await
            .unwrap(),
        42
    );
    assert_eq!(estimates.load(Ordering::SeqCst), 1);
    {
        let records = seen.lock().unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].model, "main-model");
        assert_eq!(records[0].session.as_deref(), Some("session"));
        assert_eq!(records[1].model, "review-model");
        assert_eq!(records[1].purpose, RequestPurpose::ActionReview);
        assert!(records[1].session.is_none());
    }
    let health = app.health().await.unwrap();
    assert!(health.ready);
    assert_eq!(health.provider, "fake");
    app.shutdown();
    let permit = Arc::new(permits.clone().acquire_owned().await.unwrap());
    let mut events = app
        .generate(
            request(RequestPurpose::Conversation),
            RequestContext {
                request_id: "shutdown".into(),
                session_id: None,
            },
            permit,
        )
        .await
        .unwrap();
    assert!(matches!(events.next().await, Some(Err(error)) if error.kind == FailureKind::Upstream));
    assert_eq!(seen.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn cancelled_selection_retains_admission_until_repository_work_finishes() {
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let gate = Arc::new(SelectionGate {
        entered: Mutex::new(Some(entered_tx)),
        release: Mutex::new(release_rx),
    });
    let seen = Arc::new(Mutex::new(vec![]));
    let mut providers = ProviderRegistry::default();
    providers.register(Arc::new(FakeProvider(seen.clone())));
    let app = Application::new(
        MemoryAccounts::new(Some(gate)),
        Config {
            default_provider: "fake".into(),
            default_model: Some("main-model".into()),
            ..Config::default()
        },
        providers,
        Arc::new(FixedEstimator(Arc::new(AtomicUsize::new(0)))),
    );
    let permits = Arc::new(Semaphore::new(1));
    let permit = Arc::new(permits.clone().acquire_owned().await.unwrap());
    let task = tokio::spawn(async move {
        app.generate(
            request(RequestPurpose::Conversation),
            RequestContext {
                request_id: "cancelled-selection".into(),
                session_id: None,
            },
            permit,
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), entered_rx)
        .await
        .unwrap()
        .unwrap();
    task.abort();
    let aborted = task.await;
    let retained = permits.available_permits();
    release_tx.send(()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while permits.available_permits() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(matches!(aborted, Err(error) if error.is_cancelled()));
    assert_eq!(retained, 0);
    assert!(seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn account_login_does_not_poll_authentication_before_name_validation_or_ticket() {
    use llm_proxy::application::account_commands;
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path()).unwrap();
    let polled = Arc::new(AtomicUsize::new(0));
    let probe = polled.clone();
    let authentication = async move {
        probe.fetch_add(1, Ordering::SeqCst);
        anyhow::bail!("synthetic authentication failure")
    };
    assert!(
        account_commands::login(store.clone(), " ".into(), authentication)
            .await
            .is_err()
    );
    assert_eq!(polled.load(Ordering::SeqCst), 0);
    let before = store.list().unwrap().operation_sequence;
    let copy = store.clone();
    let authentication = async move {
        assert!(copy.list().unwrap().operation_sequence > before);
        anyhow::bail!("synthetic authentication failure")
    };
    assert!(
        account_commands::login(store.clone(), "synthetic".into(), authentication)
            .await
            .is_err()
    );
    assert!(store.list().unwrap().accounts.is_empty());
}
