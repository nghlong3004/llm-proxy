//! Codex-specific OAuth endpoints, grants and browser/device sign-in.
use super::{
    callback::{Callback, oauth_callback},
    oauth::{AuthClient, Flavor, RefreshError, read_json},
    tokens::{Tokens, credential_from_tokens},
};
use crate::accounts::Credential;
use anyhow::{Context, Result, bail};
use axum::{Router, routing::get};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{sync::Arc, time::Duration};
use tokio::sync::{Mutex, oneshot};

pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const ISSUER: &str = "https://auth.openai.com";
const REDIRECT: &str = "http://localhost:1455/auth/callback";
impl AuthClient {
    pub fn new() -> Result<Self> {
        Self::with_issuer(ISSUER.into())
    }
    pub fn with_issuer(issuer: String) -> Result<Self> {
        crate::config::validate_endpoint(&issuer)?;
        Self::with_flavor(issuer, Flavor::Codex)
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
    pub(super) async fn codex_refresh(
        &self,
        current: &Credential,
    ) -> Result<Credential, RefreshError> {
        let response = self
            .client
            .post(format!("{}/oauth/token", self.issuer))
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", current.refresh_token.as_str()),
                ("client_id", CLIENT_ID),
            ])
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("OAuth refresh network error"))?;
        let status = response.status();
        if matches!(status.as_u16(), 400 | 401 | 403) {
            return Err(RefreshError::Revoked);
        }
        if !status.is_success() {
            return Err(anyhow::anyhow!("OAuth refresh failed (HTTP {})", status.as_u16()).into());
        }
        let tokens: Tokens = read_json(response)
            .await
            .context("Invalid OAuth refresh response")?;
        credential_from_tokens(tokens, Some(current)).map_err(RefreshError::from)
    }

    pub async fn browser_login(&self) -> Result<Credential> {
        if !matches!(self.flavor, Flavor::Codex) {
            bail!("Use AGY browser login for this OAuth client");
        }
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
        if !matches!(self.flavor, Flavor::Codex) {
            bail!("AGY device login is not supported; use browser login");
        }
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
