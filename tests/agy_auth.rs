//! Synthetic proxy-owned Google OAuth contracts; no live authentication or native credentials.
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{Request, State},
    http::StatusCode,
    response::Response,
    routing::any,
};
use llm_proxy::{
    accounts::{Account, AccountStatus, Credential, Store},
    authentication::{AuthClient, RefreshError},
};
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::{
    sync::{Mutex, Semaphore},
    task::JoinHandle,
};

#[derive(Clone)]
struct MockState {
    status: StatusCode,
    body: String,
    hold: bool,
    arrived: Arc<Semaphore>,
    release: Arc<Semaphore>,
    forms: Arc<Mutex<Vec<HashMap<String, String>>>>,
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
    async fn start(status: StatusCode, body: Value, hold: bool) -> Self {
        let state = MockState {
            status,
            body: body.to_string(),
            hold,
            arrived: Arc::new(Semaphore::new(0)),
            release: Arc::new(Semaphore::new(0)),
            forms: Arc::new(Mutex::new(Vec::new())),
        };
        async fn handler(State(state): State<MockState>, request: Request) -> Response {
            assert_eq!(request.uri().path(), "/token");
            let bytes = to_bytes(request.into_body(), 65536).await.unwrap();
            let form = url::form_urlencoded::parse(&bytes).into_owned().collect();
            state.forms.lock().await.push(form);
            state.arrived.add_permits(1);
            if state.hold {
                state.release.acquire().await.unwrap().forget();
            }
            Response::builder()
                .status(state.status)
                .body(Body::from(state.body))
                .unwrap()
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new()
            .fallback(any(handler))
            .with_state(state.clone());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            endpoint,
            state,
            task,
        }
    }
    fn auth(&self) -> AuthClient {
        AuthClient::with_agy_endpoints(
            format!("{}/token", self.endpoint),
            format!("{}/userinfo", self.endpoint),
            self.endpoint.clone(),
            "synthetic-client-id".into(),
            "synthetic-client-secret".into(),
        )
        .unwrap()
    }
    async fn arrived(&self) {
        tokio::time::timeout(Duration::from_secs(3), self.state.arrived.acquire())
            .await
            .unwrap()
            .unwrap()
            .forget();
    }
}
fn credential(expires_at: u64) -> Credential {
    Credential {
        access_token: "synthetic-access".into(),
        refresh_token: "synthetic-refresh".into(),
        id_token: None,
        expires_at,
        subject: "stable-google-id".into(),
        account_id: "managed-project".into(),
        generation: String::new(),
    }
}
fn account(store: &Store, expires_at: u64) -> Account {
    store
        .login_for("agy", "synthetic", credential(expires_at))
        .unwrap()
}

#[test]
fn injection_rejects_non_https_remote_and_url_secrets() {
    for endpoint in [
        "http://example.com/token",
        "https://user:secret@example.com/token",
        "https://example.com/token?token=secret",
        "https://example.com/token#secret",
    ] {
        let error = AuthClient::with_agy_endpoints(
            endpoint.into(),
            "https://example.com/userinfo".into(),
            "https://example.com".into(),
            "synthetic-client-id".into(),
            "synthetic-client-secret".into(),
        )
        .err()
        .unwrap();
        assert!(!format!("{error:#}").contains("secret"));
    }
    assert!(AuthClient::agy().is_ok());
}

#[tokio::test]
async fn fresh_credentials_and_other_callers_rotation_need_no_network() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let selected = account(&store, u64::MAX);
    let auth = AuthClient::with_agy_endpoints(
        "http://127.0.0.1:1/token".into(),
        "http://127.0.0.1:1/userinfo".into(),
        "http://127.0.0.1:1".into(),
        "synthetic-client-id".into(),
        "synthetic-client-secret".into(),
    )
    .unwrap();
    let fresh = auth
        .refresh_for_generation(&store, &selected.id, &selected.generation, None)
        .await
        .unwrap();
    assert_eq!(fresh.access_token, "synthetic-access");
    let rotated_elsewhere = auth
        .refresh_for_generation(
            &store,
            &selected.id,
            &selected.generation,
            Some("older-access"),
        )
        .await
        .unwrap();
    assert_eq!(rotated_elsewhere.generation, selected.generation);
    auth.shutdown().await;
}

