//! Codex subscription OAuth. Tokens belong to this proxy, never the native CLI.
use super::{
    callback::{Callback, oauth_callback},
    tokens::{Tokens, credential_from_tokens},
};
use crate::accounts::{Credential, Store, now};
use anyhow::{Context, Result, bail};
use axum::{Router, routing::get};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use futures_util::StreamExt;
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{Mutex, Semaphore, oneshot},
    task::JoinSet,
};

pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const ISSUER: &str = "https://auth.openai.com";
const REDIRECT: &str = "http://localhost:1455/auth/callback";
#[derive(Clone)]
pub struct AuthClient {
    client: reqwest::Client,
    issuer: String,
    refresh_tasks: Arc<Mutex<JoinSet<()>>>,
    refresh_admission: Arc<Semaphore>,
    closing: Arc<AtomicBool>,
}
#[derive(Debug, thiserror::Error)]
pub enum RefreshError {
    #[error("Account was removed or its login generation changed")]
    AccountChanged,
    #[error("Cannot read or persist account credentials")]
    Storage(#[source] anyhow::Error),
    #[error("Account session was revoked; sign in again")]
    Revoked,
    #[error("Unable to renew account credential")]
    Other(#[source] anyhow::Error),
}
impl From<anyhow::Error> for RefreshError {
    fn from(error: anyhow::Error) -> Self {
        Self::Other(error)
    }
}
impl AuthClient {
    pub fn new() -> Result<Self> {
        Self::with_issuer(ISSUER.into())
    }
    pub fn with_issuer(issuer: String) -> Result<Self> {
        crate::config::validate_endpoint(&issuer)?;
        Ok(Self {
            client: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(15))
                .timeout(Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            issuer,
            refresh_tasks: Arc::new(Mutex::new(JoinSet::new())),
            refresh_admission: Arc::new(Semaphore::new(64)),
            closing: Arc::new(AtomicBool::new(false)),
        })
    }
    async fn tokens(&self, form: &[(&str, &str)]) -> Result<Tokens> {
        let response = self
            .client
            .post(format!("{}/oauth/token", self.issuer))
            .form(form)
            .send()
            .await
            .context("OAuth token exchange network error")?;
        if !response.status().is_success() {
            bail!(
                "OAuth token exchange failed (HTTP {}); try signing in again",
                response.status().as_u16()
            );
        }
        read_json(response)
            .await
            .context("Invalid OAuth token response")
    }
    pub async fn refresh(
        &self,
        store: &Store,
        id: &str,
        rejected_access: Option<&str>,
    ) -> Result<Credential, RefreshError> {
        self.admit_refresh(store, id, None, rejected_access).await
    }
    pub async fn refresh_for_generation(
        &self,
        store: &Store,
        id: &str,
        generation: &str,
        rejected_access: Option<&str>,
    ) -> Result<Credential, RefreshError> {
        self.admit_refresh(store, id, Some(generation), rejected_access)
            .await
    }
    async fn admit_refresh(
        &self,
        store: &Store,
        id: &str,
        generation: Option<&str>,
        rejected_access: Option<&str>,
    ) -> Result<Credential, RefreshError> {
        if self.closing.load(Ordering::Acquire) {
            return Err(anyhow::anyhow!("OAuth client is shutting down").into());
        }
        let mut tasks = self.refresh_tasks.lock().await;
        if self.closing.load(Ordering::Acquire) {
            return Err(anyhow::anyhow!("OAuth client is shutting down").into());
        }
        // Bound both live transactions and retained handles, even with cancelled callers.
        while let Some(result) = tasks.try_join_next() {
            if result.is_err() {
                tracing::warn!(event = "credential_refresh_task_failed");
            }
        }
        let permit = self
            .refresh_admission
            .clone()
            .try_acquire_owned()
            .map_err(|_| anyhow::anyhow!("OAuth refresh capacity reached"))?;
        let auth = self.clone();
        let store = store.clone();
        let id = id.to_owned();
        let generation = generation.map(str::to_owned);
        let rejected_access = rejected_access.map(str::to_owned);
        let (sender, receiver) = oneshot::channel();
        // This task owns the cross-process lock through parsing and durable publication.
        // Dropping a request only drops the receiver, never a possibly rotating exchange.
        tasks.spawn(async move {
            let _permit = permit;
            let result = auth
                .refresh_transaction(
                    &store,
                    &id,
                    generation.as_deref(),
                    rejected_access.as_deref(),
                )
                .await;
            let _ = sender.send(result);
        });
        drop(tasks);
        receiver
            .await
            .map_err(|_| anyhow::anyhow!("OAuth refresh task failed"))?
    }
    /// Close admission and drain admitted rotations. Process kills cannot be made lossless.
    pub async fn shutdown(&self) {
        self.closing.store(true, Ordering::Release);
        let mut tasks = self.refresh_tasks.lock().await;
        while let Some(result) = tasks.join_next().await {
            if result.is_err() {
                tracing::warn!(event = "credential_refresh_task_failed");
            }
        }
    }
    async fn refresh_transaction(
        &self,
        store: &Store,
        id: &str,
        generation: Option<&str>,
        rejected_access: Option<&str>,
    ) -> Result<Credential, RefreshError> {
        let _guard = loop {
            if self.closing.load(Ordering::Acquire) {
                return Err(anyhow::anyhow!("OAuth client is shutting down").into());
            }
            let guard_store = store.clone();
            let guard_id = id.to_owned();
            let guard =
                tokio::task::spawn_blocking(move || guard_store.try_refresh_lock(&guard_id))
                    .await
                    .map_err(|error| RefreshError::Storage(error.into()))?
                    .map_err(RefreshError::Storage)?;
            if let Some(guard) = guard {
                break guard;
            }
            // Do not strand a blocking lock waiter beyond the shutdown grace period.
            tokio::time::sleep(Duration::from_millis(25)).await;
        };
        let read_store = store.clone();
        let read_id = id.to_owned();
        let read_generation = generation.map(str::to_owned);
        let current = tokio::task::spawn_blocking(move || match read_generation {
            Some(generation) => read_store.lease_for_generation(&read_id, &generation),
            None => read_store.lease(&read_id).map(Some),
        })
        .await
        .map_err(|error| RefreshError::Storage(error.into()))?
        .map_err(RefreshError::Storage)?
        .ok_or(RefreshError::AccountChanged)?
        .credential;
        // Queued lock waiters must not start another exchange while shutdown drains.
        if self.closing.load(Ordering::Acquire) {
            return Err(anyhow::anyhow!("OAuth client is shutting down").into());
        }
        if let Some(rejected) = rejected_access {
            if current.access_token != rejected {
                return Ok(current);
            }
        } else if current.expires_at > now().saturating_add(300) {
            return Ok(current);
        }
        tracing::debug!(account_id = id, event = "credential_refresh_started");
        let response = self
            .client
            .post(format!("{}/oauth/token", self.issuer))
            .form(&[
                ("grant_type", "refresh_token"),
                ("client_id", CLIENT_ID),
                ("refresh_token", current.refresh_token.as_str()),
            ])
            .send()
            .await
            .inspect_err(|_| {
                tracing::warn!(
                    account_id = id,
                    phase = "transport",
                    event = "credential_refresh_failed"
                );
            })
            .map_err(|_| RefreshError::Other(anyhow::anyhow!("OAuth refresh network error")))?;
        let status = response.status();
        if matches!(status.as_u16(), 400 | 401 | 403) {
            let s = store.clone();
            let revoked_id = id.to_owned();
            let generation = current.generation.clone();
            let still_current = tokio::task::spawn_blocking(move || {
                s.mark_needs_login(&revoked_id, &generation)?;
                s.lease_for_generation(&revoked_id, &generation)
                    .map(|lease| lease.is_some())
            })
            .await
            .map_err(|error| RefreshError::Storage(error.into()))?
            .map_err(RefreshError::Storage)?;
            if !still_current {
                return Err(RefreshError::AccountChanged);
            }
            tracing::warn!(
                account_id = id,
                status = status.as_u16(),
                event = "credential_revoked"
            );
            return Err(RefreshError::Revoked);
        }
        if !status.is_success() {
            tracing::warn!(
                account_id = id,
                status = status.as_u16(),
                phase = "http",
                event = "credential_refresh_failed"
            );
            return Err(anyhow::anyhow!("OAuth refresh failed (HTTP {})", status.as_u16()).into());
        }
        let tokens: Tokens = read_json(response)
            .await
            .inspect_err(|_| {
                tracing::warn!(
                    account_id = id,
                    phase = "response",
                    event = "credential_refresh_failed"
                );
            })
            .context("Invalid OAuth refresh response")?;
        let refreshed = credential_from_tokens(tokens, Some(&current))?;
        let s = store.clone();
        let refresh_id = id.to_owned();
        let result = tokio::task::spawn_blocking(move || {
            s.finish_refresh_for_generation(
                &refresh_id,
                &current.generation,
                &current.access_token,
                refreshed,
            )
        })
        .await
        .map_err(|error| RefreshError::Storage(error.into()))?
        .map_err(RefreshError::Storage)?
        .ok_or(RefreshError::AccountChanged);
        match &result {
            Ok(_) => tracing::info!(account_id = id, event = "credential_refresh_completed"),
            Err(_) => tracing::warn!(
                account_id = id,
                phase = "storage",
                event = "credential_refresh_failed"
            ),
        }
        result
    }
    pub async fn browser_login(&self) -> Result<Credential> {
        let verifier = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>());
        let state = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>());
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let mut url = url::Url::parse(&format!("{}/oauth/authorize", self.issuer))?;
        url.query_pairs_mut().extend_pairs([
            ("response_type", "code"),
            ("client_id", CLIENT_ID),
            ("redirect_uri", REDIRECT),
            ("scope", "openid profile email offline_access"),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("id_token_add_organizations", "true"),
            ("codex_cli_simplified_flow", "true"),
            ("state", state.as_str()),
            ("originator", "codex_cli_rs"),
        ]);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:1455")
            .await
            .context("OAuth callback port 1455 is busy; close other login flows or use --device")?;
        let (tx, rx) = oneshot::channel();
        let callback = Callback {
            expected: state,
            sender: Arc::new(Mutex::new(Some(tx))),
        };
        let app = Router::new()
            .route("/auth/callback", get(oauth_callback))
            .with_state(callback);
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
            code = tokio::time::timeout(Duration::from_secs(300), rx) => code.context("OAuth login timed out")?.context("OAuth callback closed")?,
            _ = tokio::signal::ctrl_c() => Err("Login cancelled".into()),
        };
        let _ = stop_tx.send(());
        let _ = tokio::time::timeout(Duration::from_secs(2), server).await;
        let code = code.map_err(|_| anyhow::anyhow!("Login was declined or cancelled"))?;
        let tokens = self
            .tokens(&[
                ("grant_type", "authorization_code"),
                ("client_id", CLIENT_ID),
                ("redirect_uri", REDIRECT),
                ("code", &code),
                ("code_verifier", &verifier),
            ])
            .await?;
        credential_from_tokens(tokens, None)
    }
    pub async fn device_login(&self) -> Result<Credential> {
        #[derive(Deserialize)]
        struct Device {
            device_auth_id: String,
            user_code: String,
            #[serde(default)]
            interval: Value,
        }
        #[derive(Deserialize)]
        struct Code {
            authorization_code: String,
            code_verifier: String,
        }
        let response = self
            .client
            .post(format!("{}/api/accounts/deviceauth/usercode", self.issuer))
            .json(&json!({"client_id":CLIENT_ID}))
            .send()
            .await
            .context("Device login network error")?;
        if !response.status().is_success() {
            bail!(
                "Device login failed (HTTP {}); enable device login in ChatGPT security settings or use browser login",
                response.status().as_u16()
            );
        }
        let device: Device = read_json(response)
            .await
            .context("Invalid device-code response")?;
        println!(
            "Open {}/codex/device and enter: {}",
            self.issuer, device.user_code
        );
        let interval = device
            .interval
            .as_u64()
            .or_else(|| device.interval.as_str().and_then(|s| s.parse().ok()))
            .unwrap_or(5)
            .clamp(1, 60);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(300);
        loop {
            tokio::select! { _ = tokio::time::sleep(Duration::from_secs(interval + 3)) => {}, _ = tokio::signal::ctrl_c() => bail!("Login cancelled") }
            if tokio::time::Instant::now() >= deadline {
                bail!("Device login timed out");
            }
            let response = self
                .client
                .post(format!("{}/api/accounts/deviceauth/token", self.issuer))
                .json(&json!({"device_auth_id":device.device_auth_id,"user_code":device.user_code}))
                .send()
                .await
                .context("Device polling network error")?;
            if matches!(response.status().as_u16(), 403 | 404) {
                continue;
            }
            if !response.status().is_success() {
                bail!(
                    "Device login polling failed (HTTP {})",
                    response.status().as_u16()
                );
            }
            let code: Code = read_json(response)
                .await
                .context("Invalid device login result")?;
            let redirect = format!("{}/deviceauth/callback", self.issuer);
            return credential_from_tokens(
                self.tokens(&[
                    ("grant_type", "authorization_code"),
                    ("client_id", CLIENT_ID),
                    ("redirect_uri", &redirect),
                    ("code", &code.authorization_code),
                    ("code_verifier", &code.code_verifier),
                ])
                .await?,
                None,
            );
        }
    }
}

