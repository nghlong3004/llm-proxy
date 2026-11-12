//! Loopback HTTP contracts. Run with --include-ignored outside socket-restricted sandboxes.
use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
};
use futures_util::TryStreamExt;
use llm_proxy::{
    accounts::{AccountStatus, Credential, Store},
    authentication::AuthClient,
    config::Config,
    domain::*,
    providers::Provider,
    providers::codex::client::Codex,
};
use serde_json::{Value, json};
type CapturedRequest = Arc<Mutex<Option<(HeaderMap, Value)>>>;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
fn credential(user: &str, expires_at: u64) -> Credential {
    Credential {
        access_token: format!("access-{user}"),
        refresh_token: format!("refresh-{user}"),
        id_token: None,
        expires_at,
        subject: user.into(),
        account_id: "team".into(),
        generation: String::new(),
    }
}
async fn serve(app: Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (url, server)
}
#[tokio::test]
#[ignore = "requires loopback sockets; run cargo test -- --include-ignored on the host"]
async fn parallel_refresh_is_single_flight_and_preserves_rotated_refresh_token() {
    let count = Arc::new(AtomicUsize::new(0));
    async fn refresh(State(count): State<Arc<AtomicUsize>>, body: String) -> Json<Value> {
        assert!(body.contains("grant_type=refresh_token"));
        count.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        Json(json!({"access_token":"renewed","refresh_token":"rotated","expires_in":3600}))
    }
    let (url, server) = serve(
        Router::new()
            .route("/oauth/token", post(refresh))
            .with_state(count.clone()),
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let a = store.login("a", credential("a", 0)).unwrap();
    let auth = AuthClient::with_issuer(url).unwrap();
    let mut tasks = vec![];
    for _ in 0..8 {
        let store = store.clone();
        let auth = auth.clone();
        let id = a.id.clone();
        tasks.push(tokio::spawn(async move {
            auth.refresh(&store, &id, None).await.unwrap()
        }));
    }
    for task in tasks {
        assert_eq!(task.await.unwrap().access_token, "renewed");
    }
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert_eq!(
        store.lease(&a.id).unwrap().credential.refresh_token,
        "rotated"
    );
    server.abort();
}
#[derive(Clone, Default)]
struct PausedRefresh {
    count: Arc<AtomicUsize>,
    rotated: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}
async fn paused_refresh(State(state): State<PausedRefresh>) -> axum::response::Response {
    let first = state.count.fetch_add(1, Ordering::SeqCst) == 0;
    let stream = async_stream::stream! {
        yield Ok::<_, std::convert::Infallible>(Bytes::from_static(b"{\"access_token\":\"renewed\","));
        if first {
            state.rotated.notify_one();
            state.release.notified().await;
        }
        yield Ok(Bytes::from_static(b"\"refresh_token\":\"rotated\",\"expires_in\":3600}"));
    };
    (
        [("content-type", "application/json")],
        Body::from_stream(stream),
    )
        .into_response()
}

#[tokio::test]
#[ignore = "requires loopback sockets; run cargo test -- --include-ignored on the host"]
async fn cancelled_refresh_caller_does_not_lose_rotation_or_release_single_flight() {
    let state = PausedRefresh::default();
    let (url, server) = serve(
        Router::new()
            .route("/oauth/token", post(paused_refresh))
            .with_state(state.clone()),
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let account = store.login("a", credential("a", 0)).unwrap();
    let auth = AuthClient::with_issuer(url).unwrap();
    let caller = {
        let auth = auth.clone();
        let store = store.clone();
        let id = account.id.clone();
        tokio::spawn(async move { auth.refresh(&store, &id, None).await })
    };
    tokio::time::timeout(std::time::Duration::from_secs(2), state.rotated.notified())
        .await
        .unwrap();
    caller.abort();
    assert!(matches!(caller.await, Err(error) if error.is_cancelled()));
    let second = {
        let auth = auth.clone();
        let store = store.clone();
        let id = account.id.clone();
        tokio::spawn(async move { auth.refresh(&store, &id, None).await })
    };
    state.release.notify_one();
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(2), second)
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .refresh_token,
        "rotated"
    );
    assert_eq!(
        state.count.load(Ordering::SeqCst),
        1,
        "cancel must not release the refresh single-flight lock before persistence"
    );
    assert_eq!(
        store.lease(&account.id).unwrap().credential.refresh_token,
        "rotated"
    );
    server.abort();
}

