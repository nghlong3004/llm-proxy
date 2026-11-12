//! HTTP implementation of the provider Strategy; no agent-specific response encoding.
use super::{quota, reasoning::ReasoningSigner, request, search, sse::Decoder, stream::Translator};
use crate::{
    accounts::{Account, Credential, Store, now},
    authentication::{AuthClient, RefreshError},
    config::Config,
    domain::error::{FailureKind, ProxyError},
    domain::*,
    providers::Provider,
};
use axum::http::{HeaderMap, HeaderValue};
use futures_util::{StreamExt, future::BoxFuture};
use serde_json::Value;
use std::{sync::Arc, time::Duration};
#[derive(Clone)]
pub struct Codex {
    store: Store,
    config: Arc<Config>,
    auth: AuthClient,
    http: reqwest::Client,
    signer: ReasoningSigner,
}
impl Codex {
    pub fn new(store: Store, config: Arc<Config>) -> anyhow::Result<Self> {
        Self::with_auth(store, config, AuthClient::new()?)
    }
    /// Supply an OAuth client explicitly for integration tests or another issuer.
    pub fn with_auth(store: Store, config: Arc<Config>, auth: AuthClient) -> anyhow::Result<Self> {
        let signer = ReasoningSigner::new(store.reasoning_key()?);
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .read_timeout(Duration::from_secs(300))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            store,
            config,
            auth,
            http,
            signer,
        })
    }
    async fn send(
        &self,
        url: &str,
        body: &Value,
        credential: &Credential,
        context: &RequestContext,
        lite: bool,
        search: bool,
    ) -> Result<reqwest::Response, ProxyError> {
        let headers = headers(credential, context, lite, search)?;
        tokio::time::timeout(
            Duration::from_secs(300),
            self.http.post(url).headers(headers).json(body).send(),
        )
        .await
        .map_err(|_| ProxyError::transport())?
        .map_err(|_| ProxyError::transport())
    }
    async fn start(
        self,
        request: Arc<GenerationRequest>,
        account: Account,
        context: RequestContext,
    ) -> Result<EventStream, ProxyError> {
        let model = self
            .config
            .resolve(&request.model)
            .map_err(|e| ProxyError::invalid(e.to_string()).with_code("unknown_model"))?;
        let mut credential = self
            .auth
            .refresh(&self.store, &account.id, None)
            .await
            .map_err(auth_error)?;
        if credential.generation != account.generation {
            return Err(account_changed());
        }
        let count_request = request.clone();
        let estimated =
            tokio::task::spawn_blocking(move || request::estimate_tokens(&count_request))
                .await
                .map_err(|_| ProxyError::protocol("Token estimator failed"))?;
        let query = search::query(&request);
        let body = if let Some(query) = &query {
            search::request_body(&request, &model.id, context.session_id.as_deref(), query)
        } else {
            request::translate(
                &request,
                &model,
                &account.id,
                &credential.generation,
                &self.signer,
                context.session_id.as_deref(),
            )?
        };
        let lite = body
            .pointer("/client_metadata/ws_request_header_x_openai_internal_codex_responses_lite")
            .and_then(Value::as_str)
            == Some("true");
        let url = if query.is_some() {
            format!(
                "{}/alpha/search",
                self.config
                    .codex_endpoint
                    .trim_end_matches('/')
                    .strip_suffix("/responses")
                    .unwrap_or(self.config.codex_endpoint.trim_end_matches('/'))
            )
        } else {
            self.config.codex_endpoint.clone()
        };
        tracing::info!(
            request_id = %context.request_id,
            account_id = %account.id,
            provider = "codex",
            requested_model = %request.model,
            model = body.get("model").and_then(|value| value.as_str()).unwrap_or(&model.id),
            reasoning_effort = body.pointer("/reasoning/effort").and_then(|value| value.as_str()),
            responses_lite = lite,
            event = "upstream_attempt"
        );
        let mut response = self
            .send(&url, &body, &credential, &context, lite, query.is_some())
            .await?;
        if response.status().as_u16() == 401 {
            // A definite authorization rejection is the only automatic same-account replay.
            drop(response);
            credential = self
                .auth
                .refresh(&self.store, &account.id, Some(&credential.access_token))
                .await
                .map_err(auth_error)?;
            if credential.generation != account.generation {
                return Err(account_changed());
            }
            response = self
                .send(&url, &body, &credential, &context, lite, query.is_some())
                .await?;
            if response.status().as_u16() == 401 {
                let store = self.store.clone();
                let id = account.id.clone();
                let generation = credential.generation.clone();
                tokio::task::spawn_blocking(move || store.mark_needs_login(&id, &generation))
                    .await
                    .map_err(|_| ProxyError::storage())?
                    .map_err(|_| ProxyError::storage())?;
            }
        }
        let status = response.status().as_u16();
        let response_headers = response.headers().clone();
        if status >= 400 {
            let bytes = read_bounded(response, 1024 * 1024).await?;
            let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            return Err(quota::classify(status, &value, &response_headers, now())
                .unwrap_or_else(|| ProxyError::protocol("Unexpected upstream error response")));
        }
        if let Some(query) = query {
            let bytes = read_bounded(response, 8 * 1024 * 1024).await?;
            let value: Value = serde_json::from_slice(&bytes)
                .map_err(|_| ProxyError::protocol("Invalid search response JSON"))?;
            if let Some(error) = quota::classify(status, &value, &response_headers, now()) {
                return Err(error);
            }
            let events = search::events(value, &request, &query, estimated)?;
            return Ok(futures_util::stream::iter(events.into_iter().map(Ok)).boxed());
        }
        let actual_model = body
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or(&model.id)
            .to_owned();
        let mut translator = Translator::new(
            request.model.clone(),
            actual_model,
            account.id,
            credential.generation,
            self.signer,
            estimated,
        );
        let mut decoder = Decoder::default();
        let mut stream = response.bytes_stream();
        Ok(async_stream::try_stream! {
            while let Some(chunk) = tokio::time::timeout(Duration::from_secs(300), stream.next())
                .await.map_err(|_| ProxyError::transport())?
            {
                let chunk = chunk.map_err(|_| ProxyError::transport())?;
                for event in decoder.push(&chunk)? {
                    if let Some(error) = quota::classify(status, &event, &response_headers, now()) {
                        Err(error)?;
                    }
                    for event in translator.accept(&event)? { yield event; }
                    if translator.finished() { break; }
                }
                if translator.finished() { break; }
            }
            if !translator.finished() {
                decoder.finish()?;
                Err(ProxyError::protocol("Codex stream ended without a completion event"))?;
            }
        }
        .boxed())
    }
}
impl Provider for Codex {
    fn name(&self) -> &'static str {
        "codex"
    }
    fn generate(
        &self,
        request: Arc<GenerationRequest>,
        account: Account,
        context: RequestContext,
    ) -> BoxFuture<'static, Result<EventStream, ProxyError>> {
        let this = self.clone();
        Box::pin(async move { this.start(request, account, context).await })
    }
}
fn auth_error(error: RefreshError) -> ProxyError {
    match error {
        RefreshError::Revoked => ProxyError::new(
            401,
            FailureKind::Authentication,
            "Account session revoked; sign in again",
        ),
        RefreshError::Other(_) => ProxyError::new(
            502,
            FailureKind::Transport,
            "Unable to renew the account session; no request was replayed",
        ),
    }
}
fn account_changed() -> ProxyError {
    ProxyError::new(
        409,
        FailureKind::AccountChanged,
        "Account changed before dispatch; reselect its current generation",
    )
}
pub fn headers(
    credential: &Credential,
    context: &RequestContext,
    lite: bool,
    search: bool,
) -> Result<HeaderMap, ProxyError> {
    let mut headers = HeaderMap::new();
    for (name, value) in [
        ("content-type", "application/json".to_owned()),
        (
            "accept",
            if search {
                "application/json"
            } else {
                "text/event-stream"
            }
            .into(),
        ),
        (
            "authorization",
            format!("Bearer {}", credential.access_token),
        ),
        ("chatgpt-account-id", credential.account_id.clone()),
        (
            "originator",
            if lite || search {
                "codex_cli_rs"
            } else {
                "llm-proxy"
            }
            .into(),
        ),
        ("openai-beta", "responses=experimental".into()),
        ("x-codex-beta-features", "remote_compaction_v2".into()),
        (
            "user-agent",
            if lite || search {
                "codex_cli_rs".into()
            } else {
                format!("llm-proxy/{}", env!("CARGO_PKG_VERSION"))
            },
        ),
    ] {
        let mut value = HeaderValue::from_str(&value)
            .map_err(|_| ProxyError::invalid("Invalid upstream header value"))?;
        if name == "authorization" {
            value.set_sensitive(true);
        }
        headers.insert(name, value);
    }
    if lite {
        headers.insert(
            "x-openai-internal-codex-responses-lite",
            HeaderValue::from_static("true"),
        );
    }
    if let Some(session) = &context.session_id {
        headers.insert(
            "x-codex-window-id",
            HeaderValue::from_str(&format!("{session}:0"))
                .map_err(|_| ProxyError::invalid("Invalid session ID"))?,
        );
        for name in ["session-id", "x-client-request-id"] {
            headers.insert(
                name,
                HeaderValue::from_str(session)
                    .map_err(|_| ProxyError::invalid("Invalid session ID"))?,
            );
        }
    }
    Ok(headers)
}
pub async fn read_bounded(
    response: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, ProxyError> {
    let mut out = vec![];
    let mut stream = response.bytes_stream();
    while let Some(bytes) = tokio::time::timeout(Duration::from_secs(300), stream.next())
        .await
        .map_err(|_| ProxyError::transport())?
    {
        let bytes = bytes.map_err(|_| ProxyError::transport())?;
        if out.len().saturating_add(bytes.len()) > limit {
            return Err(ProxyError::protocol("Upstream body exceeds size limit"));
        }
        out.extend_from_slice(&bytes);
    }
    Ok(out)
}