async fn read_json<T: DeserializeOwned>(response: reqwest::Response) -> Result<T> {
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("OAuth response read failed")?;
        if bytes.len().saturating_add(chunk.len()) > 512 * 1024 {
            bail!("OAuth response exceeds size limit");
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).context("Invalid OAuth JSON response")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(store: &Store) -> crate::accounts::Account {
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
            .unwrap()
    }

    #[tokio::test]
    async fn refresh_tracker_prunes_completed_tasks_and_bounds_admission() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let selected = account(&store);
        let auth = AuthClient::with_issuer("http://127.0.0.1:1".into()).unwrap();
        for _ in 0..128 {
            auth.refresh_for_generation(&store, &selected.id, &selected.generation, None)
                .await
                .unwrap();
            assert!(auth.refresh_tasks.lock().await.len() <= 2);
        }
        let permits = auth
            .refresh_admission
            .clone()
            .acquire_many_owned(64)
            .await
            .unwrap();
        assert!(auth.refresh(&store, &selected.id, None).await.is_err());
        drop(permits);
        auth.shutdown().await;
        assert!(auth.refresh_tasks.lock().await.is_empty());
        assert!(auth.refresh(&store, &selected.id, None).await.is_err());
    }

    #[tokio::test]
    async fn shutdown_stops_lock_waiters_without_waiting_for_another_process() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let selected = account(&store);
        let guard = store.refresh_lock(&selected.id).unwrap();
        let auth = AuthClient::with_issuer("http://127.0.0.1:1".into()).unwrap();
        let caller = {
            let auth = auth.clone();
            let store = store.clone();
            let id = selected.id.clone();
            tokio::spawn(async move { auth.refresh(&store, &id, None).await })
        };
        while auth.refresh_admission.available_permits() == 64 {
            tokio::task::yield_now().await;
        }
        tokio::time::timeout(Duration::from_secs(2), auth.shutdown())
            .await
            .unwrap();
        assert!(caller.await.unwrap().is_err());
        drop(guard);
    }
}
