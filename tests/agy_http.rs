//! Anthropic inbound to native AGY loopback contracts with fabricated credentials.
use axum::{
    Json, Router,
    body::Body,
    extract::State,
    http::{HeaderMap, Request, StatusCode},
    response::IntoResponse,
    routing::post,
};
use http_body_util::BodyExt;
use llm_proxy::{
    accounts::{AccountStatus, Credential, Store},
    adapters::{
        inbound::http::router,
        outbound::{agy::client::Agy, sse::Decoder},
    },
    config::{AutoReviewConfig, Config},
    ports::llm_provider::ProviderRegistry,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

type Captures = Arc<Mutex<Vec<(HeaderMap, Value)>>>;

fn account(store: &Store, user: &str) {
    store
        .login_for(
            "agy",
            user,
            Credential {
                access_token: format!("synthetic-access-{user}"),
                refresh_token: "synthetic-refresh".into(),
                id_token: None,
                expires_at: u64::MAX,
                subject: user.into(),
                account_id: "synthetic-project".into(),
                generation: String::new(),
            },
        )
        .unwrap();
}

async fn serve(app: Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (url, task)
}

fn app(store: Store, endpoint: String) -> (Router, ProviderRegistry) {
    let config = Config {
        default_provider: "agy".into(),
        default_model: Some("claude-sonnet-4-6".into()),
        agy_endpoint: endpoint,
        auto_review: AutoReviewConfig {
            provider: Some("agy".into()),
            model: Some("sonnet".into()),
        },
        ..Default::default()
    };
    let mut providers = ProviderRegistry::default();
    providers.register(Arc::new(
        Agy::new(store.clone(), Arc::new(config.clone())).unwrap(),
    ));
    (
        router(llm_proxy::bootstrap::http_state(
            store,
            config,
            providers.clone(),
        )),
        providers,
    )
}

fn request(payload: Value) -> Request<Body> {
    Request::post("/v1/messages")
        .header("content-type", "application/json")
        .header("authorization", "Bearer incoming-client-only")
        .header("x-claude-code-session-id", "synthetic-conversation")
        .body(Body::from(payload.to_string()))
        .unwrap()
}

#[tokio::test]
#[ignore = "requires loopback sockets; only a synthetic localhost upstream"]
async fn buffered_and_streamed_requests_preserve_model_text_usage_and_credential_isolation() {
    async fn upstream(
        State(seen): State<Captures>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> impl IntoResponse {
        seen.lock().unwrap().push((headers, body));
        (
            [("content-type", "text/event-stream")],
            concat!(
                "data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Hello\"}]}}]}}\n\n",
                "data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\" \"}]}}]}}\n\n",
                "data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"world\"}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":12,\"cachedContentTokenCount\":2,\"candidatesTokenCount\":3,\"thoughtsTokenCount\":4}}}\n\n"
            ),
        )
    }
    let seen = Captures::default();
    let (endpoint, task) = serve(
        Router::new()
            .route("/v1internal:streamGenerateContent", post(upstream))
            .with_state(seen.clone()),
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    account(&store, "one");
    let (app, providers) = app(store, endpoint);
    for stream in [false, true] {
        let response = app
            .clone()
            .oneshot(request(json!({
                "model":"sonnet", "stream":stream, "system":"original synthetic system",
                "messages":[{"role":"user","content":"synthetic prompt"}]
            })))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        if stream {
            let mut decoder = Decoder::default();
            let events = decoder.push(&body).unwrap();
            decoder.finish().unwrap();
            let text: String = events
                .iter()
                .filter_map(|event| event.pointer("/delta/text").and_then(Value::as_str))
                .collect();
            assert_eq!(text, "Hello world");
            let start = events
                .iter()
                .find(|event| event["type"] == "message_start")
                .unwrap();
            assert_eq!(start["message"]["model"], "sonnet");
            let usage = &events
                .iter()
                .find(|event| event["type"] == "message_delta")
                .unwrap()["usage"];
            assert_eq!(usage["input_tokens"], 10);
            assert_eq!(usage["cache_read_input_tokens"], 2);
            assert_eq!(usage["output_tokens"], 7);
            assert_eq!(events.last().unwrap()["type"], "message_stop");
        } else {
            let body: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(body["model"], "sonnet");
            assert_eq!(body["content"][0]["text"], "Hello world");
            assert_eq!(body["usage"]["input_tokens"], 10);
            assert_eq!(body["usage"]["output_tokens"], 7);
            assert_eq!(body["stop_reason"], "end_turn");
        }
    }
    {
        let captures = seen.lock().unwrap();
        assert_eq!(captures.len(), 2);
        for (headers, body) in captures.iter() {
            assert_eq!(headers["authorization"], "Bearer synthetic-access-one");
            assert_eq!(body["model"], "claude-sonnet-4-6");
            assert_eq!(body["project"], "synthetic-project");
            assert_eq!(
                body["request"]["systemInstruction"]["parts"][0]["text"],
                "original synthetic system"
            );
        }
        assert_eq!(
            captures[0].1["request"]["sessionId"],
            captures[1].1["request"]["sessionId"]
        );
    }
    providers.shutdown().await;
    task.abort();
}

#[tokio::test]
#[ignore = "requires loopback sockets; only a synthetic localhost upstream"]
async fn ambiguous_resource_exhausted_does_not_rotate_or_mark_accounts_exhausted() {
    async fn upstream(
        State(seen): State<Captures>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> impl IntoResponse {
        seen.lock().unwrap().push((headers, body));
        (
            StatusCode::TOO_MANY_REQUESTS,
            Json(
                json!({"error":{"code":429,"status":"RESOURCE_EXHAUSTED","message":"synthetic secret must not be exposed"}}),
            ),
        )
    }
    let seen = Captures::default();
    let (endpoint, task) = serve(
        Router::new()
            .route("/v1internal:streamGenerateContent", post(upstream))
            .with_state(seen.clone()),
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    account(&store, "one");
    account(&store, "two");
    let active = store.list().unwrap().active["agy"].clone();
    let (app, providers) = app(store.clone(), endpoint);
    let response = app
        .oneshot(request(
            json!({"model":"sonnet","messages":[{"role":"user","content":"synthetic"}]}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert!(!String::from_utf8_lossy(&body).contains("synthetic secret"));
    assert_eq!(seen.lock().unwrap().len(), 1);
    let registry = store.list().unwrap();
    assert_eq!(registry.active["agy"], active);
    assert!(
        registry
            .accounts
            .iter()
            .all(|account| account.status == AccountStatus::Available)
    );
    providers.shutdown().await;
    task.abort();
}

#[tokio::test]
async fn unsupported_structured_classifier_returns_error_without_an_approval() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    account(&store, "one");
    let (app, providers) = app(store, "http://127.0.0.1:1".into());
    let response = app.oneshot(request(json!({
        "model":"sonnet", "stream":false,
        "system":"You are a security monitor for autonomous AI coding agents.",
        "messages":[{"role":"user","content":"synthetic classifier context"}],
        "output_config":{"format":{"type":"json_schema","schema":{"type":"object","properties":{"allow":{"type":"boolean"}}}}}
    }))).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert!(body.get("error").is_some());
    assert!(body.get("content").is_none());
    providers.shutdown().await;
}

#[tokio::test]
async fn explicit_effort_is_rejected_before_account_selection_and_count_remains_recognition_only() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (app, providers) = app(store, "http://127.0.0.1:1".into());
    for effort in ["none", "low", "medium", "high", "xhigh", "max"] {
        let payload = json!({"model":"sonnet", "reasoning_effort":effort,
            "messages":[{"role":"user","content":"synthetic"}]});
        let response = app.clone().oneshot(request(payload.clone())).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(
            body["error"]["message"],
            "AGY does not support explicit reasoning effort"
        );
        let response = app
            .clone()
            .oneshot(
                Request::post("/v1/messages/count_tokens")
                    .header("content-type", "application/json")
                    .body(Body::from(payload.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
    providers.shutdown().await;
}
