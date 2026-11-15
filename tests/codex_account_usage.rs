//! Synthetic contracts only; never contacts a provider or uses existing credentials.
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    response::Response,
    routing::any,
};
use llm_proxy::{
    accounts::{Account, Credential, Store},
    adapters::outbound::codex::account_usage::CodexAccountUsage,
    authentication::AuthClient,
    domain::account_usage::{Metric, ResetOutcome, UsageError},
    ports::account_usage::AccountUsageProvider,
};
use serde_json::{Value, json};
use std::{collections::VecDeque, sync::Arc};
use tokio::{net::TcpListener, sync::Mutex, task::JoinHandle};

fn decode(value: Value) -> llm_proxy::domain::account_usage::UsageData {
    CodexAccountUsage::cached().decode_cached(&value).unwrap()
}

#[test]
fn cached_native_sse_retains_zero_balance_and_both_windows() {
    let usage = decode(json!({
        "type": "codex.rate_limits",
        "rate_limits": {
            "limit_reached": true,
            "primary": {"used_percent": 100, "limit_window_seconds": 18000, "reset_at": 2000000000u64},
            "secondary": {"used_percent": 23, "limit_window_minutes": 10080, "resets_at": 2000600000u64}
        },
        "credits": {"has_credits": false, "unlimited": false, "balance": "0.00"}
    }));
    let Metric::Known(windows) = usage.windows else {
        panic!("known windows")
    };
    assert_eq!(windows.len(), 2);
    assert_eq!(windows[1].duration_seconds, Some(604800));
    let Metric::Known(credits) = usage.prepaid_credits else {
        panic!("known credits")
    };
    assert_eq!(credits.balance, Metric::Known("0.00".into()));
    assert_eq!(usage.ordinary_usage_allowed, None);
    assert_eq!(usage.reset_credits, Metric::Unknown);
}

#[test]
fn cached_wham_keeps_authoritative_allowed_and_spend_block_independent() {
    let usage = decode(json!({
        "rate_limit": {"allowed": true, "limit_reached": true,
            "primary_window": {"used_percent": 120, "limit_window_seconds": 18000}},
        "additional_rate_limits": [{"limit_name": "codex_other", "metered_feature": "codex_other",
            "rate_limit": {"allowed": false, "primary_window": {"used_percent": 100}}}],
        "credits": {"has_credits": true, "unlimited": true},
        "spend_control": {"reached": true},
        "rate_limit_reset_credits": {"available_count": 0}
    }));
    assert_eq!(usage.ordinary_usage_allowed, Some(true));
    let Metric::Known(credits) = usage.prepaid_credits else {
        panic!("known credits")
    };
    assert_eq!(credits.balance, Metric::Unknown);
    assert_eq!(credits.unlimited, Some(true));
    assert_eq!(credits.spend_blocked, Some(true));
    let Metric::Known(reset) = usage.reset_credits else {
        panic!("known resets")
    };
    assert_eq!(reset.available, Metric::Known(0));
    let Metric::Known(windows) = usage.windows else {
        panic!("known windows")
    };
    assert_ne!(windows[0].scope, windows[1].scope);
}

#[test]
fn missing_metrics_are_unknown_not_zero_or_unsupported() {
    let usage = decode(json!({"plan_type": "pro", "rate_limit_reset_credits": {}}));
    assert_eq!(usage.windows, Metric::Unknown);
    assert_eq!(usage.prepaid_credits, Metric::Unknown);
    let Metric::Known(credits) = usage.reset_credits else {
        panic!("known summary")
    };
    assert_eq!(credits.available, Metric::Unknown);
    assert!(CodexAccountUsage::cached().capabilities().live_read);
    assert!(CodexAccountUsage::cached().capabilities().reset);
}

#[test]
fn explicit_negative_credit_balance_is_preserved_instead_of_reported_as_zero() {
    let usage = decode(json!({"credits": {"has_credits": false, "balance": "-12.50"}}));
    let Metric::Known(credits) = usage.prepaid_credits else {
        panic!("known credit details")
    };
    assert_eq!(credits.balance, Metric::Known("-12.50".into()));
}

