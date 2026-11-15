//! Native AGY Google OAuth, pinned to upstream 7c672fdc6fa62fcd50e8f75df5644dad6a63c2dc.
//! Client configuration is acquired lazily from a verified public reference or explicit overrides.
use super::{
    agy_client::ClientSource,
    callback::{Callback, oauth_callback},
    oauth::{AuthClient, Flavor, RefreshError, read_json},
    tokens::Tokens,
};
use crate::accounts::{Credential, now};
use anyhow::{Context, Result, bail};
use axum::{Router, routing::get};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{path::Path, sync::Arc, time::Duration};
use tokio::sync::{Mutex, oneshot};

const AUTHORIZE: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const REDIRECT: &str = "http://localhost:51121/oauth-callback";
const SCOPES: &str = concat!(
    "https://www.googleapis.com/auth/cloud-platform ",
    "https://www.googleapis.com/auth/userinfo.email ",
    "https://www.googleapis.com/auth/userinfo.profile ",
    "https://www.googleapis.com/auth/cclog ",
    "https://www.googleapis.com/auth/experimentsandconfigs"
);

#[derive(Clone)]
pub(super) struct AgyEndpoints {
    pub(super) token: String,
    userinfo: String,
    project: String,
    client: ClientSource,
}

impl AuthClient {
    /// Proxy-owned native AGY OAuth. Does not inspect installed native credentials.
    pub fn agy() -> Result<Self> {
        Self::agy_with_state_dir(&crate::config::state_dir()?)
    }

    /// Keep login and refresh on the selected proxy state, with lazy verified acquisition.
    pub fn agy_with_state_dir(root: &Path) -> Result<Self> {
        Self::with_agy_source(
            "https://oauth2.googleapis.com/token".into(),
            "https://www.googleapis.com/oauth2/v1/userinfo".into(),
            "https://cloudcode-pa.googleapis.com".into(),
            ClientSource::from_environment(root.to_owned()),
        )
    }

    /// Injectable Google endpoints; HTTP is accepted only on loopback for synthetic tests.
    /// `project_base` is the base preceding `/v1internal:loadCodeAssist` and `:onboardUser`.
    pub fn with_agy_endpoints(
        token: String,
        userinfo: String,
        project_base: String,
        client_id: String,
        client_secret: String,
    ) -> Result<Self> {
        Self::with_agy_source(
            token,
            userinfo,
            project_base,
            ClientSource::explicit(client_id, client_secret),
        )
    }

    pub(super) fn with_agy_source(
        token: String,
        userinfo: String,
        project_base: String,
        client: ClientSource,
    ) -> Result<Self> {
        for endpoint in [&token, &userinfo, &project_base] {
            crate::config::validate_endpoint(endpoint)
                .map_err(|_| anyhow::anyhow!("Invalid AGY OAuth endpoint"))?;
        }
        Self::with_flavor(
            String::new(),
            Flavor::Agy(AgyEndpoints {
                token,
                userinfo,
                project: project_base.trim_end_matches('/').to_owned(),
                client,
            }),
        )
    }