#[tokio::test]
async fn missing_oauth_client_configuration_fails_before_login_or_refresh_dispatch() {
    for (client_id, client_secret) in [
        ("", "synthetic-secret-do-not-display"),
        ("synthetic-id", ""),
    ] {
        let auth = AuthClient::with_agy_endpoints(
            "http://127.0.0.1:1/token".into(),
            "http://127.0.0.1:1/userinfo".into(),
            "http://127.0.0.1:1".into(),
            client_id.into(),
            client_secret.into(),
        )
        .unwrap();
        let login_error = auth.agy_browser_login().await.err().unwrap();
        let message = format!("{login_error:#}");
        assert!(message.contains("LLM_PROXY_AGY_CLIENT_ID"));
        assert!(!message.contains("synthetic-secret-do-not-display"));
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let selected = account(&store, 0);
        let error = auth
            .refresh_for_generation(&store, &selected.id, &selected.generation, None)
            .await
            .err()
            .unwrap();
        assert!(matches!(error, RefreshError::Other(_)));
        assert_eq!(
            store.list().unwrap().accounts[0].status,
            AccountStatus::Available
        );
        auth.shutdown().await;
    }
}

#[tokio::test]
async fn refresh_rotates_and_durably_preserves_google_user_project_and_generation() {
    let mock = Mock::start(StatusCode::OK, json!({
        "access_token": "rotated-access", "refresh_token": "rotated-refresh", "expires_in": 3600,
        "id_token": "not-an-identity-source"
    }), false).await;
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let selected = account(&store, 0);
    let auth = mock.auth();
    let refreshed = auth
        .refresh_for_generation(&store, &selected.id, &selected.generation, None)
        .await
        .unwrap();
    assert_eq!(refreshed.subject, "stable-google-id");
    assert_eq!(refreshed.account_id, "managed-project");
    assert_eq!(refreshed.generation, selected.generation);
    assert_eq!(refreshed.refresh_token, "rotated-refresh");
    assert!(refreshed.id_token.is_none());
    assert_eq!(
        store.lease(&selected.id).unwrap().credential.access_token,
        "rotated-access"
    );
    let forms = mock.state.forms.lock().await;
    assert_eq!(forms.len(), 1);
    assert_eq!(forms[0]["grant_type"], "refresh_token");
    assert_eq!(forms[0]["refresh_token"], "synthetic-refresh");
    assert_eq!(forms[0]["client_id"], "synthetic-client-id");
    assert_eq!(forms[0]["client_secret"], "synthetic-client-secret");
    drop(forms);
    auth.shutdown().await;
}