#[tokio::test]
#[ignore = "requires loopback sockets; run cargo test -- --include-ignored on the host"]
async fn registry_shutdown_drains_cancelled_rotation_and_closes_refresh_admission() {
    use llm_proxy::providers::ProviderRegistry;
    let state = PausedRefresh::default();
    let (url, server) = serve(
        Router::new()
            .route("/oauth/token", post(paused_refresh))
            .with_state(state.clone()),
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let account = store.login("a", credential("a", 0)).unwrap();
    let auth = AuthClient::with_issuer(url).unwrap();
    let mut providers = ProviderRegistry::default();
    providers.register(Arc::new(
        Codex::with_auth(store.clone(), Arc::new(Config::default()), auth.clone()).unwrap(),
    ));
    let caller = {
        let auth = auth.clone();
        let store = store.clone();
        let id = account.id.clone();
        tokio::spawn(async move { auth.refresh(&store, &id, None).await })
    };
    tokio::time::timeout(std::time::Duration::from_secs(2), state.rotated.notified())
        .await
        .unwrap();
    caller.abort();
    let _ = caller.await;
    let mut drain = tokio::spawn(async move { providers.shutdown().await });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), &mut drain)
            .await
            .is_err(),
        "shutdown must wait for the rotated response body and persistence"
    );
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            auth.refresh(&store, &account.id, None)
        )
        .await
        .unwrap()
        .is_err()
    );
    state.release.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(2), drain)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        store.lease(&account.id).unwrap().credential.refresh_token,
        "rotated"
    );
    assert_eq!(state.count.load(Ordering::SeqCst), 1);
    server.abort();
}

#[tokio::test]
#[ignore = "requires loopback sockets; run cargo test -- --include-ignored on the host"]
async fn rotation_finishing_after_relogin_or_removal_never_overwrites_or_resurrects() {
    use llm_proxy::authentication::RefreshError;
    for remove in [false, true] {
        let state = PausedRefresh::default();
        let (url, server) = serve(
            Router::new()
                .route("/oauth/token", post(paused_refresh))
                .with_state(state.clone()),
        )
        .await;
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let selected = store.login("a", credential("a", 0)).unwrap();
        let auth = AuthClient::with_issuer(url).unwrap();
        let caller = {
            let auth = auth.clone();
            let store = store.clone();
            let selected = selected.clone();
            tokio::spawn(async move {
                auth.refresh_for_generation(&store, &selected.id, &selected.generation, None)
                    .await
            })
        };
        tokio::time::timeout(std::time::Duration::from_secs(2), state.rotated.notified())
            .await
            .unwrap();
        if remove {
            store.remove(&selected.id).unwrap();
        } else {
            let mut replacement = credential("a", u64::MAX);
            replacement.refresh_token = "replacement-refresh".into();
            store.login("new", replacement).unwrap();
        }
        state.release.notify_one();
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), caller)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(result, Err(RefreshError::AccountChanged)));
        auth.shutdown().await;
        let registry = store.list().unwrap();
        if remove {
            assert!(registry.accounts.is_empty());
            assert_eq!(
                std::fs::read_dir(dir.path().join("credentials"))
                    .unwrap()
                    .count(),
                0
            );
        } else {
            assert_ne!(registry.accounts[0].generation, selected.generation);
            assert_eq!(
                store.lease(&selected.id).unwrap().credential.refresh_token,
                "replacement-refresh"
            );
            assert_eq!(
                std::fs::read_dir(dir.path().join("credentials"))
                    .unwrap()
                    .count(),
                1
            );
        }
        server.abort();
    }
}

