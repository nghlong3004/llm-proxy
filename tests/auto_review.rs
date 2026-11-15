//! Client-side review routing uses the same Provider contract as conversation generation.
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use futures_util::{StreamExt, future::BoxFuture};
use http_body_util::BodyExt;
use llm_proxy::{
    accounts::{Account, Credential, Store},
    config::{AutoReviewConfig, Config},
    domain::{error::ProxyError, *},
    protocols::{ProtocolAdapter, anthropic::Anthropic},
    providers::{Provider, ProviderRegistry},
    server::{
        AppState,
        request_policy::{ConfiguredRequestRoutingPolicy, RequestRoute, RequestRoutingPolicy},
        router,
    },
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

const PREFIX: &str = "You are a security monitor for autonomous AI coding agents.";
struct Seen {
    provider: &'static str,
    model: String,
    purpose: RequestPurpose,
    session: Option<String>,
}
struct RecordingProvider {
    name: &'static str,
    seen: Arc<Mutex<Vec<Seen>>>,
}
impl Provider for RecordingProvider {
    fn name(&self) -> &'static str {
        self.name
    }
    fn generate(
        &self,
        request: Arc<GenerationRequest>,
        account: Account,
        context: RequestContext,
    ) -> BoxFuture<'static, Result<EventStream, ProxyError>> {
        assert_eq!(account.provider, self.name);
        self.seen.lock().unwrap().push(Seen {
            provider: self.name,
            model: request.model.clone(),
            purpose: request.purpose,
            session: context.session_id,
        });
        Box::pin(async move {
            Ok(futures_util::stream::iter(vec![
                Ok(GenerationEvent::Start {
                    id: "synthetic-id".into(),
                    model: request.model.clone(),
                    input_tokens: 1,
                }),
                Ok(GenerationEvent::BlockStart {
                    index: 0,
                    block: Content::Text("opaque synthetic review result".into()),
                }),
                Ok(GenerationEvent::BlockEnd { index: 0 }),
                Ok(GenerationEvent::Finish {
                    usage: Usage::default(),
                    reason: StopReason::EndTurn,
                }),
            ])
            .boxed())
        })
    }
}
fn fixture(config: Config) -> (tempfile::TempDir, AppState, Arc<Mutex<Vec<Seen>>>) {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path()).unwrap();
    let seen = Arc::new(Mutex::new(vec![]));
    let mut providers = ProviderRegistry::default();
    for provider in ["codex", "review-test"] {
        store
            .login_for(
                provider,
                provider,
                Credential {
                    access_token: "synthetic-access".into(),
                    refresh_token: "synthetic-refresh".into(),
                    id_token: None,
                    expires_at: u64::MAX,
                    subject: provider.into(),
                    account_id: "synthetic-workspace".into(),
                    generation: String::new(),
                },
            )
            .unwrap();
        providers.register(Arc::new(RecordingProvider {
            name: provider,
            seen: seen.clone(),
        }));
    }
    (directory, AppState::new(store, config, providers), seen)
}
fn config() -> Config {
    Config {
        default_model: Some("gpt-6.1-sol".into()),
        auto_review: AutoReviewConfig {
            provider: Some("review-test".into()),
            model: Some("gpt-5.5-fast".into()),
        },
        ..Default::default()
    }
}
fn payload(review: bool) -> Value {
    json!({"model":"sonnet", "system": if review { PREFIX } else { "ordinary instructions" },
        "stream":false, "output_config":{"effort":"high"},
        "messages":[{"role":"user", "content":"synthetic request"}]})
}
fn request(route: &str, value: Value) -> Request<Body> {
    Request::post(route)
        .header("content-type", "application/json")
        .header("x-claude-code-session-id", "synthetic-session")
        .body(Body::from(value.to_string()))
        .unwrap()
}
async fn body(response: axum::response::Response) -> Value {
    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
}
#[tokio::test]
async fn review_route_is_isolated_from_conversation_and_preserves_response_model() {
    let (_directory, state, seen) = fixture(config());
    let app = router(state);
    for review in [false, true, false, true] {
        let response = app
            .clone()
            .oneshot(request("/v1/messages", payload(review)))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let output = body(response).await;
        assert_eq!(output["model"], "sonnet");
        assert_eq!(
            output["content"][0]["text"],
            "opaque synthetic review result"
        );
    }
    let seen = seen.lock().unwrap();
    for (index, item) in seen.iter().enumerate() {
        if index % 2 == 0 {
            assert_eq!(item.provider, "codex");
            assert_eq!(item.model, "sonnet");
            assert_eq!(item.purpose, RequestPurpose::Conversation);
            assert_eq!(item.session.as_deref(), Some("synthetic-session"));
        } else {
            assert_eq!(item.provider, "review-test");
            assert_eq!(item.model, "gpt-5.5-fast");
            assert_eq!(item.purpose, RequestPurpose::ActionReview);
            assert!(item.session.is_none());
        }
    }
}
#[tokio::test]
async fn unknown_review_provider_does_not_fall_back_to_conversation() {
    let mut config = config();
    config.auto_review.provider = Some("not-registered".into());
    let (_directory, state, seen) = fixture(config);
    let app = router(state);
    let response = app
        .clone()
        .oneshot(request("/v1/messages", payload(true)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(seen.lock().unwrap().is_empty());
    let response = app
        .oneshot(request("/v1/messages", payload(false)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}
#[test]
fn omitted_review_model_defaults_to_luna_without_changing_conversation() {
    for input in [
        "default_model = 'gpt-6.1-sol'",
        "default_model = 'gpt-6.1-sol'\n[auto_review]",
        "default_model = 'gpt-6.1-sol'\n[auto_review]\nprovider = 'codex'",
    ] {
        let config: Config = toml::from_str(input).unwrap();
        config.validate().unwrap();
        assert_eq!(config.auto_review.model.as_deref(), Some("gpt-6-luna"));
        let policy = ConfiguredRequestRoutingPolicy::new(Arc::new(config));
        let mut request = Anthropic.decode(payload(false)).unwrap();
        assert_eq!(policy.route(&request).unwrap().model, "sonnet");
        request.purpose = RequestPurpose::ActionReview;
        assert_eq!(policy.route(&request).unwrap().model, "gpt-6-luna");
    }
    assert_eq!(
        AutoReviewConfig::default().model.as_deref(),
        Some("gpt-6-luna")
    );
}

#[test]
fn configured_policy_inherits_each_override_independently_and_keeps_model_tiers() {
    for provider in [None, Some("review-test")] {
        for model in [None, Some("gpt-5.5-ultrafast")] {
            let config = Config {
                default_model: Some("gpt-6.1-sol".into()),
                auto_review: AutoReviewConfig {
                    provider: provider.map(str::to_owned),
                    model: model.map(str::to_owned),
                },
                ..Default::default()
            };
            config.validate().unwrap();
            let policy = ConfiguredRequestRoutingPolicy::new(Arc::new(config));
            let mut request = Anthropic.decode(payload(false)).unwrap();
            assert_eq!(
                policy.route(&request).unwrap(),
                RequestRoute {
                    provider: "codex".into(),
                    model: "sonnet".into()
                }
            );
            request.purpose = RequestPurpose::ActionReview;
            assert_eq!(
                policy.route(&request).unwrap(),
                RequestRoute {
                    provider: provider.unwrap_or("codex").into(),
                    model: model.unwrap_or("sonnet").into(),
                }
            );
        }
    }
}
#[test]
fn review_config_rejects_blank_unknown_and_unregistered_models() {
    for snippet in [
        "[auto_review]\nmodel = ''",
        "[auto_review]\nprovider = ' '",
        "[auto_review]\nmodel = 'not-registered'",
        "[auto_review]\nmodel_typo = 'gpt-5.5'",
    ] {
        let input = format!("default_model = 'gpt-6.1-sol'\n{snippet}");
        let invalid = match toml::from_str::<Config>(&input) {
            Ok(config) => config.validate().is_err(),
            Err(_) => true,
        };
        assert!(invalid, "accepted {snippet}");
    }
    let config: Config = toml::from_str("default_model = 'gpt-6.1-sol'\n[auto_review]\nmodel = 'custom-review'\n[models.custom-review]\nresponses_lite = false\n").unwrap();
    config.validate().unwrap();
}
struct InjectedPolicy(std::sync::atomic::AtomicUsize);
impl RequestRoutingPolicy for InjectedPolicy {
    fn route(&self, _: &GenerationRequest) -> Result<RequestRoute, ProxyError> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(RequestRoute {
            provider: "review-test".into(),
            model: "gpt-5.5".into(),
        })
    }
}
#[tokio::test]
async fn injected_policy_is_replaceable_and_token_count_does_not_call_it() {
    let (_directory, mut state, seen) = fixture(config());
    let policy = Arc::new(InjectedPolicy(std::sync::atomic::AtomicUsize::new(0)));
    state.request_policy = policy.clone();
    let app = router(state);
    let response = app
        .clone()
        .oneshot(request("/v1/messages/count_tokens", payload(true)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(body(response).await["input_tokens"].as_u64().unwrap() > 0);
    assert_eq!(policy.0.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(seen.lock().unwrap().is_empty());
    let response = app
        .oneshot(request("/v1/messages", payload(false)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body(response).await["model"], "sonnet");
    assert_eq!(policy.0.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(seen.lock().unwrap()[0].provider, "review-test");
}
struct QuotaThenReview {
    inner: RecordingProvider,
    calls: std::sync::atomic::AtomicUsize,
}
impl Provider for QuotaThenReview {
    fn name(&self) -> &'static str {
        "review-test"
    }
    fn generate(
        &self,
        request: Arc<GenerationRequest>,
        account: Account,
        context: RequestContext,
    ) -> BoxFuture<'static, Result<EventStream, ProxyError>> {
        let events = self.inner.generate(request, account, context);
        if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            Box::pin(async {
                Err(ProxyError::new(
                    429,
                    llm_proxy::domain::error::FailureKind::Quota {
                        reset_at: llm_proxy::accounts::now() + 60,
                    },
                    "synthetic quota",
                ))
            })
        } else {
            events
        }
    }
}
#[tokio::test]
async fn quota_retry_stays_with_review_provider_and_preserves_conversation_active_account() {
    let (_directory, mut state, seen) = fixture(config());
    state
        .store
        .login_for(
            "review-test",
            "second-review",
            Credential {
                access_token: "synthetic-second-access".into(),
                refresh_token: "synthetic-second-refresh".into(),
                id_token: None,
                expires_at: u64::MAX,
                subject: "second-review-user".into(),
                account_id: "synthetic-workspace".into(),
                generation: String::new(),
            },
        )
        .unwrap();
    state.providers.register(Arc::new(QuotaThenReview {
        inner: RecordingProvider {
            name: "review-test",
            seen: seen.clone(),
        },
        calls: std::sync::atomic::AtomicUsize::new(0),
    }));
    let store = state.store.clone();
    let before = store.list().unwrap().active.get("codex").cloned();
    let response = router(state)
        .oneshot(request("/v1/messages", payload(true)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        body(response).await["content"][0]["text"],
        "opaque synthetic review result"
    );
    let records = seen.lock().unwrap();
    assert_eq!(records.len(), 2);
    assert!(
        records
            .iter()
            .all(|record| record.provider == "review-test" && record.model == "gpt-5.5-fast")
    );
    assert_eq!(store.list().unwrap().active.get("codex").cloned(), before);
}

#[tokio::test]
async fn original_unknown_model_is_rejected_even_with_review_override() {
    let (_directory, state, seen) = fixture(config());
    let mut value = payload(true);
    value["model"] = json!("not-registered");
    let response = router(state)
        .oneshot(request("/v1/messages", value))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(seen.lock().unwrap().is_empty());
}
