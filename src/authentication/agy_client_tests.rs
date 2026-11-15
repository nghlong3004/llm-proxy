//! Synthetic reference and OAuth endpoints only; never accesses installed credentials.
use super::*;
use crate::{
    accounts::{AccountStatus, Credential, Store},
    authentication::AuthClient,
};
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    response::Response,
    routing::any,
};
use std::{sync::Mutex, time::Duration};

type Capture = (String, HeaderMap, Vec<u8>);
const DATA: &str = "// data, never evaluated\nexport const OAUTH_CONFIG = {\n    clientId: 'fixture-client-id',\n    clientSecret: 'fixture-client-secret',\n};\nthrow new Error('must never execute');\n";

struct Mock {
    url: String,
    calls: Arc<Mutex<Vec<Capture>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Mock {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Mock {
    async fn start(status: StatusCode, bytes: Vec<u8>) -> Self {
        #[derive(Clone)]
        struct MockState {
            status: StatusCode,
            bytes: Vec<u8>,
            calls: Arc<Mutex<Vec<Capture>>>,
        }
        async fn handler(State(state): State<MockState>, request: Request) -> Response {
            let (parts, body) = request.into_parts();
            let bytes = to_bytes(body, 65536).await.unwrap().to_vec();
            state
                .calls
                .lock()
                .unwrap()
                .push((parts.uri.path().into(), parts.headers, bytes));
            if parts.uri.path() == "/token" {
                return Response::builder().header("content-type", "application/json")
                    .body(Body::from(r#"{"access_token":"fixture-renewed","refresh_token":"fixture-rotated","expires_in":3600}"#)).unwrap();
            }
            Response::builder()
                .status(state.status)
                .header("location", "/unexpected")
                .body(Body::from(state.bytes))
                .unwrap()
        }
        let calls = Arc::new(Mutex::new(Vec::new()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new().fallback(any(handler)).with_state(MockState {
            status,
            bytes,
            calls: calls.clone(),
        });
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self { url, calls, task }
    }
    fn reference(&self) -> Reference {
        Reference {
            url: format!("{}/reference", self.url),
            digest: Sha256::digest(DATA.as_bytes()).into(),
            revision: "fixture-revision".into(),
        }
    }
    fn source(&self, root: &Path) -> ClientSource {
        ClientSource {
            source: Arc::new(Source::Reference {
                root: root.to_owned(),
                reference: self.reference(),
            }),
            resolved: Arc::new(OnceCell::new()),
        }
    }
}
fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}
fn credential() -> Credential {
    Credential {
        access_token: "fixture-old-access".into(),
        refresh_token: "fixture-refresh".into(),
        id_token: None,
        expires_at: 0,
        subject: "fixture-user".into(),
        account_id: "fixture-project".into(),
        generation: String::new(),
    }
}

#[test]
fn extraction_is_data_only_and_rejects_ambiguous_or_malformed_fields() {
    let client = parse(DATA.as_bytes()).unwrap();
    assert_eq!(
        client.pair(),
        ("fixture-client-id", "fixture-client-secret")
    );
    for data in [
        DATA.replace(
            "clientId: 'fixture-client-id',",
            "clientId: 'one',\nclientId: 'two',",
        ),
        DATA.replace("clientSecret: 'fixture-client-secret',", ""),
        DATA.replace("'fixture-client-id'", "getClient()"),
        DATA.replace("'fixture-client-secret'", "'fixture\\secret'"),
        format!("{DATA}\n{DATA}"),
    ] {
        let error = parse(data.as_bytes()).err().unwrap().to_string();
        assert!(!error.contains("fixture-client-secret"));
    }
}

#[test]
fn verification_rejects_different_bytes_before_parsing() {
    let reference = Reference {
        url: "unused".into(),
        digest: Sha256::digest(DATA.as_bytes()).into(),
        revision: "fixture".into(),
    };
    assert!(verify(DATA.as_bytes(), &reference).is_ok());
    let error = verify(b"untrusted replacement", &reference)
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("integrity"));
    assert!(!error.contains("untrusted replacement"));
    assert!(verify(&vec![b'x'; MAX_REFERENCE_BYTES + 1], &reference).is_err());
}

#[tokio::test]
async fn complete_override_wins_and_partial_empty_override_never_fetches_or_creates_cache() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("must-not-create");
    let client = http();
    let source = ClientSource::with_override(
        root.clone(),
        Some("override-id".into()),
        Some("override-secret".into()),
    );
    assert_eq!(
        source.resolve(&client).await.unwrap().pair(),
        ("override-id", "override-secret")
    );
    for (id, secret) in [
        (Some("override-id".into()), None),
        (None, Some("override-secret".into())),
        (Some("".into()), Some("override-secret".into())),
    ] {
        let source = ClientSource::with_override(root.clone(), id, secret);
        let error = source.resolve(&client).await.err().unwrap().to_string();
        assert!(error.contains("Set both"));
        assert!(!error.contains("override-secret"));
    }
    let absent = ClientSource::with_override(root.clone(), None, None);
    assert!(matches!(absent.source.as_ref(), Source::Reference { .. }));
    assert!(!root.exists());
}

#[tokio::test]
async fn fetch_is_single_flight_private_and_reused_offline_without_outgoing_credentials() {
    let mock = Mock::start(StatusCode::OK, DATA.as_bytes().to_vec()).await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("private-state");
    let source = mock.source(&root);
    assert!(!root.exists(), "construction must be lazy");
    let mut jobs = Vec::new();
    for _ in 0..8 {
        let source = source.clone();
        jobs.push(tokio::spawn(async move {
            assert_eq!(
                source.resolve(&http()).await.unwrap().pair().0,
                "fixture-client-id"
            );
        }));
    }
    for job in jobs {
        job.await.unwrap();
    }
    {
        let calls = mock.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "/reference");
        assert!(!calls[0].1.contains_key("authorization"));
        assert!(!calls[0].1.contains_key("cookie"));
        assert!(!calls[0].1.contains_key("chatgpt-account-id"));
        assert!(calls[0].2.is_empty());
    }
    let directory = root.join("agy-oauth-client/fixture-revision");
    assert_eq!(
        std::fs::read(directory.join("reference.txt")).unwrap(),
        DATA.as_bytes()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for path in [&root, &root.join("agy-oauth-client"), &directory] {
            assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, 0o700);
        }
        for path in [
            directory.join("reference.txt"),
            directory.join("cache.lock"),
        ] {
            assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, 0o600);
        }
    }
    mock.task.abort();
    let source = mock.source(&root);
    assert_eq!(
        source.resolve(&http()).await.unwrap().pair().1,
        "fixture-client-secret"
    );
}

