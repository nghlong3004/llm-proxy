//! Exercise application use cases without any transport or concrete LLM adapter.
use futures_util::{StreamExt, future::BoxFuture};
use llm_proxy::{
    accounts::{Account, Credential, Store},
    application::Application,
    config::{AutoReviewConfig, Config, ModelPolicy},
    domain::{
        error::{FailureKind, ProxyError},
        *,
    },
    ports::{
        llm_provider::{Provider, ProviderRegistry},
        token_estimator::TokenEstimator,
    },
};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::sync::Semaphore;

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
        account: Account,
        context: RequestContext,
    ) -> BoxFuture<'static, Result<EventStream, ProxyError>> {
        assert_eq!(account.provider, "fake");
        self.0.lock().unwrap().push(Seen {
            model: request.model.clone(),
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
        effort: "high".into(),
        output_format: None,
        stream: true,
        compact: false,
    }
}
#[tokio::test]
async fn application_routes_reviews_and_counts_through_injected_ports_without_http() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path()).unwrap();
    store
        .login_for(
            "fake",
            "synthetic",
            Credential {
                access_token: "synthetic-access".into(),
                refresh_token: "synthetic-refresh".into(),
                id_token: None,
                expires_at: u64::MAX,
                subject: "synthetic-user".into(),
                account_id: "synthetic-workspace".into(),
                generation: String::new(),
            },
        )
        .unwrap();
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
        assert_eq!(records[0].model, "sonnet");
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