#[tokio::test]
#[ignore = "requires loopback sockets; run cargo test -- --include-ignored on the host"]
async fn http_sse_transport_headers_and_fragmented_utf8_follow_the_native_contract() {
    let captured = Arc::new(Mutex::new(None));
    async fn responses(
        State(captured): State<CapturedRequest>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> axum::response::Response {
        *captured.lock().unwrap() = Some((headers, body));
        let text = concat!(
            "data: {\"type\":\"response.created\"}\r\n\r\n",
            "data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"delta\":\"Việt 🦀\"}\r\n\r\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":12,\"output_tokens\":3}}}\r\n\r\n"
        );
        let chunks = text
            .as_bytes()
            .iter()
            .map(|b| Ok::<Bytes, std::convert::Infallible>(Bytes::copy_from_slice(&[*b])))
            .collect::<Vec<_>>();
        (
            [("content-type", "text/event-stream")],
            Body::from_stream(futures_util::stream::iter(chunks)),
        )
            .into_response()
    }
    let (url, server) = serve(
        Router::new()
            .route("/responses", post(responses))
            .with_state(captured.clone()),
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let a = store.login("a", credential("a", u64::MAX)).unwrap();
    let config = Config {
        default_model: Some("gpt-5.5".into()),
        codex_endpoint: format!("{url}/responses"),
        ..Default::default()
    };
    let provider = Codex::new(store, Arc::new(config)).unwrap();
    use llm_proxy::protocols::ProtocolAdapter;
    let request = llm_proxy::protocols::anthropic::Anthropic
        .decode(json!({"model":"sonnet","messages":[{"role":"user","content":"hello"}]}))
        .unwrap();
    let events = provider
        .generate(
            Arc::new(request),
            a,
            RequestContext {
                request_id: "request".into(),
                session_id: Some("session".into()),
            },
        )
        .await
        .unwrap()
        .try_collect::<Vec<_>>()
        .await
        .unwrap();
    assert!(
        events
            .iter()
            .any(|event| matches!(event,GenerationEvent::TextDelta{text,..}if text=="Việt 🦀"))
    );
    let (headers, body) = captured.lock().unwrap().take().unwrap();
    assert_eq!(headers["authorization"], "Bearer access-a");
    assert_eq!(headers["chatgpt-account-id"], "team");
    assert_eq!(headers["session-id"], "session");
    assert_eq!(body["model"], "gpt-5.5");
    assert_eq!(body["store"], false);
    assert!(matches!(
        events.last(),
        Some(GenerationEvent::Finish { .. })
    ));
    server.abort();
}
#[tokio::test]
#[ignore = "requires loopback sockets; run cargo test -- --include-ignored on the host"]
async fn refresh_revocation_marks_only_the_rejected_account() {
    let (url, server) = serve(Router::new().route(
        "/oauth/token",
        post(|| async {
            (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"invalid_grant"})),
            )
        }),
    ))
    .await;
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let a = store.login("a", credential("a", 0)).unwrap();
    let b = store.login("b", credential("b", u64::MAX)).unwrap();
    assert!(
        AuthClient::with_issuer(url)
            .unwrap()
            .refresh(&store, &a.id, None)
            .await
            .is_err()
    );
    let registry = store.list().unwrap();
    assert_eq!(registry.accounts[0].status, AccountStatus::NeedsLogin);
    assert_eq!(registry.accounts[1].status, AccountStatus::Available);
    assert_eq!(
        store
            .select_at(
                &std::collections::HashSet::new(),
                llm_proxy::accounts::now()
            )
            .unwrap()
            .account
            .id,
        b.id
    );
    server.abort();
}

