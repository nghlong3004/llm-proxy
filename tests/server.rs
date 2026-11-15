use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use futures_util::{StreamExt, future::BoxFuture};
use http_body_util::BodyExt;
use llm_proxy::{
    accounts::{Account, Credential, Store},
    adapters::inbound::http::router,
    config::Config,
    domain::error::{FailureKind, ProxyError},
    domain::*,
    ports::llm_provider::{Provider, ProviderRegistry},
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tower::ServiceExt;
#[derive(Clone)]
struct Fake {
    events: HashMap<String, Vec<Result<GenerationEvent, ProxyError>>>,
    calls: Arc<Mutex<Vec<String>>>,
}
impl Provider for Fake {
    fn name(&self) -> &'static str {
        "codex"
    }
    fn generate(
        &self,
        _request: Arc<GenerationRequest>,
        account: Account,
        _ctx: RequestContext,
    ) -> BoxFuture<'static, Result<EventStream, ProxyError>> {
        self.calls.lock().unwrap().push(account.id.clone());
        let events = self.events.get(&account.id).cloned().unwrap_or_default();
        Box::pin(async move { Ok(futures_util::stream::iter(events).boxed()) })
    }
}
fn credential(subject: &str) -> Credential {
    Credential {
        access_token: "test".into(),
        refresh_token: "test".into(),
        expires_at: u64::MAX,
        id_token: None,
        subject: subject.into(),
        account_id: "workspace".into(),
        generation: String::new(),
    }
}
fn success() -> Vec<Result<GenerationEvent, ProxyError>> {
    vec![
        Ok(GenerationEvent::Start {
            id: "msg_test".into(),
            model: "sonnet".into(),
            input_tokens: 2,
        }),
        Ok(GenerationEvent::BlockStart {
            index: 0,
            block: Content::Text(String::new()),
        }),
        Ok(GenerationEvent::TextDelta {
            index: 0,
            text: "works".into(),
        }),
        Ok(GenerationEvent::BlockEnd { index: 0 }),
        Ok(GenerationEvent::Finish {
            usage: Usage::default(),
            reason: StopReason::EndTurn,
        }),
    ]
}
fn req(stream: bool) -> Request<Body> {
    Request::post("/v1/messages").header("content-type","application/json").body(Body::from(json!({"model":"sonnet","stream":stream,"messages":[{"role":"user","content":"hello"}]}).to_string())).unwrap()
}
async fn response_json(response: axum::response::Response) -> Value {
    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
}
fn admission_app() -> (tempfile::TempDir, axum::Router) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let account = store.login("synthetic", credential("synthetic")).unwrap();
    let mut providers = ProviderRegistry::default();
    providers.register(Arc::new(Fake {
        events: HashMap::from([(account.id, success())]),
        calls: Arc::new(Mutex::new(vec![])),
    }));
    let app = router(llm_proxy::bootstrap::http_state(
        store,
        Config {
            default_model: Some("gpt-5.5".into()),
            ..Default::default()
        },
        providers,
    ));
    (dir, app)
}