#[test]
fn malformed_decimal_balances_do_not_become_zero_or_echo_input() {
    for balance in ["", "NaN", "1e9", "1.2.3", " 12", "12\nsecret", ".5"] {
        let error = CodexAccountUsage::cached()
            .decode_cached(&json!({"credits": {"balance": balance}}))
            .unwrap_err();
        assert_eq!(error, UsageError::Protocol);
        assert!(!error.to_string().contains("secret"));
    }
}

#[test]
fn malformed_shapes_and_bounded_cached_values_are_rejected() {
    for value in [
        json!({"error": {"message": "secret"}}),
        json!({"rate_limit": "secret"}),
        json!({"rate_limit_reset_credits": {"available_count": -1}}),
        json!({"credits": {"balance": "1".repeat(129)}}),
        json!({"additional_rate_limits": vec![json!({"rate_limit": null}); 33]}),
        json!({"credits": null, "extra": "x".repeat(4097)}),
        json!({"rate_limit_reset_credits": {"earliest_expiry": "2026-02-30T00:00:00Z"}}),
    ] {
        assert_eq!(
            CodexAccountUsage::cached()
                .decode_cached(&value)
                .unwrap_err(),
            UsageError::Protocol
        );
    }
}

#[derive(Clone)]
struct Recorded {
    method: String,
    path: String,
    headers: HeaderMap,
    body: Value,
}
struct Reply {
    status: StatusCode,
    body: String,
    location: Option<String>,
}
impl Reply {
    fn json(value: Value) -> Self {
        Self {
            status: StatusCode::OK,
            body: value.to_string(),
            location: None,
        }
    }
    fn error(status: StatusCode) -> Self {
        Self {
            status,
            body: "private-upstream-error-token".into(),
            location: None,
        }
    }
}
#[derive(Clone)]
struct MockState {
    replies: Arc<Mutex<VecDeque<Reply>>>,
    requests: Arc<Mutex<Vec<Recorded>>>,
}
struct Mock {
    endpoint: String,
    state: MockState,
    task: JoinHandle<()>,
}
impl Drop for Mock {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Mock {
    async fn start(replies: Vec<Reply>) -> Self {
        let state = MockState {
            replies: Arc::new(Mutex::new(replies.into())),
            requests: Arc::new(Mutex::new(Vec::new())),
        };
        async fn handler(State(state): State<MockState>, request: Request) -> Response {
            let (parts, body) = request.into_parts();
            let bytes = to_bytes(body, 65536).await.unwrap();
            state.requests.lock().await.push(Recorded {
                method: parts.method.to_string(),
                path: parts.uri.path().into(),
                headers: parts.headers,
                body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            });
            let reply = state
                .replies
                .lock()
                .await
                .pop_front()
                .unwrap_or_else(|| Reply::error(StatusCode::INTERNAL_SERVER_ERROR));
            let mut response = Response::builder().status(reply.status);
            if let Some(location) = reply.location {
                response = response.header("location", location);
            }
            response.body(Body::from(reply.body)).unwrap()
        }
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new()
            .fallback(any(handler))
            .with_state(state.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            endpoint,
            state,
            task,
        }
    }
}
struct Fixture {
    _directory: tempfile::TempDir,
    store: Store,
    account: Account,
    adapter: CodexAccountUsage,
}
fn fixture(server: &Mock) -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path()).unwrap();
    let account = store
        .login(
            "synthetic",
            Credential {
                access_token: "synthetic-access-token".into(),
                refresh_token: "synthetic-refresh-token".into(),
                id_token: None,
                expires_at: u64::MAX,
                subject: "oauth-subject-not-chatgpt-user".into(),
                account_id: "workspace-a".into(),
                generation: String::new(),
            },
        )
        .unwrap();
    let auth = AuthClient::with_issuer(server.endpoint.clone()).unwrap();
    let adapter =
        CodexAccountUsage::with_auth(store.clone(), server.endpoint.clone(), auth).unwrap();
    Fixture {
        _directory: directory,
        store,
        account,
        adapter,
    }
}
fn usage(count: Option<u64>) -> Value {
    let mut value = json!({
        "account_id": "workspace-a", "user_id": "chatgpt-user-different-namespace",
        "rate_limit": {"allowed": true, "primary_window": {"used_percent": 100}},
        "credits": {"balance": "1.25", "has_credits": true, "unlimited": false}
    });
    if let Some(count) = count {
        value["rate_limit_reset_credits"] = json!({"available_count": count});
    }
    value
}

