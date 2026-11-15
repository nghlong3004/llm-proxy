//! Cancellable native HTTP provider; the only automatic replay follows a definite HTTP 401.
use super::{error, reasoning::ReasoningSigner, request, stream::Translator};
use crate::{
    accounts::{Credential, Store},
    adapters::outbound::{http::read_bounded, sse::Decoder},
    authentication::{AuthClient, RefreshError},
    config::Config,
    domain::{
        EventStream, GenerationRequest, RequestContext,
        account::AccountIdentity,
        error::{FailureKind, ProxyError},
    },
    ports::{
        llm_provider::Provider,
        request_routing::{RequestRoute, RouteOptions},
    },
};
use axum::body::Bytes;
use futures_util::{StreamExt, future::BoxFuture};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue, USER_AGENT};
use serde_json::Value;
use std::{sync::Arc, time::Duration};

const MAX_STREAM_BYTES: usize = 16 * 1024 * 1024;
const MAX_REQUEST_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone)]
pub struct Agy {
    store: Store,
    config: Arc<Config>,
    auth: AuthClient,
    http: reqwest::Client,
    signer: ReasoningSigner,
    preparation: Arc<tokio::sync::Semaphore>,
}

impl Agy {
    pub fn new(store: Store, config: Arc<Config>) -> anyhow::Result<Self> {
        let auth = AuthClient::agy_with_state_dir(store.root())?;
        Self::with_auth(store, config, auth)
    }

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
            preparation: Arc::new(tokio::sync::Semaphore::new(32)),
        })
    }

    async fn send(
        &self,
        url: &str,
        body: &Bytes,
        credential: &Credential,
        session: &str,
        thinking: bool,
    ) -> Result<reqwest::Response, ProxyError> {
        let mut headers = HeaderMap::new();
        let mut authorization =
            HeaderValue::from_str(&format!("Bearer {}", credential.access_token))
                .map_err(|_| ProxyError::invalid("Invalid AGY authorization header"))?;
        authorization.set_sensitive(true);
        headers.insert(AUTHORIZATION, authorization);
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        headers.insert("x-client-name", HeaderValue::from_static("antigravity"));
        headers.insert(
            USER_AGENT,
            HeaderValue::from_static(concat!("llm-proxy/", env!("CARGO_PKG_VERSION"))),
        );
        headers.insert(
            "x-machine-session-id",
            HeaderValue::from_str(session)
                .map_err(|_| ProxyError::invalid("Invalid AGY session identity"))?,
        );
        if thinking {
            headers.insert(
                "anthropic-beta",
                HeaderValue::from_static("interleaved-thinking-2025-05-14"),
            );
        }
        tokio::time::timeout(
            Duration::from_secs(30),
            self.http
                .post(url)
                .headers(headers)
                .body(body.clone())
                .send(),
        )
        .await
        .map_err(|_| ProxyError::transport())?
        .map_err(|_| ProxyError::transport())
    }

    async fn start(
        self,
        request: Arc<GenerationRequest>,
        route: Arc<RequestRoute>,
        account: AccountIdentity,
        context: RequestContext,
    ) -> Result<EventStream, ProxyError> {
        self.validate_request(&request, &route)?;
        if account.provider != "agy" {
            return Err(ProxyError::invalid("AGY requires an AGY account"));
        }
        let permit = self.preparation.clone().try_acquire_owned().map_err(|_| {
            ProxyError::new(
                503,
                FailureKind::Upstream,
                "AGY preparation capacity reached",
            )
            .with_code("agy_preparation_capacity")
        })?;
        let prepared_request = request.clone();
        let prepared_account = account.clone();
        let config = self.config.clone();
        let signer = self.signer.clone();
        let prepared_context = context.clone();
        let canonical = route.model.clone();
        let (body, session) = tokio::task::spawn_blocking(move || {
            // An uncancellable blocking job owns its capacity until it actually completes.
            let _permit = permit;
            let value = request::translate(
                &prepared_request,
                &canonical,
                &prepared_account,
                &config,
                &signer,
                &prepared_context,
            )?;
            let body = super::encode_bounded(&value, MAX_REQUEST_BYTES).ok_or_else(|| {
                ProxyError::invalid("AGY request exceeds 32 MiB or cannot be encoded")
            })?;
            let session = value
                .pointer("/request/sessionId")
                .and_then(Value::as_str)
                .ok_or_else(|| ProxyError::protocol("Missing AGY session identity"))?
                .to_owned();
            Ok::<_, ProxyError>((Bytes::from(body), session))
        })
        .await
        .map_err(|_| ProxyError::protocol("AGY request preparation failed"))??;
        // Revalidate after asynchronous preparation, immediately before dispatch.
        let mut credential = self
            .auth
            .refresh_for_generation(&self.store, &account.id, &account.generation, None)
            .await
            .map_err(auth_error)?;
        validate_identity(&credential, &account)?;
        let url = format!(
            "{}/v1internal:streamGenerateContent?alt=sse",
            self.config.agy_endpoint.trim_end_matches('/'),
        );
        let thinking = route.model.starts_with("claude-") && route.model.ends_with("-thinking");
        let mut response = self
            .send(&url, &body, &credential, &session, thinking)
            .await?;
        if response.status().as_u16() == 401 {
            drop(response);
            credential = self
                .auth
                .refresh_for_generation(
                    &self.store,
                    &account.id,
                    &account.generation,
                    Some(&credential.access_token),
                )
                .await
                .map_err(auth_error)?;
            validate_identity(&credential, &account)?;
            response = self
                .send(&url, &body, &credential, &session, thinking)
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
        if !(200..300).contains(&status) {
            let bytes = read_bounded(response, 1024 * 1024).await?;
            let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            return Err(error::classify(status, &value)
                .unwrap_or_else(|| ProxyError::protocol("Unexpected AGY HTTP status")));
        }
        let event_stream = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("text/event-stream"));
        if !event_stream {
            return Err(ProxyError::protocol("AGY did not return an SSE response"));
        }
        let mut translator = Translator::new(
            request.model.clone(),
            route.model.clone(),
            account.id,
            credential.generation,
            self.signer,
        )
        .with_parallel_tools(request.parallel_tools);
        let mut decoder = Decoder::default();
        let mut upstream = response.bytes_stream();
        let mut received = 0usize;
        Ok(async_stream::try_stream! {
            'read: while let Some(chunk) = tokio::time::timeout(Duration::from_secs(300), upstream.next())
                .await.map_err(|_| ProxyError::transport())?
            {
                let chunk = chunk.map_err(|_| ProxyError::transport())?;
                // Bound both total work and each decoder result, independent of network chunking.
                for bytes in chunk.chunks(16 * 1024) {
                    received = received.saturating_add(bytes.len());
                    if received > MAX_STREAM_BYTES {
                        Err(ProxyError::protocol("AGY stream exceeds 16 MiB"))?;
                    }
                    for value in decoder.push(bytes)? {
                        for event in translator.accept(&value)? {
                            yield event;
                        }
                        if translator.finished() {
                            break 'read;
                        }
                    }
                }
            }
            if !translator.finished() {
                decoder.finish()?;
                Err(ProxyError::protocol("AGY stream ended without a finish reason"))?;
            }
        }.boxed())
    }
}

