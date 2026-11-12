use llm_proxy::providers::codex::{quota::classify, sse::Decoder};
use serde_json::json;

#[test]
fn sse_handles_every_byte_fragment_including_utf8_crlf_and_multiline_data() {
    let bytes = "event: delta\r\ndata: {\"type\":\"text\",\r\ndata: \"text\":\"Tiếng Việt 🦀\"}\r\n\r\n:data comment\n\ndata: [DONE]\n\n".as_bytes();
    let mut decoder = Decoder::default();
    let mut events = vec![];
    for byte in bytes {
        events.extend(decoder.push(&[*byte]).unwrap());
    }
    decoder.finish().unwrap();
    assert_eq!(events, vec![json!({"type":"text","text":"Tiếng Việt 🦀"})]);
}
#[test]
fn partial_stream_frame_is_an_error_not_success() {
    let mut decoder = Decoder::default();
    decoder
        .push(b"data: {\"type\":\"response.completed\"}")
        .unwrap();
    assert!(decoder.finish().is_err());
}
#[test]
fn generic_429_is_not_hard_quota_and_both_windows_must_reset() {
    let headers = axum::http::HeaderMap::new();
    let generic = classify(
        429,
        &json!({"error":{"type":"rate_limit_exceeded"}}),
        &headers,
        100,
    )
    .unwrap();
    assert!(!generic.is_quota());
    let quota = classify(429, &json!({"error":{"type":"usage_limit_reached"},"rate_limits":{"primary":{"used_percent":100,"reset_at":200},"secondary":{"used_percent":100,"reset_at":300}}}), &headers, 100).unwrap();
    assert_eq!(quota.quota_reset(), Some(300));
}
#[test]
fn sse_200_quota_and_credits_are_classified_without_parsing_prose() {
    let h = axum::http::HeaderMap::new();
    let error = classify(200, &json!({"type":"response.failed","response":{"error":{"code":"usage_limit_reached","resets_in_seconds":40}}}), &h, 100).unwrap();
    assert_eq!(error.quota_reset(), Some(140));
    assert!(classify(200, &json!({"type":"codex.rate_limits","rate_limits":{"limit_reached":true},"credits":{"has_credits":true}}), &h, 100).is_none());
}

#[test]
fn upstream_validation_failures_have_safe_diagnostics_instead_of_a_generic_400() {
    let headers = axum::http::HeaderMap::new();
    let error = classify(422, &json!({"error":{"type":"invalid_request_error","code":"unsupported_parameter","param":"reasoning.context","message":"private upstream echo SECRET_PROMPT_TOKEN"}}), &headers, 100).unwrap();
    assert_eq!(error.code, "upstream_unsupported_parameter");
    assert_eq!(error.upstream_status, Some(422));
    assert_eq!(error.upstream_parameter, Some("reasoning.context"));
    assert!(!format!("{error:?}").contains("SECRET_PROMPT_TOKEN"));
    let detail = classify(
        400,
        &json!({"detail":"reasoning.context must be all_turns"}),
        &headers,
        100,
    )
    .unwrap();
    assert_eq!(detail.code, "upstream_invalid_reasoning_context");
    let schema = classify(400, &json!({"error":{"code":"invalid_function_parameters","param":"tools[0].parameters","message":"Invalid schema for tool SECRET_TOOL_ARGS"}}), &headers, 100).unwrap();
    assert_eq!(schema.code, "upstream_invalid_tool_schema");
    assert!(!format!("{schema:?}").contains("SECRET_TOOL_ARGS"));
    let model = classify(
        404,
        &json!({"error":{"code":"model_not_found","param":"model"}}),
        &headers,
        100,
    )
    .unwrap();
    assert_eq!(model.code, "upstream_model_not_available");
    assert!(!model.is_quota());
}

#[test]
fn unrecognized_upstream_diagnostics_never_retain_echoed_values() {
    let error = classify(400, &json!({"error":{"code":"SECRET_TOKEN_CODE","param":"SECRET_PROMPT_PARAMETER","message":"SECRET_REASONING_BODY"}}), &axum::http::HeaderMap::new(), 100).unwrap();
    assert_eq!(error.code, "upstream_invalid_request");
    assert!(error.upstream_parameter.is_none());
    assert!(!format!("{error:?}").contains("SECRET_"));
}