    pub(super) async fn agy_refresh(
        &self,
        current: &Credential,
    ) -> Result<Credential, RefreshError> {
        let endpoints = self.agy_endpoints()?;
        let client = endpoints.client.resolve(&self.client).await?;
        // Reference acquisition may have waited on disk/network. Do not start a
        // rotating token exchange after shutdown began during that preparation.
        self.ensure_refresh_open()?;
        let (client_id, client_secret) = client.pair();
        let response = self
            .client
            .post(&endpoints.token)
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", current.refresh_token.as_str()),
                ("client_id", client_id),
                ("client_secret", client_secret),
            ])
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("OAuth refresh network error"))?;
        let status = response.status();
        if !status.is_success() {
            #[derive(Deserialize)]
            struct GoogleError {
                error: String,
            }
            // Only explicit account rejection revokes a login; a client config
            // error or generic HTTP failure must not disable the account.
            let error = read_json::<GoogleError>(response).await.ok();
            if error
                .as_ref()
                .is_some_and(|error| error.error == "invalid_client")
            {
                return Err(
                    anyhow::anyhow!("Google OAuth client configuration was rejected").into(),
                );
            }
            if matches!(status.as_u16(), 400 | 401 | 403)
                && error.is_some_and(|error| {
                    matches!(error.error.as_str(), "invalid_grant" | "invalid_token")
                })
            {
                return Err(RefreshError::Revoked);
            }
            return Err(anyhow::anyhow!("OAuth refresh failed (HTTP {})", status.as_u16()).into());
        }
        let tokens: Tokens = read_json(response)
            .await
            .context("Invalid OAuth refresh response")?;
        agy_credential_from_tokens(tokens, &current.subject, &current.account_id, Some(current))
            .map_err(RefreshError::from)
    }

    /// Explicit browser consent is the only flow permitted to onboard a managed project.
    pub async fn agy_browser_login(&self) -> Result<Credential> {
        let client = self.agy_endpoints()?.client.resolve(&self.client).await?;
        let (client_id, _) = client.pair();
        let verifier = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>());
        let state = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>());
        let url = authorization_url(&verifier, &state, client_id)?;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:51121")
            .await
            .map_err(|_| anyhow::anyhow!("AGY OAuth callback port 51121 is unavailable"))?;
        let (tx, rx) = oneshot::channel();
        let app = Router::new()
            .route("/oauth-callback", get(oauth_callback))
            .with_state(Callback {
                expected: state,
                sender: Arc::new(Mutex::new(Some(tx))),
            });
        let (stop_tx, stop_rx) = oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = stop_rx.await;
                })
                .await
        });
        println!(
            "Open this URL to sign in (use a private browser window for another account):\n{url}\n"
        );
        #[cfg(target_os = "linux")]
        {
            let _ = std::process::Command::new("xdg-open")
                .arg(url.as_str())
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
        }
        let code = tokio::select! {
            code = tokio::time::timeout(Duration::from_secs(300), rx) => {
                code.context("OAuth login timed out")?.context("OAuth callback closed")?
            }
            _ = tokio::signal::ctrl_c() => Err("Login cancelled".into()),
        };
        let _ = stop_tx.send(());
        let _ = tokio::time::timeout(Duration::from_secs(2), server).await;
        let code = code.map_err(|_| anyhow::anyhow!("Login was declined or cancelled"))?;
        self.agy_exchange_code(&code, &verifier).await
    }

    fn agy_endpoints(&self) -> Result<&AgyEndpoints> {
        match &self.flavor {
            Flavor::Agy(endpoints) => Ok(endpoints),
            Flavor::Codex => bail!("AGY login requires an AGY OAuth client"),
        }
    }

    async fn agy_exchange_code(&self, code: &str, verifier: &str) -> Result<Credential> {
        let endpoints = self.agy_endpoints()?;
        let client = endpoints.client.resolve(&self.client).await?;
        let (client_id, client_secret) = client.pair();
        let response = self
            .client
            .post(&endpoints.token)
            .form(&[
                ("client_id", client_id),
                ("client_secret", client_secret),
                ("grant_type", "authorization_code"),
                ("code", code),
                ("code_verifier", verifier),
                ("redirect_uri", REDIRECT),
            ])
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("AGY OAuth token exchange network error"))?;
        require_success(&response, "AGY OAuth token exchange")?;
        let tokens: Tokens = read_json(response).await?;
        if tokens.access_token.is_empty() {
            bail!("OAuth returned an empty access token");
        }
        if tokens.refresh_token.as_ref().is_none_or(String::is_empty) {
            bail!("OAuth did not return a refresh token");
        }
        #[derive(Deserialize)]
        struct UserInfo {
            id: String,
        }
        let response = self
            .client
            .get(&endpoints.userinfo)
            .bearer_auth(&tokens.access_token)
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("AGY user identity request failed"))?;
        require_success(&response, "AGY user identity request")?;
        let identity: UserInfo = read_json(response).await?;
        if identity.id.trim().is_empty() {
            bail!("Google did not identify a stable user ID; account was not added");
        }
        let project = self.agy_discover_project(&tokens.access_token).await?;
        agy_credential_from_tokens(tokens, &identity.id, &project, None)
    }

    async fn agy_project_request(&self, access: &str, method: &str, body: &Value) -> Result<Value> {
        let endpoints = self.agy_endpoints()?;
        let response = self
            .client
            .post(format!("{}/v1internal:{method}", endpoints.project))
            .bearer_auth(access)
            .json(body)
            .send()
            .await
            .map_err(|_| {
                anyhow::anyhow!("AGY project discovery network error; login not completed")
            })?;
        require_success(&response, "AGY project discovery")?;
        read_json(response).await
    }

    async fn agy_discover_project(&self, access: &str) -> Result<String> {
        let metadata = client_metadata();
        let loaded = self
            .agy_project_request(
                access,
                "loadCodeAssist",
                &json!({"metadata": metadata, "mode": 1}),
            )
            .await?;
        if !loaded.is_object() || loaded.get("error").is_some() {
            bail!("Invalid AGY project discovery response; login not completed");
        }
        if let Some(project) = project_id(loaded.get("cloudaicompanionProject")) {
            return Ok(project.to_owned());
        }
        let tiers = loaded.get("allowedTiers").and_then(Value::as_array);
        let tier = tiers
            .and_then(|tiers| {
                tiers
                    .iter()
                    .find(|tier| tier.get("isDefault").and_then(Value::as_bool) == Some(true))
                    .or_else(|| tiers.first())
            })
            .and_then(|tier| tier.get("id"))
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .unwrap_or("free-tier");
        let body = json!({"tierId": tier, "metadata": metadata});
        // Poll only confirmed not-done responses. Never retry a transport error or
        // unsuccessful HTTP response: dispatch may already have changed the account.
        for attempt in 0..10 {
            let onboarded = self
                .agy_project_request(access, "onboardUser", &body)
                .await?;
            if !onboarded.is_object() || onboarded.get("error").is_some() {
                bail!("Invalid AGY onboarding response; login not completed");
            }
            if onboarded.get("done").and_then(Value::as_bool) == Some(true) {
                return project_id(onboarded.pointer("/response/cloudaicompanionProject"))
                    .map(str::to_owned)
                    .context(
                        "AGY onboarding did not identify a managed project; login not completed",
                    );
            }
            // Google long-running operations may omit the default false `done`.
            if onboarded
                .get("done")
                .is_some_and(|done| done.as_bool() != Some(false))
            {
                bail!("Invalid AGY onboarding response; login not completed");
            }
            if attempt < 9 {
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
        bail!("AGY project onboarding timed out; login not completed")
    }
}

fn authorization_url(verifier: &str, state: &str, client_id: &str) -> Result<url::Url> {
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let mut url = url::Url::parse(AUTHORIZE)?;
    url.query_pairs_mut().extend_pairs([
        ("response_type", "code"),
        ("client_id", client_id),
        ("redirect_uri", REDIRECT),
        ("scope", SCOPES),
        ("code_challenge", challenge.as_str()),
        ("code_challenge_method", "S256"),
        ("state", state),
        ("access_type", "offline"),
        ("prompt", "consent"),
    ]);
    Ok(url)
}

fn client_metadata() -> Value {
    let platform = if cfg!(target_arch = "aarch64") { 4 } else { 3 };
    json!({"ideType": 9, "platform": platform, "pluginType": 2})
}

fn project_id(value: Option<&Value>) -> Option<&str> {
    let value = value?;
    value
        .as_str()
        .or_else(|| value.get("id").and_then(Value::as_str))
        .filter(|id| !id.trim().is_empty())
}

fn require_success(response: &reqwest::Response, operation: &str) -> Result<()> {
    if !response.status().is_success() {
        bail!("{operation} failed (HTTP {})", response.status().as_u16());
    }
    Ok(())
}

pub(super) fn agy_credential_from_tokens(
    tokens: Tokens,
    subject: &str,
    project: &str,
    previous: Option<&Credential>,
) -> Result<Credential> {
    if tokens.access_token.is_empty() || subject.trim().is_empty() || project.trim().is_empty() {
        bail!("AGY OAuth returned incomplete credentials or identity");
    }
    Ok(Credential {
        access_token: tokens.access_token,
        refresh_token: tokens
            .refresh_token
            .or_else(|| previous.map(|credential| credential.refresh_token.clone()))
            .filter(|token| !token.is_empty())
            .context("OAuth did not return a refresh token")?,
        id_token: None,
        expires_at: now().saturating_add(tokens.expires_in.unwrap_or(3600)),
        subject: subject.to_owned(),
        account_id: project.to_owned(),
        generation: previous
            .map(|credential| credential.generation.clone())
            .unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        extract::{Request, State},
        http::{HeaderMap, StatusCode},
        response::Response,
        routing::any,
    };
    use std::collections::{HashMap, VecDeque};
    use tokio::task::JoinHandle;

    struct Recorded {
        path: String,
        method: String,
        headers: HeaderMap,
        body: Vec<u8>,
    }
    #[derive(Clone)]
    struct MockState {
        requests: Arc<Mutex<Vec<Recorded>>>,
        responses: Arc<Mutex<VecDeque<(StatusCode, Value)>>>,
    }
    struct Mock {
        auth: AuthClient,
        state: MockState,
        task: JoinHandle<()>,
    }
    impl Drop for Mock {
        fn drop(&mut self) {
            self.task.abort();
        }
    }
    impl Mock {
        async fn start(responses: Vec<(StatusCode, Value)>) -> Self {
            let state = MockState {
                requests: Arc::new(Mutex::new(Vec::new())),
                responses: Arc::new(Mutex::new(responses.into())),
            };
            async fn handler(State(state): State<MockState>, request: Request) -> Response {
                let (parts, body) = request.into_parts();
                state.requests.lock().await.push(Recorded {
                    path: parts.uri.path().into(),
                    method: parts.method.to_string(),
                    headers: parts.headers,
                    body: to_bytes(body, 65536).await.unwrap().to_vec(),
                });
                let (status, body) = state.responses.lock().await.pop_front().unwrap();
                Response::builder()
                    .status(status)
                    .body(Body::from(body.to_string()))
                    .unwrap()
            }
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let auth = AuthClient::with_agy_endpoints(
                format!("{base}/token"),
                format!("{base}/userinfo"),
                base,
                "synthetic-client-id".into(),
                "synthetic-client-secret".into(),
            )
            .unwrap();
            let app = Router::new()
                .fallback(any(handler))
                .with_state(state.clone());
            let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            Self { auth, state, task }
        }
    }
    fn tokens() -> (StatusCode, Value) {
        (
            StatusCode::OK,
            json!({
                "access_token": "synthetic-google-access", "refresh_token": "synthetic-google-refresh",
                "expires_in": 3600
            }),
        )
    }

    #[test]
    fn authorization_uses_exact_redirect_pkce_state_and_google_scopes() {
        let url = authorization_url(
            "synthetic-verifier",
            "synthetic-state",
            "synthetic-client-id",
        )
        .unwrap();
        assert_eq!(
            url.origin().ascii_serialization(),
            "https://accounts.google.com"
        );
        assert_eq!(url.path(), "/o/oauth2/v2/auth");
        let query: HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(query["state"], "synthetic-state");
        assert_eq!(
            query["redirect_uri"],
            "http://localhost:51121/oauth-callback"
        );
        assert_eq!(query["code_challenge_method"], "S256");
        assert_eq!(
            query["code_challenge"],
            URL_SAFE_NO_PAD.encode(Sha256::digest(b"synthetic-verifier"))
        );
        assert_eq!(query["scope"].split_whitespace().count(), 5);
        assert_eq!(query["access_type"], "offline");
        assert_eq!(query["prompt"], "consent");
    }

    #[tokio::test]
    async fn login_exchanges_exact_form_requires_user_id_and_discovers_managed_project() {
        for project in [json!("managed-project"), json!({"id": "managed-project"})] {
            let mock = Mock::start(vec![
                tokens(),
                (
                    StatusCode::OK,
                    json!({"id": "stable-google-id", "email": "not-an-identity@example.test"}),
                ),
                (StatusCode::OK, json!({"cloudaicompanionProject": project})),
            ])
            .await;
            let credential = mock
                .auth
                .agy_exchange_code("synthetic-code", "synthetic-verifier")
                .await
                .unwrap();
            assert_eq!(credential.subject, "stable-google-id");
            assert_eq!(credential.account_id, "managed-project");
            assert_eq!(credential.refresh_token, "synthetic-google-refresh");
            let requests = mock.state.requests.lock().await;
            assert_eq!(requests.len(), 3);
            let form: HashMap<_, _> = url::form_urlencoded::parse(&requests[0].body)
                .into_owned()
                .collect();
            assert_eq!(requests[0].path, "/token");
            assert_eq!(requests[0].method, "POST");
            assert_eq!(form["client_id"], "synthetic-client-id");
            assert_eq!(form["client_secret"], "synthetic-client-secret");
            assert_eq!(form["grant_type"], "authorization_code");
            assert_eq!(form["code"], "synthetic-code");
            assert_eq!(form["code_verifier"], "synthetic-verifier");
            assert_eq!(form["redirect_uri"], REDIRECT);
            assert_eq!(requests[1].method, "GET");
            assert_eq!(requests[1].path, "/userinfo");
            assert_eq!(
                requests[1].headers["authorization"],
                "Bearer synthetic-google-access"
            );
            assert_eq!(requests[2].path, "/v1internal:loadCodeAssist");
            let body: Value = serde_json::from_slice(&requests[2].body).unwrap();
            assert_eq!(body, json!({"metadata": client_metadata(), "mode": 1}));
        }
    }

    #[tokio::test]
    async fn login_does_not_substitute_email_for_stable_google_id() {
        for identity in [json!({"email": "not-id@example.test"}), json!({"id": " "})] {
            let mock = Mock::start(vec![tokens(), (StatusCode::OK, identity)]).await;
            assert!(
                mock.auth
                    .agy_exchange_code("code", "verifier")
                    .await
                    .is_err()
            );
            assert_eq!(mock.state.requests.lock().await.len(), 2);
        }
    }

    #[tokio::test]
    async fn explicit_login_onboarding_selects_default_then_first_then_free_tier() {
        for (tiers, expected) in [
            (
                json!([{"id": "first"}, {"id": "default", "isDefault": true}]),
                "default",
            ),
            (json!([{"id": "first"}]), "first"),
            (json!([]), "free-tier"),
        ] {
            let mock = Mock::start(vec![
                tokens(),
                (StatusCode::OK, json!({"id": "stable-google-id"})),
                (StatusCode::OK, json!({"allowedTiers": tiers})),
                (
                    StatusCode::OK,
                    json!({
                        "done": true,
                        "response": {"cloudaicompanionProject": {"id": "provisioned"}}
                    }),
                ),
            ])
            .await;
            let credential = mock
                .auth
                .agy_exchange_code("code", "verifier")
                .await
                .unwrap();
            assert_eq!(credential.account_id, "provisioned");
            let requests = mock.state.requests.lock().await;
            assert_eq!(requests[3].path, "/v1internal:onboardUser");
            let body: Value = serde_json::from_slice(&requests[3].body).unwrap();
            assert_eq!(
                body,
                json!({"tierId": expected, "metadata": client_metadata()})
            );
        }
    }

    #[tokio::test]
    async fn onboarding_missing_project_fails_closed_without_hardcoded_fallback() {
        let mock = Mock::start(vec![
            tokens(),
            (StatusCode::OK, json!({"id": "stable-google-id"})),
            (StatusCode::OK, json!({})),
            (StatusCode::OK, json!({"done": true})),
        ])
        .await;
        let error = mock
            .auth
            .agy_exchange_code("code", "verifier")
            .await
            .err()
            .unwrap();
        assert!(
            error
                .to_string()
                .contains("did not identify a managed project")
        );
        assert_eq!(mock.state.requests.lock().await.len(), 4);
    }

    #[tokio::test]
    async fn malformed_project_discovery_does_not_start_onboarding() {
        for body in [
            Value::Null,
            json!([]),
            json!({"error": "private-body-secret"}),
        ] {
            let mock = Mock::start(vec![
                tokens(),
                (StatusCode::OK, json!({"id": "stable-google-id"})),
                (StatusCode::OK, body),
            ])
            .await;
            let error = mock
                .auth
                .agy_exchange_code("code", "verifier")
                .await
                .err()
                .unwrap();
            assert!(!format!("{error:#}").contains("private-body-secret"));
            assert_eq!(mock.state.requests.lock().await.len(), 3);
        }
    }

    #[tokio::test]
    async fn unsuccessful_project_discovery_is_not_retried_or_onboarded() {
        let mock = Mock::start(vec![
            tokens(),
            (StatusCode::OK, json!({"id": "stable-google-id"})),
            (
                StatusCode::SERVICE_UNAVAILABLE,
                json!({"error": "private-body-secret"}),
            ),
        ])
        .await;
        let error = mock
            .auth
            .agy_exchange_code("code", "verifier")
            .await
            .err()
            .unwrap();
        assert!(!format!("{error:?}").contains("private-body-secret"));
        assert_eq!(mock.state.requests.lock().await.len(), 3);
    }

    #[tokio::test]
    async fn missing_login_refresh_token_does_not_discover_or_onboard() {
        let mock = Mock::start(vec![(
            StatusCode::OK,
            json!({"access_token": "synthetic-access", "expires_in": 3600}),
        )])
        .await;
        assert!(
            mock.auth
                .agy_exchange_code("code", "verifier")
                .await
                .is_err()
        );
        assert_eq!(mock.state.requests.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn onboarding_polls_confirmed_pending_operation_at_five_second_intervals() {
        let mock = Mock::start(vec![
            tokens(),
            (StatusCode::OK, json!({"id": "stable-google-id"})),
            (StatusCode::OK, json!({})),
            (StatusCode::OK, json!({"name": "pending-operation"})),
            (
                StatusCode::OK,
                json!({
                    "done": true,
                    "response": {"cloudaicompanionProject": {"id": "provisioned"}}
                }),
            ),
        ])
        .await;
        let start = tokio::time::Instant::now();
        let credential = mock
            .auth
            .agy_exchange_code("code", "verifier")
            .await
            .unwrap();
        assert!(start.elapsed() >= Duration::from_secs(5));
        assert_eq!(credential.account_id, "provisioned");
        assert_eq!(mock.state.requests.lock().await.len(), 5);
    }

    #[tokio::test]
    async fn unsuccessful_onboarding_is_not_retried() {
        let mock = Mock::start(vec![
            tokens(),
            (StatusCode::OK, json!({"id": "stable-google-id"})),
            (StatusCode::OK, json!({})),
            (
                StatusCode::SERVICE_UNAVAILABLE,
                json!({"error": "private-body-secret"}),
            ),
        ])
        .await;
        assert!(
            mock.auth
                .agy_exchange_code("code", "verifier")
                .await
                .is_err()
        );
        assert_eq!(mock.state.requests.lock().await.len(), 4);
    }
}