impl Provider for Agy {
    fn name(&self) -> &'static str {
        "agy"
    }

    fn shutdown(&self) -> BoxFuture<'_, ()> {
        Box::pin(self.auth.shutdown())
    }

    fn validate_request(
        &self,
        request: &GenerationRequest,
        route: &RequestRoute,
    ) -> Result<(), ProxyError> {
        if route.provider != "agy" || route.options != RouteOptions::Standard {
            return Err(ProxyError::invalid("AGY route provider/options mismatch")
                .with_code("invalid_request_route"));
        }
        request::validate(request, &route.model)
    }

    fn generate(
        &self,
        request: Arc<GenerationRequest>,
        route: Arc<RequestRoute>,
        account: AccountIdentity,
        context: RequestContext,
    ) -> BoxFuture<'static, Result<EventStream, ProxyError>> {
        let this = self.clone();
        Box::pin(async move { this.start(request, route, account, context).await })
    }
}

fn validate_identity(credential: &Credential, account: &AccountIdentity) -> Result<(), ProxyError> {
    if credential.generation != account.generation || credential.account_id != account.workspace {
        return Err(account_changed());
    }
    Ok(())
}

fn account_changed() -> ProxyError {
    ProxyError::new(
        409,
        FailureKind::AccountChanged,
        "AGY account changed before dispatch",
    )
}

fn auth_error(error: RefreshError) -> ProxyError {
    match error {
        RefreshError::AccountChanged => account_changed(),
        RefreshError::Storage(_) => ProxyError::storage(),
        RefreshError::Revoked => ProxyError::new(
            401,
            FailureKind::Authentication,
            "AGY session revoked; sign in again",
        ),
        RefreshError::Other(_) => ProxyError::new(
            502,
            FailureKind::Transport,
            "Unable to renew AGY session; no request was replayed",
        ),
    }
}
