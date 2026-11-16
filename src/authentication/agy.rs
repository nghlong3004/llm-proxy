//! Native AGY Google OAuth. Project protocol: opencode-antigravity-auth
//! 821f824440b2b7c9ffe96bbdfad9d5cdead2f1de; see docs/antigravity.md.
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
use futures_util::StreamExt;
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

const PROJECT_ENDPOINT: &str = "https://daily-cloudcode-pa.googleapis.com";
const ANTIGRAVITY_VERSION: &str = "2.24.0";
const DISCOVERY_USER_AGENT: &str =
    "antigravity/hub/2.24.0 (aidev_client; os_type=darwin; arch=arm64; cl=1000335682)";
const ONBOARD_USER_AGENT: &str = concat!(
    "antigravity/hub/2.24.0 (aidev_client; os_type=darwin; arch=arm64; cl=1000335682) ",
    "google-api-nodejs-client/10.3.0"
);
const ONBOARD_ATTEMPTS: usize = 10;
const PROJECT_RECHECKS: usize = 3;
const PROJECT_POLL_DELAY: Duration = Duration::from_secs(5);

#[derive(Clone, Copy)]
enum ProjectOperation {
    Load,
    Onboard,
}

impl ProjectOperation {
    fn method(self) -> &'static str {
        match self {
            Self::Load => "loadCodeAssist",
            Self::Onboard => "onboardUser",
        }
    }

    fn timeout(self) -> Duration {
        Duration::from_secs(match self {
            Self::Load => 10,
            Self::Onboard => 30,
        })
    }
}

#[derive(Debug, thiserror::Error)]
enum ProjectError {
    #[error("AGY project transport failure; login not completed")]
    Transport,
    #[error("AGY project request failed (HTTP {0}); login not completed")]
    Http(u16),
    #[error("Malformed AGY project response; login not completed")]
    Malformed,
    #[error("AGY project response exceeds size limit; login not completed")]
    TooLarge,
    #[error("AGY project operation returned a provider error; login not completed")]
    Provider,
    #[error(
        "AGY onboarding did not identify a managed project after bounded discovery recheck; login not completed"
    )]
    MissingProject,
    #[error("AGY project onboarding timed out; login not completed")]
    OnboardingTimeout,
}

// Only allowlisted metadata reaches tracing or CLI error chains, never response values.
#[derive(Debug)]
struct ProjectDiagnostics {
    operation: &'static str,
    host: String,
    status: Option<u16>,
    tier: &'static str,
    done: &'static str,
    project_type: &'static str,
    project_id_type: &'static str,
}

impl std::fmt::Display for ProjectDiagnostics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "AGY operation={} host={} status={} selected_tier={} done={} project_field={} project_id_field={}",
            self.operation,
            self.host,
            self.status
                .map_or_else(|| "unavailable".into(), |status| status.to_string()),
            self.tier,
            self.done,
            self.project_type,
            self.project_id_type,
        )
    }
}

impl ProjectDiagnostics {
    fn observe(&mut self, operation: ProjectOperation, body: &Value) {
        self.done = match body.get("done") {
            None => "absent",
            Some(Value::Bool(true)) => "true",
            Some(Value::Bool(false)) => "false",
            Some(_) => "invalid",
        };
        let project = match operation {
            ProjectOperation::Load => body.get("cloudaicompanionProject"),
            ProjectOperation::Onboard => body.pointer("/response/cloudaicompanionProject"),
        };
        self.project_type = field_type(project);
        self.project_id_type = field_type(project.and_then(|project| project.get("id")));
    }
}

fn field_type(value: Option<&Value>) -> &'static str {
    match value {
        None => "absent",
        Some(Value::Null) => "null",
        Some(Value::Bool(_)) => "boolean",
        Some(Value::Number(_)) => "number",
        Some(Value::String(_)) => "string",
        Some(Value::Array(_)) => "array",
        Some(Value::Object(_)) => "object",
    }
}