#[tokio::test]
async fn missing_rotated_refresh_token_retains_previous_token() {
    let mock = Mock::start(
        StatusCode::OK,
        json!({"access_token": "new-access", "expires_in": 3600}),
        false,
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let selected = account(&store, 0);
    let auth = mock.auth();
    let refreshed = auth.refresh(&store, &selected.id, None).await.unwrap();
    assert_eq!(refreshed.refresh_token, "synthetic-refresh");
    auth.shutdown().await;
}

#[tokio::test]
async fn invalid_grant_and_invalid_token_revoke_only_current_account() {
    for code in ["invalid_grant", "invalid_token"] {
        let mock = Mock::start(
            StatusCode::BAD_REQUEST,
            json!({"error": code, "error_description": "private-body-secret"}),
            false,
        )
        .await;
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let selected = account(&store, 0);
        let auth = mock.auth();
        let error = auth
            .refresh(&store, &selected.id, None)
            .await
            .err()
            .unwrap();
        assert!(matches!(error, RefreshError::Revoked));
        assert!(!format!("{error:?}").contains("private-body-secret"));
        assert_eq!(
            store.lease(&selected.id).unwrap().account.status,
            AccountStatus::NeedsLogin
        );
        auth.shutdown().await;
    }
}

#[tokio::test]
async fn generic_google_400_and_invalid_client_are_not_account_revocation() {
    for body in [
        json!({"error": "invalid_client", "error_description": "private-body-secret"}),
        json!({"error": "invalid_request", "error_description": "private-body-secret"}),
        json!({"error": {"message": "private-body-secret"}}),
    ] {
        let mock = Mock::start(StatusCode::BAD_REQUEST, body, false).await;
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let selected = account(&store, 0);
        let auth = mock.auth();
        let error = auth
            .refresh(&store, &selected.id, None)
            .await
            .err()
            .unwrap();
        assert!(matches!(error, RefreshError::Other(_)));
        assert!(!format!("{error:?}").contains("private-body-secret"));
        assert_eq!(
            store.lease(&selected.id).unwrap().account.status,
            AccountStatus::Available
        );
        auth.shutdown().await;
    }
}

#[tokio::test]
async fn provider_mismatch_is_rejected_even_when_credential_is_fresh() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let codex = store.login("synthetic", credential(u64::MAX)).unwrap();
    let agy = account(&store, u64::MAX);
    let google = AuthClient::agy().unwrap();
    let openai = AuthClient::with_issuer("http://127.0.0.1:1".into()).unwrap();
    assert!(matches!(
        google.refresh(&store, &codex.id, None).await,
        Err(RefreshError::Other(_))
    ));
    assert!(matches!(
        openai.refresh(&store, &agy.id, None).await,
        Err(RefreshError::Other(_))
    ));
    google.shutdown().await;
    openai.shutdown().await;
}

#[tokio::test]
async fn stale_generation_is_rejected_without_network() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let old = account(&store, 0);
    let new = account(&store, u64::MAX);
    let auth = AuthClient::agy().unwrap();
    let error = auth
        .refresh_for_generation(&store, &old.id, &old.generation, None)
        .await
        .err()
        .unwrap();
    assert!(matches!(error, RefreshError::AccountChanged));
    assert_eq!(
        store.lease(&new.id).unwrap().credential.generation,
        new.generation
    );
    auth.shutdown().await;
}

