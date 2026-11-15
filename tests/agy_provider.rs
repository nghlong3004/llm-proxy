//! Synthetic native AGY contracts; never reads installed credentials or calls a live provider.
use axum::{
    Json, Router,
    extract::{OriginalUri, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use futures_util::{StreamExt, TryStreamExt};
use llm_proxy::{
    accounts::{Credential, Store},
    adapters::outbound::agy::{
        account_usage::AgyAccountUsage, client::Agy, error, reasoning::ReasoningSigner,
        request::translate, stream::Translator,
    },
    authentication::AuthClient,
    config::Config,
    domain::{
        Content, GenerationEvent, GenerationRequest, Message, RequestContext, RequestPurpose, Role,
        StopReason, Tool, ToolChoice, accumulator::Accumulator, error::FailureKind,
    },
    ports::{account_usage::AccountUsageProvider, llm_provider::Provider},
};
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

const MODEL: &str = "claude-sonnet-4-5-thinking";

fn credential() -> Credential {
    Credential {
        access_token: "synthetic-proxy-google-token".into(),
        refresh_token: "synthetic-refresh".into(),
        id_token: None,
        expires_at: u64::MAX,
        subject: "synthetic-google-user".into(),
        account_id: "synthetic-google-project".into(),
        generation: String::new(),
    }
}

fn fixture() -> (
    tempfile::TempDir,
    Store,
    llm_proxy::domain::account::AccountIdentity,
) {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path()).unwrap();
    let account = store.login_for("agy", "synthetic", credential()).unwrap();
    (directory, store, account.identity())
}

fn request() -> GenerationRequest {
    GenerationRequest {
        purpose: RequestPurpose::Conversation,
        model: MODEL.into(),
        system: "Original system instruction; do not rewrite.".into(),
        messages: vec![Message {
            role: Role::User,
            content: vec![Content::Text("  original prompt\n".into())],
        }],
        tools: Vec::new(),
        tool_choice: ToolChoice::Auto,
        parallel_tools: true,
        effort: None,
        output_format: None,
        stream: true,
        compact: false,
    }
}

fn config() -> Config {
    Config {
        default_model: Some(MODEL.into()),
        ..Default::default()
    }
}

fn context() -> RequestContext {
    RequestContext {
        request_id: "synthetic-request".into(),
        session_id: Some("synthetic-conversation".into()),
    }
}

fn tool() -> Tool {
    Tool::Function {
        name: "read_file".into(),
        description: "Read a file".into(),
        parameters: json!({"type":"object", "properties":{"path":{"type":"string"}}, "required":["path"]}),
    }
}

fn translator() -> Translator {
    Translator::new(
        "client-alias".into(),
        MODEL.into(),
        "account".into(),
        "generation".into(),
        ReasoningSigner::new([3; 32]),
    )
}

#[test]
fn request_preserves_text_project_model_and_native_envelope() {
    let (_directory, _store, account) = fixture();
    let request = request();
    let body = translate(
        &request,
        MODEL,
        &account,
        &config(),
        &ReasoningSigner::new([3; 32]),
        &context(),
    )
    .unwrap();
    assert_eq!(body["project"], account.workspace);
    assert_eq!(body["model"], MODEL);
    assert_eq!(body["userAgent"], "antigravity");
    assert_eq!(body["requestType"], "agent");
    assert!(body["requestId"].as_str().unwrap().starts_with("agent-"));
    assert_eq!(
        body["request"]["systemInstruction"]["parts"][0]["text"],
        request.system
    );
    assert_eq!(
        body["request"]["contents"][0]["parts"][0]["text"],
        "  original prompt\n"
    );
    assert_eq!(
        body["request"]["generationConfig"],
        json!({"maxOutputTokens":40960,"thinkingConfig":{"include_thoughts":true,"thinking_budget":32000}})
    );
}

#[test]
fn session_identity_is_stable_scoped_and_not_prompt_derived() {
    let (_directory, _store, account) = fixture();
    let signer = ReasoningSigner::new([3; 32]);
    let original = translate(&request(), MODEL, &account, &config(), &signer, &context()).unwrap();
    let mut changed = request();
    changed.messages[0].content = vec![Content::Text("different prompt".into())];
    let same = translate(&changed, MODEL, &account, &config(), &signer, &context()).unwrap();
    assert_eq!(
        original["request"]["sessionId"],
        same["request"]["sessionId"]
    );
    let mut other = account.clone();
    other.workspace = "other-project".into();
    let other = translate(&request(), MODEL, &other, &config(), &signer, &context()).unwrap();
    assert_ne!(
        original["request"]["sessionId"],
        other["request"]["sessionId"]
    );
    assert_ne!(
        signer.session_id("a", "bc", "d"),
        signer.session_id("ab", "c", "d")
    );
}

#[test]
fn tools_preserve_function_name_id_error_json_and_required_schema() {
    let (_directory, _store, account) = fixture();
    let mut request = request();
    request.tools.push(tool());
    request.parallel_tools = false;
    request.messages.push(Message {
        role: Role::Assistant,
        content: vec![Content::ToolCall {
            id: "call-one".into(),
            name: "read_file".into(),
            arguments: json!({"path":"a"}),
        }],
    });
    request.messages.push(Message {
        role: Role::User,
        content: vec![Content::ToolResult {
            id: "call-one".into(),
            content: vec![Content::Text("{\"message\":\"missing\"}".into())],
            is_error: true,
        }],
    });
    let body = translate(
        &request,
        MODEL,
        &account,
        &config(),
        &ReasoningSigner::new([0; 32]),
        &context(),
    )
    .unwrap();
    assert_eq!(
        body.pointer("/request/contents/1/parts/0/functionCall"),
        Some(&json!({"name":"read_file","id":"call-one","args":{"path":"a"}}))
    );
    assert_eq!(
        body.pointer("/request/contents/2/parts/0/functionResponse"),
        Some(
            &json!({"name":"read_file","id":"call-one","response":{"result":{"message":"missing"},"error":true}})
        )
    );
    assert_eq!(
        body.pointer("/request/tools/0/functionDeclarations/0/parameters"),
        Some(&json!({"type":"OBJECT","properties":{"path":{"type":"STRING"}},"required":["path"]}))
    );
    assert_eq!(
        body.pointer("/request/toolConfig/functionCallingConfig/mode"),
        Some(&json!("VALIDATED"))
    );
}

#[test]
fn unsupported_features_and_invalid_names_fail_before_dispatch() {
    let (_directory, _store, account) = fixture();
    let signer = ReasoningSigner::new([0; 32]);
    let mut cases = Vec::new();
    let mut compact = request();
    compact.compact = true;
    cases.push(compact);
    let mut structured = request();
    structured.output_format = Some(json!({"type":"json_schema"}));
    cases.push(structured);
    for choice in [
        ToolChoice::None,
        ToolChoice::Required,
        ToolChoice::Named("read_file".into()),
    ] {
        let mut chosen = request();
        chosen.tool_choice = choice;
        cases.push(chosen);
    }
    let mut serial = request();
    serial.parallel_tools = false;
    serial.tools = vec![tool(), tool()];
    cases.push(serial);
    let mut invalid = request();
    invalid.tools = vec![Tool::Function {
        name: "invalid.name".into(),
        description: String::new(),
        parameters: json!({"type":"object"}),
    }];
    cases.push(invalid);
    let mut schema = request();
    schema.tools = vec![Tool::Function {
        name: "valid".into(),
        description: String::new(),
        parameters: json!({"type":"array","items":{"type":"string"}}),
    }];
    cases.push(schema);
    let mut hosted = request();
    hosted.tools = vec![Tool::WebSearch {
        name: "web_search".into(),
        allowed_domains: Vec::new(),
        blocked_domains: Vec::new(),
    }];
    cases.push(hosted);
    for request in cases {
        assert_eq!(
            translate(&request, MODEL, &account, &config(), &signer, &context())
                .unwrap_err()
                .kind,
            FailureKind::InvalidRequest
        );
    }
    let mut gemini = request();
    gemini.tools.push(tool());
    assert!(
        translate(
            &gemini,
            "gemini-2.5-pro",
            &account,
            &config(),
            &signer,
            &context()
        )
        .is_err()
    );
}

#[test]
fn rich_function_json_schema_preserves_nested_constraints_without_rewriting() {
    let (_directory, _store, account) = fixture();
    let parameters = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "options": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "limit": {"type": "integer", "minimum": 1, "default": 10},
                    "selection": {"anyOf": [{"type": "string"}, {"type": "null"}], "default": null}
                },
                "required": ["selection"]
            }
        },
        "required": ["options"]
    });
    let mut request = request();
    request.tools.push(Tool::Function {
        name: "rich_tool".into(),
        description: "Preserve the caller's full schema".into(),
        parameters: parameters.clone(),
    });
    let body = translate(
        &request,
        MODEL,
        &account,
        &config(),
        &ReasoningSigner::new([0; 32]),
        &context(),
    )
    .unwrap();
    let declaration = body
        .pointer("/request/tools/0/functionDeclarations/0")
        .unwrap();
    assert_eq!(declaration["parametersJsonSchema"], parameters);
    assert!(declaration.get("parameters").is_none());
}

