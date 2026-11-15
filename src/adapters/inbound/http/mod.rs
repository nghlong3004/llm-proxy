//! Inbound HTTP adapter: parsing, admission, delivery and cancellation ownership.
pub mod anthropic;
use crate::{
    application::{Application, WorkPermit},
    domain::{
        GenerationEvent, RequestContext,
        accumulator::Accumulator,
        error::{FailureKind, ProxyError},
    },
    logging::RequestLog,
    ports::protocol_adapter::ProtocolAdapter,
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
use serde_json::{Value, json};
use std::{convert::Infallible, sync::Arc, time::Duration};
use tokio::sync::Semaphore;

#[derive(Clone)]
pub struct AppState {
    pub application: Application,
    pub protocol: Arc<dyn ProtocolAdapter>,
    permits: Arc<Semaphore>,
}
impl AppState {
    pub fn new(application: Application, protocol: Arc<dyn ProtocolAdapter>) -> Self {
        Self {
            application,
            protocol,
            permits: Arc::new(Semaphore::new(32)),
        }
    }
    pub fn shutdown(&self) {
        self.application.shutdown();
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
fn admit(state: &AppState) -> Result<WorkPermit, ProxyError> {
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
fn retain_admission(response: Response, permit: WorkPermit) -> Response {
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
    let permit = admit(&state)?;
    let headers = request.headers().clone();
    let value = read_json(&state, request).await?;
    let purpose = state.protocol.request_purpose(&value);
    let mut request = state.protocol.decode(value)?;
    request.purpose = purpose;
    let stream = request.stream;
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
        session_id,
    };
    let mut events = state
        .application
        .generate(request, context, permit.clone())
        .await?;
    let adapter = state.protocol.clone();
    let response = if stream {
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
                            let frame = anthropic::sse::encode(&name, &value);
                            yield Ok::<Bytes, Infallible>(Bytes::from(frame));
                        }
                        if finished {
                            break;
                        }
                    }
                    Some(Err(error)) => {
                        log.fail(&error);
                        for (name, value) in adapter.encode_stream_error(&error) {
                            let frame = anthropic::sse::encode(&name, &value);
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
    let count = state.application.count_tokens(request, permit).await?;
    Ok(Json(json!({"input_tokens":count})))
}
async fn models(State(state): State<AppState>) -> Json<Value> {
    let models = state
        .application
        .config
        .catalog()
        .into_iter()
        .map(|id| json!({ "id": id, "object": "model", "created": 0, "owned_by": "llm-proxy" }))
        .collect::<Vec<_>>();
    Json(json!({ "object": "list", "data": models }))
}
async fn health(State(state): State<AppState>) -> Response {
    match state.application.health().await {
        Ok(status) => Json(status).into_response(),
        Err(error) => error_response(state.protocol.as_ref(), error),
    }
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
