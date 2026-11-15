//! Real loopback wire verification with synthetic credentials, never provider traffic.
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
    accounts::{Credential, Store},
    adapters::inbound::http::router,
    adapters::outbound::codex::client::Codex,
    config::{AutoReviewConfig, Config},
    ports::llm_provider::ProviderRegistry,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;
const PREFIX: &str = "You are a security monitor for autonomous AI coding agents.";
type Captures = Arc<Mutex<Vec<(HeaderMap, Value)>>>;

#[tokio::test]
#[ignore = "requires loopback sockets; uses only a synthetic localhost upstream"]
async fn classifier_wire_uses_own_model_without_session_state_and_preserves_errors() {
    async fn upstream(
        State(seen): State<Captures>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> axum::response::Response {
        assert_eq!(headers["authorization"], "Bearer synthetic-access");
        let failure = body["instructions"]
            .as_str()
            .is_some_and(|text| text.ends_with("synthetic failure"));
        seen.lock().unwrap().push((headers, body));
        if failure {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({"error":{"code":"model_not_allowed"}})),
            )
                .into_response();
        }
        ([ ("content-type", "text/event-stream") ], concat!(
            "data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"delta\":\"synthetic verdict payload\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n"
        )).into_response()
    }
    let seen: Captures = Arc::new(Mutex::new(vec![]));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/responses", post(upstream))
        .with_state(seen.clone());
    let upstream_task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path()).unwrap();
    store
        .login(
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
    let config = Config {
        default_model: Some("gpt-6.1-sol".into()),
        codex_endpoint: format!("http://{address}/responses"),
        auto_review: AutoReviewConfig {
            provider: Some("codex".into()),
            model: Some("gpt-5.5-fast".into()),
        },
        ..Default::default()
    };
    let mut providers = ProviderRegistry::default();
    providers.register(Arc::new(
        Codex::new(store.clone(), Arc::new(config.clone())).unwrap(),
    ));
    let app = router(llm_proxy::bootstrap::http_state(
        store,
        config,
        providers.clone(),
    ));
    for (index, system) in [
        "ordinary instructions".to_owned(),
        PREFIX.into(),
        "ordinary instructions".into(),
        format!("{PREFIX} synthetic failure"),
    ]
    .into_iter()
    .enumerate()
    {
        let request = Request::post("/v1/messages").header("content-type","application/json")
            .header("authorization","Bearer incoming-client-only")
            .header("x-claude-code-session-id","synthetic-session")
            .body(Body::from(json!({"model":"sonnet", "stream":false, "system":system,
                "messages":[{"role":"user", "content":"synthetic context must be preserved"}],
                "output_config":{"effort":"high", "format":{"type":"json_schema", "schema":{"type":"object", "properties":{"result":{"type":"string"}}, "additionalProperties":false}}}
            }).to_string())).unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(
            response.status(),
            if index == 3 {
                StatusCode::FORBIDDEN
            } else {
                StatusCode::OK
            }
        );
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        if index != 3 {
            assert_eq!(body["model"], "sonnet");
            assert_eq!(body["content"][0]["text"], "synthetic verdict payload");
        } else {
            assert_eq!(body["error"]["type"], "permission_error");
        }
    }
    providers.shutdown().await;
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 4);
    for (index, (headers, body)) in seen.iter().enumerate() {
        let review = index == 1 || index == 3;
        assert_eq!(
            body["model"],
            if review { "gpt-5.5" } else { "gpt-6.1-sol" }
        );
        assert_eq!(body["reasoning"]["effort"], "high");
        assert_eq!(body["text"]["format"]["type"], "json_schema");
        assert!(body["input"].as_array().unwrap().iter().any(|item| {
            item["content"].as_array().is_some_and(|parts| {
                parts
                    .iter()
                    .any(|part| part["text"] == "synthetic context must be preserved")
            })
        }));
        if review {
            assert!(body.get("prompt_cache_key").is_none());
            assert!(!headers.contains_key("session-id"));
            assert!(!headers.contains_key("x-codex-window-id"));
            assert_eq!(body["service_tier"], "priority");
        } else {
            assert_eq!(body["prompt_cache_key"], "synthetic-session");
            assert_eq!(headers["session-id"], "synthetic-session");
        }
    }
    upstream_task.abort();
}

#[tokio::test]
#[ignore = "requires loopback sockets; uses only a synthetic localhost upstream"]
async fn native_default_with_codex_classifier_normalizes_fast_lane_on_actual_wire() {
    async fn upstream(
        State(seen): State<Captures>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> impl IntoResponse {
        seen.lock().unwrap().push((headers, body));
        (
            [("content-type", "text/event-stream")],
            concat!(
                "data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"delta\":\"synthetic verdict\"}\n\n",
                "data: {\"type\":\"response.completed\",\"response\":{}}\n\n"
            ),
        )
    }
    let seen: Captures = Arc::new(Mutex::new(vec![]));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let upstream = Router::new()
        .route("/responses", post(upstream))
        .with_state(seen.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, upstream).await.unwrap();
    });
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path()).unwrap();
    store
        .login(
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
    let config = Config {
        default_provider: "agy".into(),
        default_model: Some("claude-sonnet-4-6".into()),
        codex_endpoint: format!("http://{address}/responses"),
        auto_review: AutoReviewConfig {
            provider: Some("codex".into()),
            model: Some("gpt-5.5-fast".into()),
        },
        ..Default::default()
    };
    config.validate().unwrap();
    let mut providers = ProviderRegistry::default();
    let auth =
        llm_proxy::authentication::AuthClient::with_issuer(format!("http://{address}")).unwrap();
    providers.register(Arc::new(
        Codex::with_auth(store.clone(), Arc::new(config.clone()), auth).unwrap(),
    ));
    let app = router(llm_proxy::bootstrap::http_state(
        store,
        config,
        providers.clone(),
    ));
    let response = app.oneshot(Request::post("/v1/messages").header("content-type", "application/json")
        .header("authorization", "Bearer synthetic-client-only")
        .body(Body::from(json!({"model":"sonnet", "system":PREFIX, "messages":[{"role":"user","content":"synthetic review"}],
            "output_config":{"effort":"medium", "format":{"type":"json_schema","schema":{"type":"object","properties":{"allow":{"type":"boolean"}}}}}
        }).to_string())).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["model"], "sonnet");
    providers.shutdown().await;
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0["authorization"], "Bearer synthetic-access");
    assert_eq!(seen[0].1["model"], "gpt-5.5");
    assert_eq!(seen[0].1["service_tier"], "priority");
    assert_eq!(seen[0].1["reasoning"]["effort"], "medium");
    task.abort();
}