#[tokio::test]
#[ignore = "requires synthetic localhost listener"]
async fn live_get_uses_selected_auth_and_preserves_count_when_details_fail() {
    let mut value = usage(Some(3));
    value["error"] = json!({"message": "private-upstream-error-token"});
    value["access_token"] = json!("private-upstream-error-token");
    value["rate_limit"]["unexpected"] = json!("private-upstream-error-token");
    let server = Mock::start(vec![
        Reply::json(value),
        Reply::error(StatusCode::INTERNAL_SERVER_ERROR),
    ])
    .await;
    let fixture = fixture(&server);
    let read = fixture.adapter.read(fixture.account).await.unwrap();
    let Metric::Known(credits) = &read.data.reset_credits else {
        panic!("known resets")
    };
    assert_eq!(credits.available, Metric::Known(3));
    assert_eq!(read.warning, Some(UsageError::Upstream));
    assert!(
        !read
            .snapshot
            .to_string()
            .contains("private-upstream-error-token")
    );
    assert_eq!(
        fixture.adapter.decode_cached(&read.snapshot).unwrap(),
        read.data
    );
    let requests = server.state.requests.lock().await;
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].path, "/wham/usage");
    assert_eq!(requests[1].path, "/wham/rate-limit-reset-credits");
    for request in requests.iter() {
        assert_eq!(request.method, "GET");
        assert_eq!(
            request.headers["authorization"],
            "Bearer synthetic-access-token"
        );
        assert_eq!(request.headers["chatgpt-account-id"], "workspace-a");
        assert!(!request.headers.contains_key("x-openai-codex-luna-reserve"));
    }
}

#[tokio::test]
#[ignore = "requires synthetic localhost listener"]
async fn known_zero_skips_details_and_does_not_infer_exhaustion() {
    let server = Mock::start(vec![Reply::json(usage(Some(0)))]).await;
    let fixture = fixture(&server);
    let read = fixture.adapter.read(fixture.account.clone()).await.unwrap();
    assert_eq!(read.data.ordinary_usage_allowed, Some(true));
    assert_eq!(read.warning, None);
    let lease = fixture.store.lease(&fixture.account.id).unwrap();
    assert_eq!(lease.account.status, fixture.account.status);
    assert_eq!(server.state.requests.lock().await.len(), 1);
}

#[tokio::test]
#[ignore = "requires synthetic localhost listener"]
async fn unknown_count_is_discovered_and_expiry_is_validated_and_ordered_in_utc() {
    let server = Mock::start(vec![
        Reply::json(usage(None)),
        Reply::json(json!({"available_count": 2, "credits": [
            {"status": "available", "expires_at": "2026-11-01T00:00:00-05:00", "title": "private-upstream-error-token"},
            {"status": "available", "expires_at": "2026-11-01T02:00:00Z"},
            {"status": "expired", "expires_at": "2025-01-01T00:00:00Z"},
            {"status": "future_status", "expires_at": null}
        ]})),
    ]).await;
    let fixture = fixture(&server);
    let read = fixture.adapter.read(fixture.account).await.unwrap();
    let Metric::Known(credits) = &read.data.reset_credits else {
        panic!("known resets")
    };
    assert_eq!(credits.available, Metric::Known(2));
    assert_eq!(
        credits.earliest_expiry.as_deref(),
        Some("2026-11-01T02:00:00Z")
    );
    assert!(
        !read
            .snapshot
            .to_string()
            .contains("private-upstream-error-token")
    );
    assert_eq!(
        fixture.adapter.decode_cached(&read.snapshot).unwrap(),
        read.data
    );
}