#[tokio::test]
#[ignore = "requires loopback sockets; run cargo test -- --include-ignored on the host"]
async fn anthropic_http_request_fails_over_to_codex_account_b_and_stays_there() {
    use llm_proxy::{
        providers::ProviderRegistry,
        server::{AppState, router},
    };
    let attempts = Arc::new(Mutex::new(Vec::<String>::new()));
    async fn upstream(
        State(attempts): State<Arc<Mutex<Vec<String>>>>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> axum::response::Response {
        let authorization = headers["authorization"].to_str().unwrap().to_owned();
        assert_ne!(authorization, "Bearer INCOMING_LOCAL_SECRET");
        attempts.lock().unwrap().push(authorization.clone());
        assert_eq!(body["model"], "gpt-5.5");
        assert_eq!(body["input"][1]["role"], "developer");
        assert_eq!(
            body["input"][1]["content"][0]["text"],
            "in-turn instruction"
        );
        if authorization == "Bearer access-a" {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                Json(json!({"error":{"code":"usage_limit_reached","resets_in_seconds":3600}})),
            )
                .into_response();
        }
        assert_eq!(authorization, "Bearer access-b");
        ([("content-type", "text/event-stream")], concat!(
            "data: {\"type\":\"response.created\"}\n\n",
            "data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"delta\":\"ACCOUNT_B_OK\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":5,\"output_tokens\":2}}}\n\n"
        )).into_response()
    }
    let (upstream_url, upstream_server) = serve(
        Router::new()
            .route("/responses", post(upstream))
            .with_state(attempts.clone()),
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store.login("a", credential("a", u64::MAX)).unwrap();
    let b = store.login("b", credential("b", u64::MAX)).unwrap();
    let config = Config {
        default_model: Some("gpt-5.5".into()),
        codex_endpoint: format!("{upstream_url}/responses"),
        ..Default::default()
    };
    let mut registry = ProviderRegistry::default();
    registry.register(Arc::new(
        Codex::new(store.clone(), Arc::new(config.clone())).unwrap(),
    ));
    let (proxy_url, proxy_server) =
        serve(router(AppState::new(store.clone(), config, registry))).await;
    let client = reqwest::Client::new();
    for stream in [false, true] {
        let response = client.post(format!("{proxy_url}/v1/messages"))
            .bearer_auth("INCOMING_LOCAL_SECRET")
            .json(&json!({"model":"sonnet","stream":stream,"messages":[{"role":"user","content":"hello"},{"role":"system","content":[{"type":"text","text":"in-turn instruction"}]}]}))
            .send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().contains_key("x-request-id"));
        let text = response.text().await.unwrap();
        if stream {
            assert!(text.contains("event: message_stop"));
            assert!(text.contains("ACCOUNT_B_OK"));
        } else {
            let body: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(body["content"][0]["text"], "ACCOUNT_B_OK");
            assert_eq!(body["usage"]["output_tokens"], 2);
        }
    }
    assert_eq!(
        *attempts.lock().unwrap(),
        ["Bearer access-a", "Bearer access-b", "Bearer access-b"]
    );
    assert_eq!(store.list().unwrap().active["codex"], b.id);
    proxy_server.abort();
    upstream_server.abort();
}

#[tokio::test]
#[ignore = "requires loopback sockets; run cargo test -- --include-ignored on the host"]
async fn lite_http_request_must_match_the_real_raine_translator_contract() {
    let fixtures: Vec<Value> =
        serde_json::from_str(include_str!("fixtures/codex/raine-contract.json")).unwrap();
    for fixture in fixtures
        .into_iter()
        .filter(|fixture| fixture["lite"] == true)
    {
        async fn upstream(
            State(expected): State<Value>,
            headers: HeaderMap,
            Json(body): Json<Value>,
        ) -> axum::response::Response {
            assert_eq!(
                body, expected["expected"],
                "native request differs from reference translator"
            );
            for name in [
                "originator",
                "x-codex-beta-features",
                "x-codex-window-id",
                "x-openai-internal-codex-responses-lite",
            ] {
                assert_eq!(
                    headers[name].to_str().unwrap(),
                    expected["expected_headers"][name].as_str().unwrap(),
                    "header {name}"
                );
            }
            ([("content-type", "text/event-stream")], concat!(
                "data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"delta\":\"CONTRACT_OK\"}\n\n",
                "data: {\"type\":\"response.completed\",\"response\":{}}\n\n"
            )).into_response()
        }
        let (url, server) = serve(
            Router::new()
                .route("/responses", post(upstream))
                .with_state(fixture.clone()),
        )
        .await;
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let account = store.login("fake", credential("fake", u64::MAX)).unwrap();
        let config = Config {
            default_model: Some("gpt-6.1-sol".into()),
            codex_endpoint: format!("{url}/responses"),
            ..Default::default()
        };
        let provider = Codex::new(store, Arc::new(config)).unwrap();
        use llm_proxy::protocols::ProtocolAdapter;
        let request = llm_proxy::protocols::anthropic::Anthropic
            .decode(fixture["input"].clone())
            .unwrap();
        let events = provider
            .generate(
                Arc::new(request),
                account,
                RequestContext {
                    request_id: "contract-request".into(),
                    session_id: Some("contract-session".into()),
                },
            )
            .await
            .unwrap()
            .try_collect::<Vec<_>>()
            .await
            .unwrap();
        assert!(events.iter().any(
            |event| matches!(event,GenerationEvent::TextDelta{text,..} if text == "CONTRACT_OK")
        ));
        server.abort();
    }
}