#[tokio::test]
async fn saturated_admission_rejects_generation_and_token_count_without_reading_body() {
    let (_dir, app) = admission_app();
    let mut active = vec![];
    for _ in 0..32 {
        let response = app.clone().oneshot(req(true)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        active.push(response);
    }
    for route in ["/v1/messages", "/v1/messages/count_tokens"] {
        let polled = Arc::new(AtomicBool::new(false));
        let probe = polled.clone();
        let mut bytes = Some(axum::body::Bytes::from_static(
            b"{\"model\":\"sonnet\",\"messages\":[{\"role\":\"user\",\"content\":\"test\"}]}",
        ));
        let body = Body::from_stream(futures_util::stream::poll_fn(move |_| {
            probe.store(true, Ordering::SeqCst);
            std::task::Poll::Ready(bytes.take().map(Ok::<_, std::convert::Infallible>))
        }));
        let request = Request::post(route)
            .header("content-type", "application/json")
            .body(body)
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "{route}"
        );
        assert!(response.headers().contains_key("x-request-id"));
        assert!(
            !polled.load(Ordering::SeqCst),
            "overload read body for {route}"
        );
    }
    drop(active);
    assert_eq!(
        app.oneshot(req(false)).await.unwrap().status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn buffered_bodies_retain_admission_and_invalid_json_returns_capacity() {
    let (_dir, app) = admission_app();
    for _ in 0..40 {
        let request = Request::post("/v1/messages")
            .header("content-type", "application/json")
            .body(Body::from("not json"))
            .unwrap();
        assert_eq!(
            app.clone().oneshot(request).await.unwrap().status(),
            StatusCode::BAD_REQUEST
        );
    }
    let mut buffered = vec![];
    for _ in 0..32 {
        let response = app.clone().oneshot(req(false)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        buffered.push(response);
    }
    assert_eq!(
        app.clone().oneshot(req(false)).await.unwrap().status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    drop(buffered);
    assert_eq!(
        app.oneshot(req(false)).await.unwrap().status(),
        StatusCode::OK
    );
}

#[test]
fn cancelled_token_count_keeps_admission_until_blocking_job_finishes() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let (_dir, app) = admission_app();
        let mut active = vec![];
        for _ in 0..31 {
            active.push(app.clone().oneshot(req(true)).await.unwrap());
        }
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let blocker = tokio::task::spawn_blocking(move || {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });
        started_rx.await.unwrap();
        let mut token_request = req(false);
        *token_request.uri_mut() = "/v1/messages/count_tokens".parse().unwrap();
        let mut pending = Box::pin(app.clone().oneshot(token_request));
        assert!(futures_util::poll!(pending.as_mut()).is_pending());
        drop(pending);
        let mut another = req(false);
        *another.uri_mut() = "/v1/messages/count_tokens".parse().unwrap();
        // Do not assert until releasing the blocking pool, even on failure.
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            app.clone().oneshot(another),
        )
        .await;
        release_tx.send(()).unwrap();
        blocker.await.unwrap();
        drop(active);
        assert_eq!(
            response.unwrap().unwrap().status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    });
}

#[tokio::test]
async fn hard_quota_switches_but_generic_429_does_not() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let a = store.login("a", credential("a")).unwrap();
    let b = store.login("b", credential("b")).unwrap();
    let calls = Arc::new(Mutex::new(vec![]));
    let quota = ProxyError::new(
        429,
        FailureKind::Quota {
            reset_at: llm_proxy::accounts::now() + 60,
        },
        "quota",
    );
    let fake = Fake {
        events: HashMap::from([
            (a.id.clone(), vec![Ok(GenerationEvent::Ping), Err(quota)]),
            (b.id.clone(), success()),
        ]),
        calls: calls.clone(),
    };
    let mut registry = ProviderRegistry::default();
    registry.register(Arc::new(fake));
    let config = Config {
        default_model: Some("gpt-5.5".into()),
        ..Default::default()
    };
    let app = router(llm_proxy::bootstrap::http_state(
        store.clone(),
        config,
        registry,
    ));
    let response = app.clone().oneshot(req(false)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response_json(response).await["content"][0]["text"], "works");
    assert_eq!(*calls.lock().unwrap(), vec![a.id, b.id.clone()]);
    let fake = Fake {
        events: HashMap::from([(
            b.id.clone(),
            vec![Err(ProxyError::new(
                429,
                FailureKind::RateLimit,
                "capacity",
            ))],
        )]),
        calls: calls.clone(),
    };
    let mut registry = ProviderRegistry::default();
    registry.register(Arc::new(fake));
    let app = router(llm_proxy::bootstrap::http_state(
        store,
        Config {
            default_model: Some("gpt-5.5".into()),
            ..Default::default()
        },
        registry,
    ));
    assert_eq!(
        app.oneshot(req(false)).await.unwrap().status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(calls.lock().unwrap().len(), 3);
}
#[tokio::test]
async fn quota_after_start_does_not_replay_or_emit_success() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let a = store.login("a", credential("a")).unwrap();
    let b = store.login("b", credential("b")).unwrap();
    let calls = Arc::new(Mutex::new(vec![]));
    let fake = Fake {
        events: HashMap::from([
            (
                a.id.clone(),
                vec![
                    Ok(GenerationEvent::Start {
                        id: "msg".into(),
                        model: "sonnet".into(),
                        input_tokens: 0,
                    }),
                    Err(ProxyError::new(
                        429,
                        FailureKind::Quota {
                            reset_at: llm_proxy::accounts::now() + 60,
                        },
                        "quota",
                    )),
                ],
            ),
            (b.id, success()),
        ]),
        calls: calls.clone(),
    };
    let mut registry = ProviderRegistry::default();
    registry.register(Arc::new(fake));
    let app = router(llm_proxy::bootstrap::http_state(
        store,
        Config {
            default_model: Some("gpt-5.5".into()),
            ..Default::default()
        },
        registry,
    ));
    let response = app.oneshot(req(true)).await.unwrap();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(text.contains("event: error"));
    assert!(!text.contains("event: message_stop"));
    assert_eq!(*calls.lock().unwrap(), vec![a.id]);
}
#[tokio::test]
async fn known_all_quota_and_empty_pool_are_http_errors() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let app = router(llm_proxy::bootstrap::http_state(
        store.clone(),
        Config {
            default_model: Some("gpt-5.5".into()),
            ..Default::default()
        },
        ProviderRegistry::default(),
    ));
    assert_eq!(
        app.clone().oneshot(req(false)).await.unwrap().status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    let a = store.login("a", credential("a")).unwrap();
    let time = llm_proxy::accounts::now();
    store
        .mark_quota_at(&a.id, &a.generation, time + 60, time)
        .unwrap();
    let response = app.oneshot(req(true)).await.unwrap();
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(response.headers().contains_key("retry-after"));
}
struct PendingBackend {
    dropped: Arc<AtomicBool>,
}
struct Probe(Arc<AtomicBool>);
impl Drop for Probe {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}
impl Provider for PendingBackend {
    fn name(&self) -> &'static str {
        "codex"
    }
    fn generate(
        &self,
        _: Arc<GenerationRequest>,
        _: Account,
        _: RequestContext,
    ) -> BoxFuture<'static, Result<EventStream, ProxyError>> {
        let probe = Probe(self.dropped.clone());
        Box::pin(async move {
            Ok(async_stream::stream!{let _probe=probe;yield Ok(GenerationEvent::Start{id:"id".into(),model:"sonnet".into(),input_tokens:0});std::future::pending::<()>().await;}.boxed())
        })
    }
}
#[tokio::test]
async fn dropping_client_body_cancels_backend_stream() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store.login("a", credential("a")).unwrap();
    let dropped = Arc::new(AtomicBool::new(false));
    let mut registry = ProviderRegistry::default();
    registry.register(Arc::new(PendingBackend {
        dropped: dropped.clone(),
    }));
    let app = router(llm_proxy::bootstrap::http_state(
        store,
        Config {
            default_model: Some("gpt-5.5".into()),
            ..Default::default()
        },
        registry,
    ));
    let response = app.oneshot(req(true)).await.unwrap();
    let mut body = response.into_body().into_data_stream();
    while let Some(bytes) = body.next().await {
        if String::from_utf8_lossy(&bytes.unwrap()).contains("message_start") {
            break;
        }
    }
    drop(body);
    assert!(dropped.load(Ordering::SeqCst));
}