#[tokio::test]
#[ignore = "requires synthetic localhost listener"]
async fn malformed_details_keep_usage_count_and_report_warning() {
    let server = Mock::start(vec![
        Reply::json(usage(Some(4))),
        Reply::json(json!({"available_count": 4, "credits": [
            {"status": "available", "expires_at": "private-upstream-error-token"}
        ]})),
    ])
    .await;
    let fixture = fixture(&server);
    let read = fixture.adapter.read(fixture.account).await.unwrap();
    let Metric::Known(credits) = read.data.reset_credits else {
        panic!("known resets")
    };
    assert_eq!(credits.available, Metric::Known(4));
    assert_eq!(read.warning, Some(UsageError::Protocol));
}

#[tokio::test]
#[ignore = "requires synthetic localhost listener"]
async fn response_workspace_mismatch_is_rejected_before_credit_lookup() {
    let mut value = usage(Some(1));
    value["account_id"] = json!("wrong-workspace");
    let server = Mock::start(vec![Reply::json(value)]).await;
    let fixture = fixture(&server);
    assert_eq!(
        fixture.adapter.read(fixture.account).await.unwrap_err(),
        UsageError::Protocol
    );
    assert_eq!(server.state.requests.lock().await.len(), 1);
}

#[tokio::test]
#[ignore = "requires synthetic localhost listener"]
async fn malformed_and_oversized_http_bodies_return_only_static_errors() {
    let server = Mock::start(vec![
        Reply {
            status: StatusCode::OK,
            body: "private-upstream-error-token".into(),
            location: None,
        },
        Reply {
            status: StatusCode::OK,
            body: "x".repeat(256 * 1024 + 1),
            location: None,
        },
    ])
    .await;
    let fixture = fixture(&server);
    for _ in 0..2 {
        let error = fixture
            .adapter
            .read(fixture.account.clone())
            .await
            .unwrap_err();
        assert_eq!(error, UsageError::Protocol);
        assert!(!format!("{error:?} {error}").contains("private-upstream-error-token"));
    }
}

#[tokio::test]
#[ignore = "requires synthetic localhost listener"]
async fn redirects_are_not_followed_with_credentials() {
    let server = Mock::start(vec![Reply {
        status: StatusCode::TEMPORARY_REDIRECT,
        body: String::new(),
        location: Some("/credential-leak-target".into()),
    }])
    .await;
    let fixture = fixture(&server);
    assert_eq!(
        fixture.adapter.read(fixture.account).await.unwrap_err(),
        UsageError::Upstream
    );
    assert_eq!(server.state.requests.lock().await.len(), 1);
}

#[tokio::test]
#[ignore = "requires synthetic localhost listener"]
async fn consume_sends_exact_uuid_once_and_maps_each_known_result() {
    let server = Mock::start(vec![
        Reply::json(json!({"code": "reset", "windows_reset": 2})),
        Reply::json(json!({"code": "nothing_to_reset"})),
        Reply::json(json!({"code": "no_credit"})),
        Reply::json(json!({"code": "already_redeemed"})),
    ])
    .await;
    let fixture = fixture(&server);
    let request_id = uuid::Uuid::new_v4().to_string();
    for expected in [
        ResetOutcome::Reset { windows_reset: 2 },
        ResetOutcome::NothingToReset,
        ResetOutcome::NoCredit,
        ResetOutcome::AlreadyRedeemed,
    ] {
        assert_eq!(
            fixture
                .adapter
                .reset(fixture.account.clone(), request_id.clone())
                .await
                .unwrap(),
            expected
        );
    }
    let requests = server.state.requests.lock().await;
    assert_eq!(requests.len(), 4);
    for request in requests.iter() {
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/wham/rate-limit-reset-credits/consume");
        assert_eq!(request.body, json!({"redeem_request_id": request_id}));
        assert_eq!(
            request.headers["authorization"],
            "Bearer synthetic-access-token"
        );
        assert_eq!(request.headers["chatgpt-account-id"], "workspace-a");
    }
}