#[test]
fn unsupported_tool_results_and_unknown_ids_are_not_silently_dropped() {
    let (_directory, _store, account) = fixture();
    let mut request = request();
    request.messages.push(Message {
        role: Role::Assistant,
        content: vec![Content::ToolCall {
            id: "one".into(),
            name: "read_file".into(),
            arguments: json!({}),
        }],
    });
    request.messages.push(Message {
        role: Role::User,
        content: vec![Content::ToolResult {
            id: "one".into(),
            content: vec![Content::Image {
                media_type: "image/png".into(),
                data: "AA==".into(),
            }],
            is_error: false,
        }],
    });
    assert!(
        translate(
            &request,
            MODEL,
            &account,
            &config(),
            &ReasoningSigner::new([0; 32]),
            &context()
        )
        .is_err()
    );
    request.messages[2].content = vec![Content::ToolResult {
        id: "unknown".into(),
        content: vec![],
        is_error: false,
    }];
    assert!(
        translate(
            &request,
            MODEL,
            &account,
            &config(),
            &ReasoningSigner::new([0; 32]),
            &context()
        )
        .is_err()
    );
}

#[test]
fn reasoning_replay_requires_exact_provider_account_generation_and_model_scope() {
    let signer = ReasoningSigner::new([3; 32]);
    let signature = signer
        .sign("account", "generation", MODEL, "signed original", "opaque")
        .unwrap();
    assert!(
        signer
            .verify(&signature, "account", "generation", MODEL)
            .is_some()
    );
    assert!(
        signer
            .verify(&signature, "other", "generation", MODEL)
            .is_none()
    );
    assert!(
        signer
            .verify(&signature, "account", "new-generation", MODEL)
            .is_none()
    );
    assert!(
        signer
            .verify(&signature, "account", "generation", "other-model")
            .is_none()
    );
    assert!(
        signer
            .verify(
                &signature.replace("llmp:agy:", "llmp:codex:"),
                "account",
                "generation",
                MODEL
            )
            .is_none()
    );
    let (_directory, _store, mut account) = fixture();
    account.id = "account".into();
    account.generation = "generation".into();
    let mut request = request();
    request.messages.push(Message {
        role: Role::Assistant,
        content: vec![
            Content::Thinking {
                summary: "tampered summary".into(),
                signature: Some(signature),
            },
            Content::Text("answer".into()),
        ],
    });
    let body = translate(&request, MODEL, &account, &config(), &signer, &context()).unwrap();
    assert_eq!(
        body.pointer("/request/contents/1/parts/0"),
        Some(&json!({"thought":true,"text":"signed original","thoughtSignature":"opaque"}))
    );
    account.generation = "new-generation".into();
    let body = translate(&request, MODEL, &account, &config(), &signer, &context()).unwrap();
    assert_eq!(
        body.pointer("/request/contents/1/parts/0"),
        Some(&json!({"text":"answer"}))
    );
}