struct ReloginBackend {
    store: Store,
    calls: Arc<std::sync::atomic::AtomicUsize>,
}
impl Provider for ReloginBackend {
    fn name(&self) -> &'static str {
        "codex"
    }
    fn generate(
        &self,
        _: Arc<GenerationRequest>,
        _: Account,
        _: RequestContext,
    ) -> BoxFuture<'static, Result<EventStream, ProxyError>> {
        let changed = self.calls.fetch_add(1, Ordering::SeqCst) == 0;
        if changed {
            self.store.login("renewed", credential("a")).unwrap();
        }
        Box::pin(async move {
            if changed {
                Err(ProxyError::new(
                    409,
                    FailureKind::AccountChanged,
                    "Account changed before dispatch",
                ))
            } else {
                Ok(futures_util::stream::iter(success()).boxed())
            }
        })
    }
}
#[tokio::test]
async fn concurrent_relogin_reselects_without_marking_new_generation_exhausted() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store.login("a", credential("a")).unwrap();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut registry = ProviderRegistry::default();
    registry.register(Arc::new(ReloginBackend {
        store: store.clone(),
        calls: calls.clone(),
    }));
    let app = router(llm_proxy::bootstrap::http_state(
        store.clone(),
        Config {
            default_model: Some("gpt-5.5".into()),
            ..Default::default()
        },
        registry,
    ));
    assert_eq!(
        app.oneshot(req(false)).await.unwrap().status(),
        StatusCode::OK
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        store.list().unwrap().accounts[0].status,
        llm_proxy::accounts::AccountStatus::Available
    );
}