#[tokio::test]
async fn late_rotation_does_not_overwrite_new_login() {
    let mock = Mock::start(
        StatusCode::OK,
        json!({
            "access_token": "stale-rotated-access", "refresh_token": "stale-rotated-refresh",
            "expires_in": 3600
        }),
        true,
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let old = account(&store, 0);
    let auth = mock.auth();
    let caller = {
        let auth = auth.clone();
        let store = store.clone();
        let old = old.clone();
        tokio::spawn(async move {
            auth.refresh_for_generation(&store, &old.id, &old.generation, None)
                .await
        })
    };
    mock.arrived().await;
    let new = account(&store, u64::MAX);
    mock.state.release.add_permits(1);
    assert!(matches!(
        caller.await.unwrap(),
        Err(RefreshError::AccountChanged)
    ));
    let current = store.lease(&new.id).unwrap().credential;
    assert_eq!(current.generation, new.generation);
    assert_eq!(current.access_token, "synthetic-access");
    auth.shutdown().await;
}

#[tokio::test]
async fn late_invalid_grant_cannot_disable_new_login() {
    let mock = Mock::start(
        StatusCode::BAD_REQUEST,
        json!({"error": "invalid_grant"}),
        true,
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let old = account(&store, 0);
    let auth = mock.auth();
    let caller = {
        let auth = auth.clone();
        let store = store.clone();
        let id = old.id.clone();
        tokio::spawn(async move { auth.refresh(&store, &id, None).await })
    };
    mock.arrived().await;
    let new = account(&store, u64::MAX);
    mock.state.release.add_permits(1);
    assert!(matches!(
        caller.await.unwrap(),
        Err(RefreshError::AccountChanged)
    ));
    assert_eq!(
        store.lease(&new.id).unwrap().account.status,
        AccountStatus::Available
    );
    auth.shutdown().await;
}

#[tokio::test]
async fn cancelled_caller_keeps_crossprocess_lock_until_rotation_is_persisted_and_drained() {
    let mock = Mock::start(StatusCode::OK, json!({
        "access_token": "cancelled-rotated-access", "refresh_token": "cancelled-rotated-refresh",
        "expires_in": 3600
    }), true).await;
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let selected = account(&store, 0);
    let auth = mock.auth();
    let caller = {
        let auth = auth.clone();
        let store = store.clone();
        let id = selected.id.clone();
        tokio::spawn(async move { auth.refresh(&store, &id, None).await })
    };
    mock.arrived().await;
    caller.abort();
    assert!(caller.await.err().unwrap().is_cancelled());
    // A separate Store observes the same filesystem lock, not merely a client mutex.
    let other_store = Store::open(dir.path()).unwrap();
    assert!(
        other_store
            .try_refresh_lock(&selected.id)
            .unwrap()
            .is_none()
    );
    let drain = {
        let auth = auth.clone();
        tokio::spawn(async move { auth.shutdown().await })
    };
    tokio::task::yield_now().await;
    assert!(!drain.is_finished());
    mock.state.release.add_permits(1);
    tokio::time::timeout(Duration::from_secs(3), drain)
        .await
        .unwrap()
        .unwrap();
    let saved = other_store.lease(&selected.id).unwrap().credential;
    assert_eq!(saved.refresh_token, "cancelled-rotated-refresh");
    assert_eq!(saved.access_token, "cancelled-rotated-access");
    assert!(
        other_store
            .try_refresh_lock(&selected.id)
            .unwrap()
            .is_some()
    );
    assert!(auth.refresh(&store, &selected.id, None).await.is_err());
}

#[tokio::test]
async fn independent_clients_single_flight_under_filesystem_lock() {
    let mock = Mock::start(
        StatusCode::OK,
        json!({
            "access_token": "single-access", "refresh_token": "single-refresh", "expires_in": 3600
        }),
        true,
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let selected = account(&store, 0);
    let first_auth = mock.auth();
    let second_auth = mock.auth();
    let first = {
        let auth = first_auth.clone();
        let store = store.clone();
        let id = selected.id.clone();
        tokio::spawn(async move { auth.refresh(&store, &id, None).await })
    };
    mock.arrived().await;
    let second = {
        let auth = second_auth.clone();
        let store = Store::open(dir.path()).unwrap();
        let id = selected.id.clone();
        tokio::spawn(async move { auth.refresh(&store, &id, None).await })
    };
    mock.state.release.add_permits(1);
    assert_eq!(first.await.unwrap().unwrap().access_token, "single-access");
    assert_eq!(second.await.unwrap().unwrap().access_token, "single-access");
    assert_eq!(mock.state.forms.lock().await.len(), 1);
    first_auth.shutdown().await;
    second_auth.shutdown().await;
}

#[tokio::test]
async fn malformed_success_response_does_not_echo_token_body() {
    let mock = Mock::start(
        StatusCode::OK,
        json!({
            "access_token": "private-body-secret", "refresh_token": "private-body-secret",
            "expires_in": "private-body-secret"
        }),
        false,
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let selected = account(&store, 0);
    let auth = mock.auth();
    let error = auth
        .refresh(&store, &selected.id, None)
        .await
        .err()
        .unwrap();
    assert!(!format!("{error:?}").contains("private-body-secret"));
    auth.shutdown().await;
}