#[tokio::test]
async fn corrupt_selected_credential_is_storage_failure_not_reselection_or_transport() {
    use llm_proxy::{domain::error::FailureKind, protocols::ProtocolAdapter};
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let account = store.login("a", credential("a", 0)).unwrap();
    let path = dir
        .path()
        .join("credentials")
        .join(format!("{}-{}.json", account.id, account.generation));
    std::fs::write(path, b"invalid json").unwrap();
    let config = Config {
        default_model: Some("gpt-5.5".into()),
        ..Default::default()
    };
    let provider = Codex::new(store, Arc::new(config)).unwrap();
    let request = llm_proxy::protocols::anthropic::Anthropic
        .decode(json!({"model":"sonnet","messages":[{"role":"user","content":"synthetic"}]}))
        .unwrap();
    let error = match provider
        .generate(
            Arc::new(request),
            account,
            RequestContext {
                request_id: "corrupt".into(),
                session_id: None,
            },
        )
        .await
    {
        Ok(_) => panic!("corrupt storage must stop dispatch"),
        Err(error) => error,
    };
    assert_eq!(error.kind, FailureKind::Storage);
}

#[tokio::test]
#[ignore = "requires loopback sockets; run cargo test -- --include-ignored on the host"]
async fn selected_removed_or_replaced_account_is_not_transport_or_refreshed() {
    use llm_proxy::{domain::error::FailureKind, protocols::ProtocolAdapter};
    let calls = Arc::new(AtomicUsize::new(0));
    async fn unexpected(State(calls): State<Arc<AtomicUsize>>) -> StatusCode {
        calls.fetch_add(1, Ordering::SeqCst);
        StatusCode::INTERNAL_SERVER_ERROR
    }
    let (url, server) = serve(
        Router::new()
            .route("/oauth/token", post(unexpected))
            .route("/responses", post(unexpected))
            .with_state(calls.clone()),
    )
    .await;
    for remove in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let selected = store.login("selected", credential("a", 0)).unwrap();
        if remove {
            store.remove(&selected.id).unwrap();
        } else {
            store.login("replacement", credential("a", 0)).unwrap();
        }
        let config = Config {
            default_model: Some("gpt-5.5".into()),
            codex_endpoint: format!("{url}/responses"),
            ..Default::default()
        };
        let provider = Codex::with_auth(
            store,
            Arc::new(config),
            AuthClient::with_issuer(url.clone()).unwrap(),
        )
        .unwrap();
        let request = llm_proxy::protocols::anthropic::Anthropic
            .decode(json!({"model":"sonnet","messages":[{"role":"user","content":"synthetic"}]}))
            .unwrap();
        let error = match provider
            .generate(
                Arc::new(request),
                selected,
                RequestContext {
                    request_id: "mutation".into(),
                    session_id: None,
                },
            )
            .await
        {
            Ok(_) => panic!("stale selection must not dispatch"),
            Err(error) => error,
        };
        assert_eq!(error.kind, FailureKind::AccountChanged);
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "a stale selection must not refresh a newer login"
    );
    server.abort();
}

#[tokio::test]
#[ignore = "requires loopback sockets; run cargo test -- --include-ignored on the host"]
async fn native_validation_failure_preserves_status_and_safe_parameter_without_body_echoes() {
    let (url, server) = serve(Router::new().route("/responses", post(|| async {
        (StatusCode::UNPROCESSABLE_ENTITY, Json(json!({"error":{"code":"unsupported_parameter","param":"reasoning.context","message":"SECRET_UPSTREAM_PROMPT_OR_TOKEN"}})))
    }))).await;
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let account = store.login("fake", credential("fake", u64::MAX)).unwrap();
    let config = Config {
        default_model: Some("gpt-6.1-sol".into()),
        codex_endpoint: format!("{url}/responses"),
        ..Default::default()
    };
    let provider = Codex::new(store, Arc::new(config)).unwrap();
    use llm_proxy::protocols::ProtocolAdapter;
    let request = llm_proxy::protocols::anthropic::Anthropic
        .decode(json!({"model":"sonnet","messages":[{"role":"user","content":"synthetic"}]}))
        .unwrap();
    let error = match provider
        .generate(
            Arc::new(request),
            account,
            RequestContext {
                request_id: "contract-request".into(),
                session_id: None,
            },
        )
        .await
    {
        Ok(_) => panic!("expected native rejection"),
        Err(error) => error,
    };
    assert_eq!(error.upstream_status, Some(422));
    assert_eq!(error.upstream_parameter, Some("reasoning.context"));
    assert_eq!(error.code, "upstream_unsupported_parameter");
    assert!(!format!("{error:?}").contains("SECRET_UPSTREAM"));
    server.abort();
}