#[tokio::test]
#[ignore = "requires synthetic localhost listener"]
async fn consume_401_unknown_and_malformed_results_are_dispatched_without_retry() {
    let server = Mock::start(vec![
        Reply::error(StatusCode::UNAUTHORIZED),
        Reply::json(json!({"code": "private-upstream-error-token"})),
        Reply::json(json!({"code": "reset", "windows_reset": -1})),
    ])
    .await;
    let fixture = fixture(&server);
    for expected in [
        UsageError::Authentication,
        UsageError::Protocol,
        UsageError::Protocol,
    ] {
        let error = fixture
            .adapter
            .reset(fixture.account.clone(), uuid::Uuid::new_v4().to_string())
            .await
            .unwrap_err();
        assert!(error.dispatched);
        assert_eq!(error.error, expected);
        assert!(!format!("{error:?}").contains("private-upstream-error-token"));
    }
    let requests = server.state.requests.lock().await;
    assert_eq!(requests.len(), 3);
    assert!(requests.iter().all(|request| request.method == "POST"));
}

#[tokio::test]
#[ignore = "requires synthetic localhost listener"]
async fn changed_account_or_invalid_uuid_never_dispatches_consume() {
    let server = Mock::start(vec![]).await;
    let fixture = fixture(&server);
    let error = fixture
        .adapter
        .reset(fixture.account.clone(), "invalid-uuid".into())
        .await
        .unwrap_err();
    assert!(!error.dispatched);
    fixture.store.remove(&fixture.account.id).unwrap();
    let error = fixture
        .adapter
        .reset(fixture.account, uuid::Uuid::new_v4().to_string())
        .await
        .unwrap_err();
    assert!(!error.dispatched);
    assert_eq!(error.error, UsageError::AccountChanged);
    assert!(server.state.requests.lock().await.is_empty());
}

#[tokio::test]
#[ignore = "requires synthetic localhost listener"]
async fn verified_equivalent_user_namespace_rejects_user_mismatch() {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    let server = Mock::start(vec![Reply::json(usage(Some(0)))]).await;
    let mut fixture = fixture(&server);
    let mut credential = fixture.store.lease(&fixture.account.id).unwrap().credential;
    credential.access_token = format!(
        "e30.{}.signature",
        URL_SAFE_NO_PAD.encode(
            json!({
                "https://api.openai.com/auth": {"chatgpt_user_id": credential.subject}
            })
            .to_string()
        )
    );
    fixture.account = fixture.store.login("synthetic", credential).unwrap();
    assert_eq!(
        fixture.adapter.read(fixture.account).await.unwrap_err(),
        UsageError::Protocol
    );
}

#[tokio::test]
#[ignore = "requires synthetic localhost listener"]
async fn expiry_comparison_retains_subsecond_precision() {
    let server = Mock::start(vec![
        Reply::json(usage(None)),
        Reply::json(json!({"available_count": 2, "credits": [
            {"status": "available", "expires_at": "2026-11-01T00:00:00.2Z"},
            {"status": "available", "expires_at": "2026-11-01T00:00:00.11Z"}
        ]})),
    ])
    .await;
    let fixture = fixture(&server);
    let read = fixture.adapter.read(fixture.account).await.unwrap();
    let Metric::Known(credits) = read.data.reset_credits else {
        panic!("known resets")
    };
    assert_eq!(
        credits.earliest_expiry.as_deref(),
        Some("2026-11-01T00:00:00.11Z")
    );
}

#[tokio::test]
#[ignore = "requires synthetic localhost listener"]
async fn consume_transport_failure_is_uncertain_and_not_retried() {
    let mut server = Mock::start(vec![]).await;
    let fixture = fixture(&server);
    server.task.abort();
    // Await server cancellation so its synthetic listener is definitely closed.
    assert!((&mut server.task).await.unwrap_err().is_cancelled());
    let error = fixture
        .adapter
        .reset(fixture.account, uuid::Uuid::new_v4().to_string())
        .await
        .unwrap_err();
    assert!(error.dispatched);
    assert_eq!(error.error, UsageError::Transport);
    assert!(server.state.requests.lock().await.is_empty());
}