#[tokio::test]
async fn separate_sources_converge_on_one_verified_atomic_cache() {
    let mock = Mock::start(StatusCode::OK, DATA.as_bytes().to_vec()).await;
    let dir = tempfile::tempdir().unwrap();
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let source = mock.source(dir.path());
        tasks.push(tokio::spawn(async move {
            source.resolve(&http()).await.unwrap().pair().0.to_owned()
        }));
    }
    for task in tasks {
        assert_eq!(task.await.unwrap(), "fixture-client-id");
    }
    let directory = dir.path().join("agy-oauth-client/fixture-revision");
    assert_eq!(
        std::fs::read(directory.join("reference.txt")).unwrap(),
        DATA.as_bytes()
    );
    assert!(std::fs::read_dir(directory).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".tmp-")
    }));
}

#[tokio::test]
async fn corrupt_cache_fails_closed_without_redownload_or_oauth() {
    let mock = Mock::start(StatusCode::OK, DATA.as_bytes().to_vec()).await;
    let dir = tempfile::tempdir().unwrap();
    mock.source(dir.path()).resolve(&http()).await.unwrap();
    let path = dir
        .path()
        .join("agy-oauth-client/fixture-revision/reference.txt");
    std::fs::write(&path, "fixture-cache-tamper-secret").unwrap();
    let source = mock.source(dir.path());
    let error = source.resolve(&http()).await.err().unwrap().to_string();
    assert!(error.contains("integrity"));
    assert!(!error.contains("fixture-cache-tamper-secret"));
    assert_eq!(mock.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn unsuccessful_redirected_oversized_and_untrusted_downloads_are_not_cached() {
    for (status, data) in [
        (
            StatusCode::SERVICE_UNAVAILABLE,
            b"fixture-upstream-secret".to_vec(),
        ),
        (StatusCode::FOUND, DATA.as_bytes().to_vec()),
        (StatusCode::OK, vec![b'x'; MAX_REFERENCE_BYTES + 1]),
        (StatusCode::OK, b"fixture-upstream-secret".to_vec()),
    ] {
        let mock = Mock::start(status, data).await;
        let dir = tempfile::tempdir().unwrap();
        let source = mock.source(dir.path());
        let error = source.resolve(&http()).await.err().unwrap().to_string();
        assert!(!error.contains("fixture-upstream-secret"));
        assert!(
            !dir.path()
                .join("agy-oauth-client/fixture-revision/reference.txt")
                .exists()
        );
        assert_eq!(mock.calls.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn transport_failure_is_redacted_and_never_publishes_a_cache() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/reference", listener.local_addr().unwrap());
    drop(listener);
    let dir = tempfile::tempdir().unwrap();
    let reference = Reference {
        url: url.clone(),
        digest: [0; 32],
        revision: "fixture-revision".into(),
    };
    let error = load_reference(&http(), dir.path(), &reference)
        .await
        .err()
        .unwrap()
        .to_string();
    assert!(!error.contains(&url));
    assert!(
        !dir.path()
            .join("agy-oauth-client/fixture-revision/reference.txt")
            .exists()
    );
}

#[tokio::test]
async fn streamed_body_without_content_length_is_still_bounded() {
    async fn oversized() -> Response {
        let chunks = futures_util::stream::iter((0..9).map(|_| {
            Ok::<_, std::convert::Infallible>(axum::body::Bytes::from(vec![b'x'; 16 * 1024]))
        }));
        Response::new(Body::from_stream(chunks))
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/reference", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, Router::new().fallback(any(oversized)))
            .await
            .unwrap();
    });
    let dir = tempfile::tempdir().unwrap();
    let reference = Reference {
        url,
        digest: [0; 32],
        revision: "fixture-revision".into(),
    };
    let error = load_reference(&http(), dir.path(), &reference)
        .await
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("size limit"));
    assert!(
        !dir.path()
            .join("agy-oauth-client/fixture-revision/reference.txt")
            .exists()
    );
    task.abort();
}

#[tokio::test]
async fn acquisition_failure_does_not_disable_account_or_reach_oauth_or_browser() {
    let mock = Mock::start(StatusCode::OK, b"fixture-untrusted-secret".to_vec()).await;
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let account = store.login_for("agy", "fixture", credential()).unwrap();
    let auth = AuthClient::with_agy_source(
        format!("{}/token", mock.url),
        format!("{}/userinfo", mock.url),
        mock.url.clone(),
        mock.source(store.root()),
    )
    .unwrap();
    let error = auth.agy_browser_login().await.err().unwrap().to_string();
    assert!(error.contains("integrity"));
    let result = auth
        .refresh_for_generation(&store, &account.id, &account.generation, None)
        .await;
    assert!(result.is_err());
    assert_eq!(
        store.list().unwrap().accounts[0].status,
        AccountStatus::Available
    );
    assert!(
        mock.calls
            .lock()
            .unwrap()
            .iter()
            .all(|call| call.0 == "/reference")
    );
    auth.shutdown().await;
}

#[tokio::test]
async fn shutdown_during_reference_acquisition_does_not_start_token_rotation() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::Notify;
    #[derive(Clone, Default)]
    struct Pending {
        arrived: Arc<Notify>,
        release: Arc<Notify>,
        tokens: Arc<AtomicUsize>,
    }
    async fn handler(State(state): State<Pending>, request: Request) -> Response {
        if request.uri().path() == "/reference" {
            state.arrived.notify_one();
            state.release.notified().await;
            return Response::new(Body::from(DATA));
        }
        state.tokens.fetch_add(1, Ordering::SeqCst);
        Response::new(Body::from(
            r#"{"access_token":"fixture-renewed","expires_in":3600}"#,
        ))
    }
    let pending = Pending::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new()
        .fallback(any(handler))
        .with_state(pending.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let account = store.login_for("agy", "fixture", credential()).unwrap();
    let source = ClientSource {
        source: Arc::new(Source::Reference {
            root: store.root().to_owned(),
            reference: Reference {
                url: format!("{url}/reference"),
                digest: Sha256::digest(DATA.as_bytes()).into(),
                revision: "fixture-revision".into(),
            },
        }),
        resolved: Arc::new(OnceCell::new()),
    };
    let auth = AuthClient::with_agy_source(
        format!("{url}/token"),
        format!("{url}/userinfo"),
        url,
        source,
    )
    .unwrap();
    let caller_auth = auth.clone();
    let caller_store = store.clone();
    let id = account.id.clone();
    let generation = account.generation.clone();
    let caller = tokio::spawn(async move {
        caller_auth
            .refresh_for_generation(&caller_store, &id, &generation, None)
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), pending.arrived.notified())
        .await
        .unwrap();
    let closing = auth.clone();
    let shutdown = tokio::spawn(async move {
        closing.shutdown().await;
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while auth.ensure_refresh_open().is_ok() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    pending.release.notify_one();
    let result = tokio::time::timeout(Duration::from_secs(5), caller)
        .await
        .unwrap()
        .unwrap();
    assert!(result.is_err());
    tokio::time::timeout(Duration::from_secs(5), shutdown)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pending.tokens.load(Ordering::SeqCst), 0);
    assert_eq!(
        store.list().unwrap().accounts[0].status,
        AccountStatus::Available
    );
    task.abort();
}

#[tokio::test]
async fn refresh_uses_resolved_client_and_cache_without_changing_account_identity() {
    let mock = Mock::start(StatusCode::OK, DATA.as_bytes().to_vec()).await;
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let account = store.login_for("agy", "fixture", credential()).unwrap();
    let auth = AuthClient::with_agy_source(
        format!("{}/token", mock.url),
        format!("{}/userinfo", mock.url),
        mock.url.clone(),
        mock.source(store.root()),
    )
    .unwrap();
    let refreshed = auth
        .refresh_for_generation(&store, &account.id, &account.generation, None)
        .await
        .unwrap();
    assert_eq!(refreshed.subject, account.subject);
    assert_eq!(refreshed.account_id, account.workspace);
    assert_eq!(refreshed.generation, account.generation);
    assert_eq!(refreshed.refresh_token, "fixture-rotated");
    {
        let calls = mock.calls.lock().unwrap();
        assert_eq!(
            calls.iter().map(|call| call.0.as_str()).collect::<Vec<_>>(),
            ["/reference", "/token"]
        );
        assert!(!calls[0].1.contains_key("authorization"));
        let form: std::collections::HashMap<_, _> = url::form_urlencoded::parse(&calls[1].2)
            .into_owned()
            .collect();
        assert_eq!(form["client_id"], "fixture-client-id");
        assert_eq!(form["client_secret"], "fixture-client-secret");
    }
    auth.shutdown().await;
}