#[test]
fn deltas_preserve_whitespace_sign_thoughts_and_report_total_prompt_usage() {
    let mut translator = translator();
    let mut events = translator.accept(&json!({"response":{"candidates":[{"content":{"role":"model","parts":[{"thought":true,"text":" reason "}]}}]}})).unwrap();
    events.extend(translator.accept(&json!({"candidates":[{"content":{"parts":[{"thought":true,"text":"","thoughtSignature":"opaque"},{"text":" hello"},{"text":"\nworld "}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":100,"cachedContentTokenCount":40,"candidatesTokenCount":5,"thoughtsTokenCount":7}})).unwrap());
    let mut accumulator = Accumulator::default();
    for event in events {
        accumulator.accept(event).unwrap();
    }
    let result = accumulator.finish().unwrap();
    assert_eq!(result.model, "client-alias");
    assert!(
        matches!(&result.content[0], Content::Thinking { summary, signature:Some(signature) } if summary == " reason " && ReasoningSigner::new([3;32]).verify(signature,"account","generation",MODEL).is_some())
    );
    assert!(matches!(&result.content[1], Content::Text(text) if text == " hello\nworld "));
    assert_eq!(
        (
            result.usage.input,
            result.usage.cached,
            result.usage.output,
            result.usage.reasoning
        ),
        (100, 40, 12, 7)
    );
    assert_eq!(result.reason, StopReason::EndTurn);
}

#[test]
fn thought_signature_is_emitted_once_with_the_complete_block() {
    let mut translator = translator();
    let mut events = translator
        .accept(&json!({"candidates":[{"content":{"parts":[
            {"thought":true,"text":"first","thoughtSignature":"initial"}
        ]}}]}))
        .unwrap();
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, GenerationEvent::Signature { .. }))
    );
    events.extend(
        translator
            .accept(&json!({"candidates":[{"content":{"parts":[
        {"thought":true,"text":" last","thoughtSignature":"final"}
    ]},"finishReason":"STOP"}]}))
            .unwrap(),
    );
    let signatures: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            GenerationEvent::Signature { signature, .. } => Some(signature),
            _ => None,
        })
        .collect();
    assert_eq!(signatures.len(), 1);
    let replay = ReasoningSigner::new([3; 32])
        .verify(signatures[0], "account", "generation", MODEL)
        .unwrap();
    assert_eq!(replay.summary, "first last");
    assert_eq!(replay.thought_signature, "final");
}

#[test]
fn tool_call_stop_precedence_and_max_tokens_are_explicit() {
    let events = translator().accept(&json!({"candidates":[{"content":{"parts":[{"functionCall":{"id":"one","name":"read_file","args":{"path":"a"}}}]},"finishReason":"STOP"}]})).unwrap();
    assert!(matches!(
        events.last(),
        Some(GenerationEvent::Finish {
            reason: StopReason::ToolUse,
            ..
        })
    ));
    assert!(events.iter().any(|event| matches!(event, GenerationEvent::ToolArgumentsDelta { json, .. } if json == "{\"path\":\"a\"}")));
    let events = translator()
        .accept(&json!({"candidates":[{"finishReason":"MAX_TOKENS"}]}))
        .unwrap();
    assert!(matches!(
        events.last(),
        Some(GenerationEvent::Finish {
            reason: StopReason::MaxTokens,
            ..
        })
    ));
}

#[test]
fn safety_errors_unknown_parts_and_invalid_usage_do_not_synthesize_completion() {
    for response in [
        json!({"promptFeedback":{"blockReason":"SAFETY"}}),
        json!({"candidates":[{"finishReason":"SAFETY"}]}),
        json!({"candidates":[{"content":{"parts":[{"inlineData":{"data":"x"}}]}}]}),
        json!({"usageMetadata":{"promptTokenCount":1,"cachedContentTokenCount":2}}),
        json!({"error":{"code":500,"message":"SECRET-DO-NOT-LOG"}}),
    ] {
        let mut translator = translator();
        let error = translator.accept(&response).unwrap_err();
        assert!(!error.message.contains("SECRET"));
        assert!(!translator.finished());
    }
}

#[test]
fn rate_limit_is_never_confirmed_quota_and_errors_are_sanitized() {
    for status in [429, 500, 403, 400] {
        let error = error::classify(
            status,
            &json!({"error":{"status":"RESOURCE_EXHAUSTED","message":"SECRET-token-prompt"}}),
        )
        .unwrap();
        assert!(!error.is_quota());
        assert!(!error.message.contains("SECRET"));
        assert_eq!(error.upstream_status, Some(status));
    }
    assert_eq!(
        error::classify(429, &Value::Null).unwrap().kind,
        FailureKind::RateLimit
    );
    assert_eq!(
        error::classify(500, &Value::Null).unwrap().kind,
        FailureKind::Upstream
    );
    assert_eq!(
        error::classify(401, &Value::Null).unwrap().kind,
        FailureKind::Authentication
    );
}

#[test]
fn agy_account_usage_is_known_but_unsupported() {
    let provider = AgyAccountUsage;
    assert_eq!(provider.name(), "agy");
    assert_eq!(
        serde_json::to_value(provider.empty_usage()).unwrap(),
        serde_json::to_value(provider.decode_cached(&json!({"anything":1})).unwrap()).unwrap()
    );
}

#[test]
fn serial_tool_policy_rejects_second_call_and_duplicate_ids_are_errors() {
    let first = json!({"candidates":[{"content":{"parts":[{"functionCall":{"id":"one","name":"read_file","args":{}}}]}}]});
    let second = json!({"candidates":[{"content":{"parts":[{"functionCall":{"id":"two","name":"read_file","args":{}}}]}}]});
    let mut serial = translator().with_parallel_tools(false);
    serial.accept(&first).unwrap();
    assert!(serial.accept(&second).is_err());
    let mut parallel = translator();
    parallel.accept(&first).unwrap();
    assert!(parallel.accept(&first).is_err());
}

#[test]
fn retained_output_and_escaped_signature_payloads_are_bounded() {
    let mut translator = translator();
    let frame = json!({"candidates":[{"content":{"parts":[{"text":"x".repeat(6*1024*1024)}]}}]});
    translator.accept(&frame).unwrap();
    translator.accept(&frame).unwrap();
    assert!(translator.accept(&frame).is_err());
    assert!(
        ReasoningSigner::new([0; 32])
            .sign("a", "g", MODEL, "", &"\0".repeat(2 * 1024 * 1024))
            .is_none()
    );
}

#[derive(Clone)]
struct Mock {
    status: u16,
    sse: String,
    calls: Arc<AtomicUsize>,
    refreshes: Arc<AtomicUsize>,
    seen: Arc<Mutex<Vec<(HeaderMap, Value)>>>,
    first_unauthorized: bool,
}

impl Mock {
    fn success() -> Self {
        Self {
            status: 200,
            sse: format!(
                "data: {}\n\n",
                json!({"response":{"candidates":[{"content":{"role":"model","parts":[{"text":"synthetic answer"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":9,"candidatesTokenCount":2}}})
            ),
            calls: Arc::new(AtomicUsize::new(0)),
            refreshes: Arc::new(AtomicUsize::new(0)),
            seen: Arc::new(Mutex::new(Vec::new())),
            first_unauthorized: false,
        }
    }
}

async fn generate(
    State(mock): State<Mock>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    assert_eq!(uri.path(), "/v1internal:streamGenerateContent");
    assert_eq!(uri.query(), Some("alt=sse"));
    let call = mock.calls.fetch_add(1, Ordering::SeqCst);
    mock.seen.lock().unwrap().push((headers, body));
    if mock.first_unauthorized && call == 0 {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":{"code":401}})),
        )
            .into_response();
    }
    (
        StatusCode::from_u16(mock.status).unwrap(),
        [("content-type", "text/event-stream")],
        mock.sse,
    )
        .into_response()
}

async fn refresh(State(mock): State<Mock>, body: String) -> Json<Value> {
    assert!(body.contains("grant_type=refresh_token"));
    mock.refreshes.fetch_add(1, Ordering::SeqCst);
    Json(json!({"access_token":"synthetic-renewed-google-token","expires_in":3600}))
}

async fn server(mock: Mock) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = Router::new()
        .route("/v1internal:streamGenerateContent", post(generate))
        .route("/token", post(refresh))
        .with_state(mock);
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (format!("http://{address}"), task)
}

fn provider(store: Store, url: &str) -> Agy {
    let auth = AuthClient::with_agy_endpoints(
        format!("{url}/token"),
        format!("{url}/userinfo"),
        url.into(),
        "synthetic-client-id".into(),
        "synthetic-client-secret".into(),
    )
    .unwrap();
    Agy::with_auth(
        store,
        Arc::new(Config {
            agy_endpoint: url.into(),
            ..config()
        }),
        auth,
    )
    .unwrap()
}

#[tokio::test]
#[ignore = "requires loopback sockets; synthetic only"]
async fn native_http_stream_and_buffered_requests_use_same_sse_contract_and_proxy_auth() {
    let mock = Mock::success();
    let (url, server) = server(mock.clone()).await;
    let (_directory, store, account) = fixture();
    let provider = provider(store, &url);
    for stream in [true, false] {
        let mut request = request();
        request.stream = stream;
        let events: Vec<_> = provider
            .generate(Arc::new(request), route(), account.clone(), context())
            .await
            .unwrap()
            .try_collect()
            .await
            .unwrap();
        assert!(matches!(
            events.last(),
            Some(GenerationEvent::Finish { .. })
        ));
    }
    let seen = mock.seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    for (headers, body) in seen.iter() {
        assert_eq!(
            headers["authorization"],
            "Bearer synthetic-proxy-google-token"
        );
        assert_eq!(headers["accept"], "text/event-stream");
        assert_eq!(headers["x-client-name"], "antigravity");
        assert_eq!(
            headers["x-machine-session-id"],
            body["request"]["sessionId"].as_str().unwrap()
        );
        assert_eq!(headers["anthropic-beta"], "interleaved-thinking-2025-05-14");
        assert!(
            headers["user-agent"]
                .to_str()
                .unwrap()
                .starts_with("llm-proxy/")
        );
        assert_eq!(body["model"], MODEL);
        assert_eq!(body["project"], "synthetic-google-project");
    }
    server.abort();
}

#[tokio::test]
#[ignore = "requires loopback sockets; synthetic only"]
async fn definite_401_refreshes_once_and_replays_only_same_account() {
    let mock = Mock {
        first_unauthorized: true,
        ..Mock::success()
    };
    let (url, server) = server(mock.clone()).await;
    let (_directory, store, account) = fixture();
    let provider = provider(store, &url);
    provider
        .generate(Arc::new(request()), route(), account, context())
        .await
        .unwrap()
        .try_collect::<Vec<_>>()
        .await
        .unwrap();
    assert_eq!(mock.calls.load(Ordering::SeqCst), 2);
    assert_eq!(mock.refreshes.load(Ordering::SeqCst), 1);
    let seen = mock.seen.lock().unwrap();
    assert_eq!(
        seen[1].0["authorization"],
        "Bearer synthetic-renewed-google-token"
    );
    assert_eq!(seen[0].1, seen[1].1);
    server.abort();
}

#[tokio::test]
#[ignore = "requires loopback sockets; synthetic only"]
async fn permission_rate_limit_and_server_error_do_not_refresh_or_retry() {
    for status in [403, 429, 500] {
        let mock = Mock {
            status,
            sse: json!({"error":{"message":"SECRET","status":"RESOURCE_EXHAUSTED"}}).to_string(),
            ..Mock::success()
        };
        let (url, server) = server(mock.clone()).await;
        let (_directory, store, account) = fixture();
        let result = provider(store, &url)
            .generate(Arc::new(request()), route(), account, context())
            .await;
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("unexpected success"),
        };
        assert!(!error.is_quota());
        assert!(!error.message.contains("SECRET"));
        assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
        assert_eq!(mock.refreshes.load(Ordering::SeqCst), 0);
        server.abort();
    }
}

#[tokio::test]
#[ignore = "requires loopback sockets; synthetic only"]
async fn incomplete_stream_is_protocol_error_without_replay() {
    let mock = Mock {
        sse: format!(
            "data: {}\n\n",
            json!({"candidates":[{"content":{"parts":[{"text":"partial"}]}}]})
        ),
        ..Mock::success()
    };
    let (url, server) = server(mock.clone()).await;
    let (_directory, store, account) = fixture();
    let mut events = provider(store, &url)
        .generate(Arc::new(request()), route(), account, context())
        .await
        .unwrap();
    let mut saw_text = false;
    let mut saw_error = false;
    while let Some(event) = events.next().await {
        match event {
            Ok(GenerationEvent::TextDelta { .. }) => saw_text = true,
            Err(error) => {
                assert_eq!(error.kind, FailureKind::Protocol);
                saw_error = true;
            }
            Ok(GenerationEvent::Finish { .. }) => panic!("EOF must not synthesize finish"),
            _ => {}
        }
    }
    assert!(saw_text && saw_error);
    assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
    server.abort();
}

#[tokio::test]
#[ignore = "requires loopback sockets; synthetic only"]
async fn repeated_401_marks_only_selected_generation_and_stops_after_one_retry() {
    let mock = Mock {
        status: 401,
        ..Mock::success()
    };
    let (url, server) = server(mock.clone()).await;
    let (_directory, store, account) = fixture();
    let result = provider(store.clone(), &url)
        .generate(Arc::new(request()), route(), account.clone(), context())
        .await;
    assert!(matches!(result,Err(error) if error.kind == FailureKind::Authentication));
    assert_eq!(mock.calls.load(Ordering::SeqCst), 2);
    assert_eq!(mock.refreshes.load(Ordering::SeqCst), 1);
    assert!(matches!(
        store.lease(&account.id).unwrap().account.status,
        llm_proxy::accounts::AccountStatus::NeedsLogin
    ));
    server.abort();
}

#[tokio::test]
#[ignore = "requires loopback sockets; synthetic only"]
async fn uncertain_dispatch_transport_failure_is_not_retried() {
    use tokio::io::AsyncReadExt;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = [0u8; 4096];
        assert!(socket.read(&mut bytes).await.unwrap() > 0);
        drop(socket);
        tokio::time::timeout(std::time::Duration::from_millis(200), listener.accept())
            .await
            .is_ok()
    });
    let (_directory, store, account) = fixture();
    let result = provider(store, &format!("http://{address}"))
        .generate(Arc::new(request()), route(), account, context())
        .await;
    assert!(matches!(result,Err(error) if error.kind == FailureKind::Transport));
    assert!(
        !task.await.unwrap(),
        "uncertain dispatch must not open another connection"
    );
}

#[tokio::test]
#[ignore = "requires loopback sockets; synthetic only"]
async fn dropping_event_stream_cancels_upstream_response_body() {
    struct Dropped(Arc<AtomicBool>);
    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let dropped = Arc::new(AtomicBool::new(false));
    let probe = dropped.clone();
    let router = Router::new().route("/v1internal:streamGenerateContent",post(move || {
        let guard = Dropped(probe.clone());
        async move {
            let stream = async_stream::stream! {
                let _guard = guard;
                yield Ok::<_,std::io::Error>(axum::body::Bytes::from_static(b"data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"partial\"}]}}]}\n\n"));
                std::future::pending::<()>().await;
            };
            ([("content-type","text/event-stream")],axum::body::Body::from_stream(stream))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let (_directory, store, account) = fixture();
    let mut stream = provider(store, &format!("http://{address}"))
        .generate(Arc::new(request()), route(), account, context())
        .await
        .unwrap();
    assert!(stream.next().await.unwrap().is_ok());
    drop(stream);
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while !dropped.load(Ordering::SeqCst) {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    server.abort();
}

#[tokio::test]
#[ignore = "requires loopback sockets; synthetic only"]
async fn changed_generation_is_rejected_before_dispatch() {
    let mock = Mock::success();
    let (url, server) = server(mock.clone()).await;
    let (_directory, store, old) = fixture();
    store.login_for("agy", "new", credential()).unwrap();
    let result = provider(store, &url)
        .generate(Arc::new(request()), route(), old, context())
        .await;
    assert!(matches!(result,Err(error) if error.kind == FailureKind::AccountChanged));
    assert_eq!(mock.calls.load(Ordering::SeqCst), 0);
    server.abort();
}

fn route() -> Arc<llm_proxy::ports::request_routing::RequestRoute> {
    Arc::new(config().resolve_route(MODEL, Some("agy")).unwrap())
}

#[tokio::test]
#[ignore = "requires loopback sockets; synthetic only"]
async fn every_explicit_effort_is_rejected_before_auth_or_inference_for_claude_and_gemini() {
    use llm_proxy::{
        domain::ReasoningEffort,
        ports::request_routing::{RequestRoute, RouteOptions},
    };
    let mock = Mock::success();
    let (url, task) = server(mock.clone()).await;
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path()).unwrap();
    let account = store
        .login_for(
            "agy",
            "expired-synthetic",
            Credential {
                expires_at: 0,
                ..credential()
            },
        )
        .unwrap()
        .identity();
    let provider = provider(store, &url);
    for model in [MODEL, "gemini-2.5-pro"] {
        let route = Arc::new(RequestRoute {
            provider: "agy".into(),
            model: model.into(),
            options: RouteOptions::Standard,
        });
        for effort in [
            ReasoningEffort::None,
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
            ReasoningEffort::Xhigh,
            ReasoningEffort::Max,
        ] {
            let mut request = request();
            request.effort = Some(effort);
            assert_eq!(
                provider
                    .validate_request(&request, &route)
                    .unwrap_err()
                    .code,
                "unsupported_reasoning_effort"
            );
            let result = provider
                .generate(Arc::new(request), route.clone(), account.clone(), context())
                .await;
            assert!(matches!(result, Err(error) if error.code == "unsupported_reasoning_effort"));
        }
    }
    assert_eq!(mock.calls.load(Ordering::SeqCst), 0);
    assert_eq!(mock.refreshes.load(Ordering::SeqCst), 0);
    task.abort();
}