fn diagnostic_tier(tier: Option<&str>) -> &'static str {
    // Unknown provider strings can contain identifiers or secrets, not just tier names.
    match tier {
        None => "not-selected",
        Some("free-tier") => "free-tier",
        Some("standard-tier") => "standard-tier",
        Some(_) => "other",
    }
}

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
            PROJECT_ENDPOINT.into(),
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

    async fn agy_project_request(
        &self,
        access: &str,
        operation: ProjectOperation,
        body: &Value,
        tier: Option<&str>,
    ) -> Result<(Value, ProjectDiagnostics)> {
        let endpoints = self.agy_endpoints()?;
        let method = operation.method();
        let mut diagnostics = ProjectDiagnostics {
            operation: method,
            host: url::Url::parse(&endpoints.project)?
                .host_str()
                .unwrap_or("unknown")
                .to_owned(),
            status: None,
            tier: diagnostic_tier(tier),
            done: "unavailable",
            project_type: "unavailable",
            project_id_type: "unavailable",
        };
        let mut request = self
            .client
            .post(format!("{}/v1internal:{method}", endpoints.project))
            .timeout(operation.timeout())
            .bearer_auth(access)
            .json(body);
        request = match operation {
            ProjectOperation::Load => request.header("User-Agent", DISCOVERY_USER_AGENT),
            ProjectOperation::Onboard => request
                .header("User-Agent", ONBOARD_USER_AGENT)
                .header("Accept", "*/*")
                .header("X-Goog-Api-Client", "gl-node/22.21.1"),
        };
        let result = async {
            let response = request.send().await.map_err(|_| ProjectError::Transport)?;
            diagnostics.status = Some(response.status().as_u16());
            if !response.status().is_success() {
                return Err(ProjectError::Http(response.status().as_u16()));
            }
            let mut bytes = Vec::new();
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|_| ProjectError::Transport)?;
                if bytes.len().saturating_add(chunk.len()) > 512 * 1024 {
                    return Err(ProjectError::TooLarge);
                }
                bytes.extend_from_slice(&chunk);
            }
            let value: Value =
                serde_json::from_slice(&bytes).map_err(|_| ProjectError::Malformed)?;
            diagnostics.observe(operation, &value);
            if !value.is_object() {
                return Err(ProjectError::Malformed);
            }
            if value.get("error").is_some() {
                return Err(ProjectError::Provider);
            }
            if matches!(operation, ProjectOperation::Onboard)
                && value.get("done").is_some_and(|done| !done.is_boolean())
            {
                return Err(ProjectError::Malformed);
            }
            Ok(value)
        }
        .await;
        tracing::info!(event = "agy_project_response", diagnostics = %diagnostics);
        match result {
            Ok(value) => Ok((value, diagnostics)),
            Err(error) => Err(anyhow::Error::new(error).context(diagnostics.to_string())),
        }
    }

    async fn agy_load_project(
        &self,
        access: &str,
        tier: Option<&str>,
    ) -> Result<(Value, ProjectDiagnostics)> {
        self.agy_project_request(
            access,
            ProjectOperation::Load,
            &json!({"metadata": {"ideType": "ANTIGRAVITY"}}),
            tier,
        )
        .await
    }

    async fn agy_discover_project(&self, access: &str) -> Result<String> {
        let (loaded, _) = self.agy_load_project(access, None).await?;
        if let Some(project) = project_id(loaded.get("cloudaicompanionProject")) {
            return Ok(project.to_owned());
        }
        let tier = selected_tier(&loaded);
        let body = json!({
            "tier_id": tier,
            "metadata": {
                "ide_type": "ANTIGRAVITY",
                "ide_version": ANTIGRAVITY_VERSION,
                "ide_name": "antigravity"
            }
        });
        // This protocol polls via the same POST, but only after confirmed pending
        // responses. Dispatch failures must never replay this mutation.
        for attempt in 0..ONBOARD_ATTEMPTS {
            let (onboarded, diagnostics) = self
                .agy_project_request(access, ProjectOperation::Onboard, &body, Some(tier))
                .await?;
            if onboarded.get("done").and_then(Value::as_bool) == Some(true) {
                if let Some(project) =
                    project_id(onboarded.pointer("/response/cloudaicompanionProject"))
                {
                    return Ok(project.to_owned());
                }
                // A completed operation may precede discovery visibility. This is
                // a bounded, read-only robustness improvement, not upstream behavior.
                return self
                    .agy_recheck_project(access, tier)
                    .await
                    .context(diagnostics.to_string());
            }
            // Google long-running operations may omit the default false `done`.
            if attempt + 1 == ONBOARD_ATTEMPTS {
                return Err(anyhow::Error::new(ProjectError::OnboardingTimeout)
                    .context(diagnostics.to_string()));
            }
            tokio::time::sleep(PROJECT_POLL_DELAY).await;
        }
        Err(ProjectError::OnboardingTimeout.into())
    }

    async fn agy_recheck_project(&self, access: &str, tier: &str) -> Result<String> {
        for attempt in 0..PROJECT_RECHECKS {
            if attempt > 0 {
                tokio::time::sleep(PROJECT_POLL_DELAY).await;
            }
            let (loaded, diagnostics) = self.agy_load_project(access, Some(tier)).await?;
            if let Some(project) = project_id(loaded.get("cloudaicompanionProject")) {
                return Ok(project.to_owned());
            }
            if attempt + 1 == PROJECT_RECHECKS {
                return Err(anyhow::Error::new(ProjectError::MissingProject)
                    .context(diagnostics.to_string()));
            }
        }
        Err(ProjectError::MissingProject.into())
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

fn selected_tier(loaded: &Value) -> &str {
    let tiers = loaded.get("allowedTiers").and_then(Value::as_array);
    tiers
        .and_then(|tiers| {
            tiers
                .iter()
                .filter(|tier| tier.get("isDefault").and_then(Value::as_bool) == Some(true))
                .find_map(|tier| nonempty_string(tier.get("id")))
                .or_else(|| {
                    tiers
                        .first()
                        .and_then(|tier| nonempty_string(tier.get("id")))
                })
        })
        .or_else(|| nonempty_string(loaded.pointer("/currentTier/id")))
        .unwrap_or("free-tier")
}

fn nonempty_string(value: Option<&Value>) -> Option<&str> {
    value?.as_str().map(str::trim).filter(|id| !id.is_empty())
}

fn project_id(value: Option<&Value>) -> Option<&str> {
    let value = value?;
    nonempty_string(Some(value)).or_else(|| nonempty_string(value.get("id")))
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
        responses: Arc<Mutex<VecDeque<(StatusCode, Body)>>>,
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
            Self::start_bodies(
                responses
                    .into_iter()
                    .map(|(status, value)| (status, Body::from(value.to_string())))
                    .collect(),
            )
            .await
        }

        async fn start_bodies(responses: Vec<(StatusCode, Body)>) -> Self {
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
                Response::builder().status(status).body(body).unwrap()
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
        for project in [
            json!("managed-project"),
            json!({"id": "managed-project"}),
            json!(" \tmanaged-project\n"),
            json!({"id": " managed-project "}),
        ] {
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
            assert_eq!(requests[2].method, "POST");
            assert_eq!(requests[2].headers["content-type"], "application/json");
            assert_eq!(
                requests[2].headers["authorization"],
                "Bearer synthetic-google-access"
            );
            assert_eq!(
                requests[2].headers["user-agent"],
                "antigravity/hub/2.24.0 (aidev_client; os_type=darwin; arch=arm64; cl=1000335682)"
            );
            assert!(!requests[2].headers.contains_key("x-goog-api-client"));
            assert!(!requests[2].headers.contains_key("client-metadata"));
            assert_eq!(body, json!({"metadata": {"ideType": "ANTIGRAVITY"}}));
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
    async fn explicit_login_onboarding_selects_default_then_first_then_current_then_free_tier() {
        for (loaded, expected) in [
            (
                json!({
                    "allowedTiers": [{"id": "first"}, {"id": "default", "isDefault": true}],
                    "currentTier": {"id": "current"}
                }),
                "default",
            ),
            (
                json!({"allowedTiers": [{"id": "first"}], "currentTier": {"id": "current"}}),
                "first",
            ),
            (
                json!({"allowedTiers": [], "currentTier": {"id": "current"}}),
                "current",
            ),
            (json!({}), "free-tier"),
            (
                json!({
                    "allowedTiers": [{"isDefault": true}, {"id": "default", "isDefault": true}]
                }),
                "default",
            ),
            (
                json!({
                    "allowedTiers": [{"id": " "}, {"id": "not-first"}],
                    "currentTier": {"id": " current "}
                }),
                "current",
            ),
            (
                json!({"allowedTiers": [{"id": 42}], "currentTier": {"id": " "}}),
                "free-tier",
            ),
        ] {
            let mock = Mock::start(vec![
                tokens(),
                (StatusCode::OK, json!({"id": "stable-google-id"})),
                (StatusCode::OK, loaded),
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
            assert_eq!(requests[3].method, "POST");
            assert_eq!(
                requests[3].headers["authorization"],
                "Bearer synthetic-google-access"
            );
            assert_eq!(requests[3].headers["content-type"], "application/json");
            assert_eq!(requests[3].headers["accept"], "*/*");
            assert_eq!(
                requests[3].headers["user-agent"],
                "antigravity/hub/2.24.0 (aidev_client; os_type=darwin; arch=arm64; cl=1000335682) google-api-nodejs-client/10.3.0"
            );
            assert_eq!(requests[3].headers["x-goog-api-client"], "gl-node/22.21.1");
            assert!(!requests[3].headers.contains_key("client-metadata"));
            assert_eq!(
                body,
                json!({
                    "tier_id": expected,
                    "metadata": {
                        "ide_type": "ANTIGRAVITY",
                        "ide_version": "2.24.0",
                        "ide_name": "antigravity"
                    }
                })
            );
        }
    }

    #[tokio::test]
    async fn onboarding_missing_project_fails_closed_without_storing_incomplete_account() {
        let mut replies = vec![
            tokens(),
            (StatusCode::OK, json!({"id": "stable-google-id"})),
            (StatusCode::OK, json!({})),
            (StatusCode::OK, json!({"done": true})),
        ];
        replies.extend((0..PROJECT_RECHECKS).map(|_| (StatusCode::OK, json!({}))));
        let mock = Mock::start(replies).await;
        let directory = tempfile::tempdir().unwrap();
        let store = crate::accounts::Store::open(directory.path()).unwrap();
        let error = crate::application::account_commands::login_for(
            store.clone(),
            "agy".into(),
            "synthetic".into(),
            mock.auth.agy_exchange_code("code", "verifier"),
        )
        .await
        .err()
        .unwrap();
        assert!(matches!(
            error.downcast_ref::<ProjectError>(),
            Some(ProjectError::MissingProject)
        ));
        assert!(format!("{error:#}").contains("done=true project_field=absent"));
        assert!(store.list().unwrap().accounts.is_empty());
        let requests = mock.state.requests.lock().await;
        assert_eq!(requests.len(), 4 + PROJECT_RECHECKS);
        assert!(
            requests[4..]
                .iter()
                .all(|request| request.path == "/v1internal:loadCodeAssist")
        );
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

    #[test]
    fn project_endpoint_and_timeout_contract_is_pinned() {
        let endpoints =
            AuthClient::agy_with_state_dir(Path::new("/unused-synthetic-state")).unwrap();
        assert_eq!(
            endpoints.agy_endpoints().unwrap().project,
            "https://daily-cloudcode-pa.googleapis.com"
        );
        assert_eq!(ProjectOperation::Load.timeout(), Duration::from_secs(10));
        assert_eq!(ProjectOperation::Onboard.timeout(), Duration::from_secs(30));
    }

    #[test]
    fn project_parser_trims_strings_and_objects_and_rejects_empty_or_other_types() {
        for value in [json!(" project "), json!({"id": "\tproject\n"})] {
            assert_eq!(project_id(Some(&value)), Some("project"));
        }
        for value in [
            Value::Null,
            json!(""),
            json!(" \t\n"),
            json!({"id": " "}),
            json!({}),
            json!(42),
            json!({"id": 42}),
            json!([]),
        ] {
            assert_eq!(project_id(Some(&value)), None);
        }
        assert_eq!(project_id(None), None);
    }

    #[tokio::test]
    async fn onboarding_accepts_trimmed_string_and_object_projects() {
        for project in [json!(" provisioned "), json!({"id": "\tprovisioned\n"})] {
            let mock = Mock::start(vec![
                (StatusCode::OK, json!({})),
                (
                    StatusCode::OK,
                    json!({"done": true, "response": {"cloudaicompanionProject": project}}),
                ),
            ])
            .await;
            assert_eq!(
                mock.auth
                    .agy_discover_project("synthetic-access")
                    .await
                    .unwrap(),
                "provisioned"
            );
            assert_eq!(mock.state.requests.lock().await.len(), 2);
        }
    }

    #[tokio::test]
    async fn done_without_usable_project_rechecks_discovery_without_restarting_onboarding() {
        for project in [Value::Null, json!(" "), json!({"id": " "}), json!(42)] {
            let mock = Mock::start(vec![
                (StatusCode::OK, json!({})),
                (
                    StatusCode::OK,
                    json!({"done": true, "response": {"cloudaicompanionProject": project}}),
                ),
                (
                    StatusCode::OK,
                    json!({"cloudaicompanionProject": {"id": " discovered "}}),
                ),
            ])
            .await;
            assert_eq!(
                mock.auth
                    .agy_discover_project("synthetic-access")
                    .await
                    .unwrap(),
                "discovered"
            );
            let requests = mock.state.requests.lock().await;
            assert_eq!(requests.len(), 3);
            assert_eq!(requests[2].path, "/v1internal:loadCodeAssist");
            assert_eq!(requests[2].body, requests[0].body);
            assert_eq!(
                requests[2].headers["user-agent"],
                requests[0].headers["user-agent"]
            );
            assert!(!requests[2].headers.contains_key("x-goog-api-client"));
        }
    }

    #[tokio::test]
    async fn completed_onboarding_allows_bounded_delayed_discovery_visibility() {
        let mock = Mock::start(vec![
            (StatusCode::OK, json!({})),
            (StatusCode::OK, json!({"done": true})),
            (StatusCode::OK, json!({})),
            (StatusCode::OK, json!({})),
            (
                StatusCode::OK,
                json!({"cloudaicompanionProject": "eventually-visible"}),
            ),
        ])
        .await;
        let start = tokio::time::Instant::now();
        assert_eq!(
            mock.auth
                .agy_discover_project("synthetic-access")
                .await
                .unwrap(),
            "eventually-visible"
        );
        assert!(start.elapsed() >= Duration::from_secs(10));
        assert_eq!(mock.state.requests.lock().await.len(), 5);
    }

    #[tokio::test]
    async fn pending_onboarding_is_bounded_and_reports_distinct_timeout() {
        let mut replies = vec![(StatusCode::OK, json!({}))];
        replies.extend((0..ONBOARD_ATTEMPTS).map(|_| (StatusCode::OK, json!({"done": false}))));
        let mock = Mock::start(replies).await;
        let error = mock
            .auth
            .agy_discover_project("synthetic-access")
            .await
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<ProjectError>(),
            Some(ProjectError::OnboardingTimeout)
        ));
        assert_eq!(mock.state.requests.lock().await.len(), 1 + ONBOARD_ATTEMPTS);
    }

    #[tokio::test]
    async fn invalid_onboarding_responses_stop_without_replay_or_discovery_recheck() {
        for (body, provider_error) in [
            (Value::Null, false),
            (json!([]), false),
            (json!({"done": "true"}), false),
            (
                json!({"done": true, "error": {"message": "private-response-secret"}}),
                true,
            ),
        ] {
            let mock = Mock::start(vec![(StatusCode::OK, json!({})), (StatusCode::OK, body)]).await;
            let error = mock
                .auth
                .agy_discover_project("synthetic-access")
                .await
                .unwrap_err();
            if provider_error {
                assert!(matches!(
                    error.downcast_ref::<ProjectError>(),
                    Some(ProjectError::Provider)
                ));
            } else {
                assert!(matches!(
                    error.downcast_ref::<ProjectError>(),
                    Some(ProjectError::Malformed)
                ));
            }
            assert!(!format!("{error:#}").contains("private-response-secret"));
            assert_eq!(mock.state.requests.lock().await.len(), 2);
        }
    }

    #[tokio::test]
    async fn invalid_json_and_oversized_responses_remain_distinct_from_missing_project() {
        for operation in [ProjectOperation::Load, ProjectOperation::Onboard] {
            for oversized in [false, true] {
                let body = if oversized {
                    "x".repeat(512 * 1024 + 1)
                } else {
                    "private-invalid-json".into()
                };
                let mock = Mock::start_bodies(vec![(StatusCode::OK, Body::from(body))]).await;
                let error = mock
                    .auth
                    .agy_project_request("access-secret", operation, &json!({}), None)
                    .await
                    .err()
                    .unwrap();
                assert!(matches!(
                    error.downcast_ref::<ProjectError>(),
                    Some(ProjectError::Malformed | ProjectError::TooLarge)
                ));
                assert!(!format!("{error:#}").contains("private-invalid-json"));
                assert_eq!(mock.state.requests.lock().await.len(), 1);
            }
        }
    }

    #[tokio::test]
    async fn failed_read_only_recheck_does_not_restart_onboarding() {
        for (status, body) in [
            (StatusCode::FORBIDDEN, json!({"error": "secret"})),
            (StatusCode::OK, json!({"error": "secret"})),
            (StatusCode::OK, Value::Null),
        ] {
            let mock = Mock::start(vec![
                (StatusCode::OK, json!({})),
                (StatusCode::OK, json!({"done": true})),
                (status, body),
            ])
            .await;
            let error = mock
                .auth
                .agy_discover_project("synthetic-access")
                .await
                .unwrap_err();
            assert!(!matches!(
                error.downcast_ref::<ProjectError>(),
                Some(ProjectError::MissingProject)
            ));
            let requests = mock.state.requests.lock().await;
            assert_eq!(requests.len(), 3);
            assert_eq!(requests[2].path, "/v1internal:loadCodeAssist");
        }
    }

    #[tokio::test]
    async fn onboarding_transport_failure_never_replays_uncertain_dispatch() {
        let broken_body = Body::from_stream(futures_util::stream::once(async {
            Err::<Vec<u8>, _>(std::io::Error::other("private-network-secret"))
        }));
        let mock = Mock::start_bodies(vec![
            (StatusCode::OK, Body::from("{}")),
            (StatusCode::OK, broken_body),
        ])
        .await;
        let error = mock
            .auth
            .agy_discover_project("access-secret")
            .await
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<ProjectError>(),
            Some(ProjectError::Transport)
        ));
        assert!(!format!("{error:#}").contains("private-network-secret"));
        assert_eq!(mock.state.requests.lock().await.len(), 2);
    }

    #[tokio::test]
    async fn diagnostics_only_include_allowlisted_metadata_not_secrets_or_account_ids() {
        let mock = Mock::start(vec![
            (
                StatusCode::OK,
                json!({
                    "allowedTiers": [{"id": "tier-secret", "isDefault": true}],
                    "account": "account-secret"
                }),
            ),
            (
                StatusCode::OK,
                json!({
                    "done": true,
                    "response": {"cloudaicompanionProject": {"id": "project-secret"}}
                }),
            ),
            (
                StatusCode::OK,
                json!({"done": true, "error": "response-secret"}),
            ),
        ])
        .await;
        let (_, discovery) = mock
            .auth
            .agy_load_project("access-secret", None)
            .await
            .unwrap();
        let (_, onboard) = mock
            .auth
            .agy_project_request(
                "access-secret",
                ProjectOperation::Onboard,
                &json!({"tier_id": "tier-secret"}),
                Some("tier-secret"),
            )
            .await
            .unwrap();
        let error = mock
            .auth
            .agy_project_request(
                "access-secret",
                ProjectOperation::Onboard,
                &json!({"tier_id": "tier-secret"}),
                Some("tier-secret"),
            )
            .await
            .err()
            .unwrap();
        // This is the same Display payload used by both tracing and CLI errors.
        // Test its exact bytes without relying on process-global tracing caches.
        assert_eq!(
            discovery.to_string(),
            concat!(
                "AGY operation=loadCodeAssist host=127.0.0.1 status=200 ",
                "selected_tier=not-selected done=absent project_field=absent project_id_field=absent"
            )
        );
        assert_eq!(
            onboard.to_string(),
            concat!(
                "AGY operation=onboardUser host=127.0.0.1 status=200 ",
                "selected_tier=other done=true project_field=object project_id_field=string"
            )
        );
        assert_eq!(diagnostic_tier(Some("free-tier")), "free-tier");
        assert_eq!(diagnostic_tier(Some("standard-tier")), "standard-tier");
        let output = format!("{discovery}\n{onboard}\n{error:#}");
        for secret in [
            "tier-secret",
            "account-secret",
            "response-secret",
            "project-secret",
            "access-secret",
        ] {
            assert!(!output.contains(secret));
        }
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
