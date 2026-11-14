//! HTTP delivery and lifecycle. Provider selection/retry lives in `routing`.
pub mod request_policy;
mod routing;
use crate::{
    accounts::{AccountStatus, Store, now},
    config::Config,
    domain::accumulator::Accumulator,
    domain::error::{FailureKind, ProxyError},
    domain::{GenerationEvent, RequestContext, RequestPurpose},
    logging::RequestLog,
    protocols::{ProtocolAdapter, anthropic::Anthropic},
    providers::ProviderRegistry,
};
use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, FromRequest, Request, State, rejection::JsonRejection},
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::StreamExt;
use request_policy::{ConfiguredRequestRoutingPolicy, RequestRoutingPolicy};
use serde_json::{Value, json};
use std::{convert::Infallible, sync::Arc, time::Duration};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};
type AdmissionPermit = Arc<OwnedSemaphorePermit>;
#[derive(Clone)]
pub struct AppState {
    pub store: Store,
    pub config: Arc<Config>,
    pub providers: ProviderRegistry,
    pub protocol: Arc<dyn ProtocolAdapter>,
    pub request_policy: Arc<dyn RequestRoutingPolicy>,
    permits: Arc<Semaphore>,
    shutdown: watch::Sender<bool>,
}
impl AppState {
    pub fn new(store: Store, config: Config, providers: ProviderRegistry) -> Self {
        let (shutdown, _) = watch::channel(false);
        let config = Arc::new(config);
        let request_policy = Arc::new(ConfiguredRequestRoutingPolicy::new(config.clone()));
        Self {
            store,
            config,
            providers,
            protocol: Arc::new(Anthropic),
            request_policy,
            permits: Arc::new(Semaphore::new(32)),
            shutdown,
        }
    }
    pub fn shutdown(&self) {
        self.shutdown.send_replace(true);
    }
}
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/v1/messages", post(messages))
        .route("/v1/messages/count_tokens", post(count_tokens))
        .route("/v1/models", get(models))
        .route("/health", get(health))
        .layer(DefaultBodyLimit::max(32 * 1024 * 1024))
        .with_state(state)
}
async fn messages(State(state): State<AppState>, request: Request) -> Response {
    let protocol = state.protocol.clone();
    let request_id = uuid::Uuid::new_v4().to_string();
    let mut log = Some(RequestLog::new(request_id.clone()));
    let response = match generate_response(state, request, &request_id, &mut log).await {
        Ok(response) => response,
        Err(error) => {
            if let Some(log) = log.as_mut() {
                log.fail(&error);
            }
            error_response(protocol.as_ref(), error)
        }
    };
    tag_response(response, &request_id)
}
fn tag_response(mut response: Response, request_id: &str) -> Response {
    if let Ok(value) = HeaderValue::from_str(request_id) {
        response.headers_mut().insert("x-request-id", value);
    }
    response
}
fn admit(state: &AppState) -> Result<AdmissionPermit, ProxyError> {
    state
        .permits
        .clone()
        .try_acquire_owned()
        .map(Arc::new)
        .map_err(|_| {
            ProxyError::new(
                503,
                FailureKind::NoAccounts,
                "Proxy concurrency limit reached; retry shortly",
            )
        })
}
async fn read_json(state: &AppState, request: Request) -> Result<Value, ProxyError> {
    let Json(value) = tokio::time::timeout(
        Duration::from_secs(30),
        Json::<Value>::from_request(request, state),
    )
    .await
    .map_err(|_| {
        ProxyError::new(408, FailureKind::InvalidRequest, "Request body timeout")
            .with_code("request_body_timeout")
    })?
    .map_err(json_error)?;
    Ok(value)
}
fn retain_admission(response: Response, permit: AdmissionPermit) -> Response {
    response.map(|body| {
        Body::from_stream(async_stream::stream! {
            let _permit = permit;
            let mut chunks = body.into_data_stream();
            while let Some(chunk) = chunks.next().await {
                yield chunk;
            }
        })
    })
}
async fn generate_response(
    state: AppState,
    request: Request,
    request_id: &str,
    log: &mut Option<RequestLog>,
) -> Result<Response, ProxyError> {
    // Admit before buffering JSON, not just before dispatching upstream.
    let permit = admit(&state)?;
    let headers = request.headers().clone();
    let value = read_json(&state, request).await?;
    let purpose = state.protocol.request_purpose(&value);
    let mut request = state.protocol.decode(value)?;
    request.purpose = purpose;
    state
        .config
        .resolve(&request.model)
        .map_err(|e| ProxyError::invalid(e.to_string()).with_code("unknown_model"))?;
    let route = state.request_policy.route(&request)?;
    if purpose == RequestPurpose::ActionReview || route.provider != state.config.default_provider {
        state.providers.get(&route.provider)?;
    }
    state
        .config
        .resolve(&route.model)
        .map_err(|e| ProxyError::invalid(e.to_string()).with_code("unknown_model"))?;
    let requested_model = request.model.clone();
    tracing::debug!(request_id, request_purpose = purpose.as_str(),
        requested_model = %requested_model, routed_model = %route.model,
        provider = %route.provider, event = "request_route_selected");
    let model_overridden = route.model != requested_model;
    request.model = route.model.clone();
    let request = Arc::new(request);
    let excluded = std::collections::HashSet::new();
    let provider_name = route.provider.clone();
    let store = state.store.clone();
    let preparation_permit = permit.clone();
    let account = tokio::task::spawn_blocking(move || {
        let _permit = preparation_permit;
        store.select_for_provider_at(&provider_name, &excluded, now())
    })
    .await
    .map_err(|_| ProxyError::storage())??
    .account;
    state.providers.get(&route.provider)?;
    let session_id = headers
        .get("x-claude-code-session-id")
        .and_then(|v| v.to_str().ok())
        .filter(|s| {
            !s.is_empty()
                && s.len() <= 256
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "_-.:".contains(c))
        })
        .map(str::to_owned);
    let context = RequestContext {
        request_id: request_id.to_owned(),
        session_id: if purpose == RequestPurpose::ActionReview {
            None
        } else {
            session_id
        },
    };
    let mut events = routing::generate(
        state.clone(),
        request.clone(),
        context,
        account,
        permit.clone(),
        route,
    )
    .map(move |event| {
        event.map(|event| match event {
            GenerationEvent::Start {
                id, input_tokens, ..
            } if model_overridden => GenerationEvent::Start {
                id,
                model: requested_model.clone(),
                input_tokens,
            },
            event => event,
        })
    });
    let adapter = state.protocol.clone();
    let response = if request.stream {
        let mut log = log
            .take()
            .ok_or_else(|| ProxyError::protocol("Missing request trace"))?;
        let body = async_stream::stream! {
            let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
            heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                let event = tokio::select! {
                    event = events.next() => event,
                    _ = heartbeat.tick() => Some(Ok(GenerationEvent::Ping)),
                };
                match event {
                    Some(Ok(event)) => {
                        let finished = matches!(event, GenerationEvent::Finish { .. });
                        if let GenerationEvent::Finish { usage, .. } = &event {
                            log.complete(usage);
                        }
                        for (name, value) in adapter.encode_event(event) {
                            let frame = crate::protocols::sse::encode(&name, &value);
                            yield Ok::<Bytes, Infallible>(Bytes::from(frame));
                        }
                        if finished { break; }
                    }
                    Some(Err(error)) => {
                        log.fail(&error);
                        for (name, value) in adapter.encode_stream_error(&error) {
                            let frame = crate::protocols::sse::encode(&name, &value);
                            yield Ok(Bytes::from(frame));
                        }
                        break;
                    }
                    None => break,
                }
            }
        };
        (
            [
                ("content-type", "text/event-stream"),
                ("cache-control", "no-cache"),
                ("x-accel-buffering", "no"),
            ],
            Body::from_stream(body),
        )
            .into_response()
    } else {
        let mut accumulator = Accumulator::default();
        while let Some(event) = events.next().await {
            accumulator.accept(event?)?;
        }
        let output = accumulator.finish()?;
        if let Some(log) = log.as_mut() {
            log.complete(&output.usage);
        }
        Json(adapter.encode_response(output)).into_response()
    };
    Ok(retain_admission(response, permit))
}
async fn count_tokens(State(state): State<AppState>, request: Request) -> Response {
    let request_id = uuid::Uuid::new_v4().to_string();
    let response = match token_count(&state, request).await {
        Ok(response) => response.into_response(),
        Err(error) => error_response(state.protocol.as_ref(), error),
    };
    tag_response(response, &request_id)
}
async fn token_count(state: &AppState, request: Request) -> Result<Json<Value>, ProxyError> {
    let permit = admit(state)?;
    let value = read_json(state, request).await?;
    let request = state.protocol.decode(value)?;
    state
        .config
        .resolve(&request.model)
        .map_err(|e| ProxyError::invalid(e.to_string()).with_code("unknown_model"))?;
    let count = tokio::task::spawn_blocking(move || {
        // A dropped HTTP waiter must not release capacity while this job still runs.
        let _permit = permit;
        crate::providers::codex::request::estimate_tokens(&request)
    })
    .await
    .map_err(|_| ProxyError::protocol("Token estimator failed"))?;
    Ok(Json(json!({"input_tokens":count})))
}
async fn models(State(state): State<AppState>) -> Json<Value> {
    Json(
        json!({"object":"list","data":state.config.catalog().iter().map(|id|json!({"id":id,"object":"model","created":0,"owned_by":"llm-proxy"})).collect::<Vec<_>>()}),
    )
}
async fn health(State(state): State<AppState>) -> Response {
    match health_status(&state).await {
        Ok(response) => response.into_response(),
        Err(error) => error_response(state.protocol.as_ref(), error),
    }
}
async fn health_status(state: &AppState) -> Result<Json<Value>, ProxyError> {
    let store = state.store.clone();
    let registry = tokio::task::spawn_blocking(move || store.list())
        .await
        .map_err(|_| ProxyError::storage())?
        .map_err(|_| ProxyError::storage())?;
    let usable = registry
        .accounts
        .iter()
        .filter(|a| a.provider == state.config.default_provider)
        .filter(|a| {
            matches!(a.status, AccountStatus::Available)
                || matches!(a.status,AccountStatus::QuotaExhausted{reset_at}if reset_at<=now())
        })
        .count();
    Ok(Json(
        json!({"status":"ok","ready":usable>0&&state.providers.get(&state.config.default_provider).is_ok(),"accounts":registry.accounts.len(),"usable_accounts":usable,"provider":state.config.default_provider}),
    ))
}
fn json_error(error: JsonRejection) -> ProxyError {
    if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
        ProxyError::new(
            413,
            FailureKind::InvalidRequest,
            "Request body exceeds 32 MiB",
        )
    } else {
        ProxyError::invalid("Expected a valid application/json request body")
    }
}

fn error_response(protocol: &dyn ProtocolAdapter, error: ProxyError) -> Response {
    let status = StatusCode::from_u16(error.status).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut response = (status, Json(protocol.encode_error(&error))).into_response();
    if let Some(value) = error
        .retry_after
        .and_then(|seconds| HeaderValue::from_str(&seconds.to_string()).ok())
    {
        response.headers_mut().insert("retry-after", value);
    }
    response
}
