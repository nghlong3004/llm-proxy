//! Prove a second client protocol and a non-Codex provider need no router changes.
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use futures_util::{StreamExt, future::BoxFuture};
use http_body_util::BodyExt;
use llm_proxy::{
    accounts::{Account, Credential, Store},
    adapters::inbound::http::router,
    config::Config,
    domain::error::ProxyError,
    domain::*,
    ports::llm_provider::{Provider, ProviderRegistry},
    ports::protocol_adapter::ProtocolAdapter,
};
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;
struct EchoBackend;
impl Provider for EchoBackend {
    fn name(&self) -> &'static str {
        "echo"
    }
    fn generate(
        &self,
        request: Arc<GenerationRequest>,
        account: Account,
        _: RequestContext,
    ) -> BoxFuture<'static, Result<EventStream, ProxyError>> {
        assert_eq!(account.provider, "echo");
        Box::pin(async move {
            Ok(futures_util::stream::iter(vec![
                Ok(GenerationEvent::Start {
                    id: "echo-id".into(),
                    model: request.model.clone(),
                    input_tokens: 0,
                }),
                Ok(GenerationEvent::BlockStart {
                    index: 0,
                    block: Content::Text(request.system.clone()),
                }),
                Ok(GenerationEvent::BlockEnd { index: 0 }),
                Ok(GenerationEvent::Finish {
                    usage: Usage::default(),
                    reason: StopReason::EndTurn,
                }),
            ])
            .boxed())
        })
    }
}
struct MinimalProtocol;
impl ProtocolAdapter for MinimalProtocol {
    fn decode(&self, value: Value) -> Result<GenerationRequest, ProxyError> {
        let text = value
            .get("input")
            .and_then(Value::as_str)
            .ok_or_else(|| ProxyError::invalid("input is required"))?;
        Ok(GenerationRequest {
            purpose: RequestPurpose::Conversation,
            model: "echo-model".into(),
            system: text.into(),
            messages: vec![],
            tools: vec![],
            tool_choice: ToolChoice::None,
            parallel_tools: false,
            effort: "none".into(),
            output_format: None,
            stream: value
                .get("stream")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            compact: false,
        })
    }
    fn encode_event(&self, _: GenerationEvent) -> Vec<(String, Value)> {
        vec![]
    }
    fn encode_response(&self, output: GenerationOutput) -> Value {
        match &output.content[0] {
            Content::Text(text) => json!({"answer":text}),
            _ => json!({"error":"unexpected"}),
        }
    }
    fn encode_error(&self, error: &ProxyError) -> Value {
        json!({"problem":error.message})
    }
    fn encode_stream_error(&self, error: &ProxyError) -> Vec<(String, Value)> {
        vec![("problem".into(), self.encode_error(error))]
    }
}
#[tokio::test]
async fn protocol_adapter_and_provider_strategy_can_be_replaced_independently() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let credential = |subject: &str| Credential {
        access_token: "test".into(),
        refresh_token: "test".into(),
        id_token: None,
        expires_at: u64::MAX,
        subject: subject.into(),
        account_id: "workspace".into(),
        generation: String::new(),
    };
    store.login("codex", credential("codex-user")).unwrap();
    store
        .login_for("echo", "echo", credential("echo-user"))
        .unwrap();
    let config = Config {
        default_model: Some("echo-model".into()),
        default_provider: "echo".into(),
        ..Default::default()
    };
    let mut providers = ProviderRegistry::default();
    providers.register(Arc::new(EchoBackend));
    let mut state = llm_proxy::bootstrap::http_state(store, config, providers);
    state.protocol = Arc::new(MinimalProtocol);
    let response = router(state)
        .oneshot(
            Request::post("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"input":"different client protocol"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        serde_json::from_slice::<Value>(&bytes).unwrap(),
        json!({"answer":"different client protocol"})
    );
}

#[tokio::test]
async fn replacement_ingress_owns_decode_error_shape() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let mut state =
        llm_proxy::bootstrap::http_state(store, Config::default(), ProviderRegistry::default());
    state.protocol = Arc::new(MinimalProtocol);
    let response = router(state)
        .oneshot(
            Request::post("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let value: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(value["problem"], "input is required");
    assert!(value.get("type").is_none());
}
struct FailingBackend;
impl Provider for FailingBackend {
    fn name(&self) -> &'static str {
        "echo"
    }
    fn generate(
        &self,
        _: Arc<GenerationRequest>,
        _: Account,
        _: RequestContext,
    ) -> BoxFuture<'static, Result<EventStream, ProxyError>> {
        Box::pin(async {
            Ok(futures_util::stream::iter(vec![Err(ProxyError::protocol("mock failure"))]).boxed())
        })
    }
}
#[tokio::test]
async fn replacement_ingress_owns_stream_error_shape() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store
        .login_for(
            "echo",
            "echo",
            Credential {
                access_token: "test".into(),
                refresh_token: "test".into(),
                id_token: None,
                expires_at: u64::MAX,
                subject: "u".into(),
                account_id: "w".into(),
                generation: String::new(),
            },
        )
        .unwrap();
    let mut registry = ProviderRegistry::default();
    registry.register(Arc::new(FailingBackend));
    let mut state = llm_proxy::bootstrap::http_state(
        store,
        Config {
            default_model: Some("echo-model".into()),
            default_provider: "echo".into(),
            ..Default::default()
        },
        registry,
    );
    state.protocol = Arc::new(MinimalProtocol);
    let response = router(state)
        .oneshot(
            Request::post("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"input":"hello","stream":true}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    let text = String::from_utf8(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    assert!(text.contains("event: problem"));
    assert!(text.contains("mock failure"));
    assert!(!text.contains("event: error"));
}
