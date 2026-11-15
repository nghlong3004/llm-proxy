use llm_proxy::{
    adapters::inbound::http::anthropic::Anthropic,
    adapters::outbound::codex::{reasoning::ReasoningSigner, request::translate},
    config::Config,
    domain::{Content, RequestPurpose},
    ports::protocol_adapter::ProtocolAdapter,
};
use serde_json::json;

const ACTION_REVIEW_PREFIX: &str = "You are a security monitor for autonomous AI coding agents.";

#[test]
fn action_review_classifies_exact_system_prefix_in_string_or_any_text_block() {
    for system in [
        json!(ACTION_REVIEW_PREFIX),
        json!(format!(
            "{ACTION_REVIEW_PREFIX}\nReview the proposed action."
        )),
        json!([{"type":"text","text":ACTION_REVIEW_PREFIX}]),
        json!([
            {"type":"text","text":"Client preamble"},
            {"type":"text","text":format!("{ACTION_REVIEW_PREFIX}\nReview.")}
        ]),
    ] {
        for stream in [None, Some(json!(false))] {
            for tools in [None, Some(json!([]))] {
                let mut value = json!({"model":"sonnet","system":system,"messages":[{"role":"user","content":"Action"}]});
                if let Some(stream) = stream.as_ref() {
                    value["stream"] = stream.clone();
                }
                if let Some(tools) = tools {
                    value["tools"] = tools;
                }
                assert_eq!(
                    Anthropic.request_purpose(&value),
                    RequestPurpose::ActionReview,
                    "system prefix should classify: {value}"
                );
                assert_eq!(
                    Anthropic.decode(value).unwrap().purpose,
                    RequestPurpose::Conversation,
                    "decoding alone must not classify, including for token counting"
                );
            }
        }
    }
}

#[test]
fn action_review_does_not_scan_substrings_history_or_non_text_system_blocks() {
    for value in [
        json!({}),
        json!({"system":null}),
        json!({"system":42}),
        json!({"system":format!(" {ACTION_REVIEW_PREFIX}")}),
        json!({"system":format!("Quoted: {ACTION_REVIEW_PREFIX}")}),
        json!({"system":"You are a security monitor for autonomous AI coding agents"}),
        json!({"system":ACTION_REVIEW_PREFIX.to_lowercase()}),
        json!({"system":[{"type":"image","text":ACTION_REVIEW_PREFIX}]}),
        json!({"system":[{"text":ACTION_REVIEW_PREFIX}]}),
        json!({"system":[{"type":"text","text":42}]}),
        json!({"system":[{"type":"text","text":format!("Before {ACTION_REVIEW_PREFIX}")}]}),
        json!({"system":[{"type":"text","content":[{"type":"text","text":ACTION_REVIEW_PREFIX}]}]}),
        json!({"messages":[{"role":"user","content":ACTION_REVIEW_PREFIX}]}),
        json!({"messages":[{"role":"system","content":ACTION_REVIEW_PREFIX}]}),
    ] {
        assert_eq!(
            Anthropic.request_purpose(&value),
            RequestPurpose::Conversation
        );
    }
}

#[test]
fn action_review_rejects_streaming_and_tools_including_malformed_values() {
    for (field, invalid_values) in [
        (
            "stream",
            vec![
                json!(true),
                json!(null),
                json!("false"),
                json!(0),
                json!([]),
                json!({}),
            ],
        ),
        (
            "tools",
            vec![
                json!([{"name":"Read","input_schema":{"type":"object"}}]),
                json!(null),
                json!({}),
                json!(""),
                json!(false),
            ],
        ),
    ] {
        for invalid in invalid_values {
            let mut value = json!({"model":"sonnet","system":ACTION_REVIEW_PREFIX,"messages":[{"role":"user","content":"Action"}]});
            value[field] = invalid;
            assert_eq!(
                Anthropic.request_purpose(&value),
                RequestPurpose::Conversation
            );
            if value[field] != json!(true) && !value[field].is_array() {
                assert!(
                    Anthropic.decode(value).is_err(),
                    "malformed {field} must still fail decoding"
                );
            }
        }
    }
}

#[test]
fn conversation_replays_signed_reasoning_but_action_review_keeps_only_summary() {
    let signer = ReasoningSigner::new([4; 32]);
    let config = Config::default();
    for model_id in ["gpt-5.5", "gpt-6.1-sol"] {
        let model = config.resolve(model_id).unwrap();
        let signature = signer
            .sign("a", "gen1", model_id, "rs_1", "encrypted")
            .unwrap();
        let value = json!({
            "model":model_id,"system":ACTION_REVIEW_PREFIX,"output_config":{"effort":"high","format":{"type":"json_schema","schema":{"type":"object","properties":{"allow":{"type":"boolean"}},"required":["allow"],"additionalProperties":false}}},
            "messages":[
                {"role":"assistant","content":[{"type":"thinking","thinking":"Portable reasoning summary","signature":signature},{"type":"text","text":"Earlier reply"}]},
                {"role":"user","content":"Review this action"}
            ]
        });
        let mut request = Anthropic.decode(value.clone()).unwrap();
        assert_eq!(request.purpose, RequestPurpose::Conversation);
        let conversation = translate(&request, &model, "a", "gen1", &signer, None).unwrap();
        assert!(conversation["input"].as_array().unwrap().iter().any(|item| item["type"] == "reasoning" && item["encrypted_content"] == "encrypted"));
        request.purpose = Anthropic.request_purpose(&value);
        assert_eq!(request.purpose, RequestPurpose::ActionReview);
        let review = translate(&request, &model, "a", "gen1", &signer, None).unwrap();
        assert!(
            review["input"]
                .as_array()
                .unwrap()
                .iter()
                .all(|item| item["type"] != "reasoning")
        );
        let offset = usize::from(model.responses_lite);
        assert_eq!(
            review["input"][offset]["content"],
            json!([
                {"type":"output_text","text":"Prior reasoning summary:\nPortable reasoning summary"},
                {"type":"output_text","text":"Earlier reply"}
            ])
        );
        assert_eq!(
            review["input"][offset + 1],
            json!({"type":"message","role":"user","content":[{"type":"input_text","text":"Review this action"}]})
        );
        assert_eq!(review["reasoning"], conversation["reasoning"]);
        assert_eq!(review["text"], conversation["text"]);
    }
}

#[test]
fn anthropic_tools_and_images_become_native_codex_input() {
    let request = Anthropic.decode(json!({"model":"sonnet", "system":"Be careful", "tools":[{"name":"Read","input_schema":{"type":"object","properties":{}}}], "messages":[{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Read","input":{"path":"a.png"}}]},{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":[{"type":"text","text":"image"},{"type":"image","source":{"type":"base64","media_type":"image/png","data":"YQ=="}}]}]}]})).unwrap();
    assert!(matches!(
        request.messages[0].content[0],
        Content::ToolCall { .. }
    ));
    let config = Config {
        default_model: Some("gpt-5.5".into()),
        ..Default::default()
    };
    let model = config.resolve("sonnet").unwrap();
    let value = translate(
        &request,
        &model,
        "a",
        "generation",
        &ReasoningSigner::new([1; 32]),
        None,
    )
    .unwrap();
    assert_eq!(value["input"][0]["type"], "function_call");
    assert_eq!(value["input"][1]["output"][1]["type"], "input_image");
    assert_eq!(value["tools"][0]["parameters"]["type"], "object");
    assert_eq!(value["store"], false);
}
#[test]
fn forced_search_without_dedicated_marker_preserves_full_history_in_both_lanes() {
    for model_id in ["gpt-5.5", "gpt-6.1-sol"] {
        let request = Anthropic.decode(json!({"model":"sonnet","tools":[{"type":"web_search_20250305","name":"web_search"}],"tool_choice":{"type":"tool","name":"web_search"},"messages":[{"role":"user","content":"Earlier context about Rust"},{"role":"assistant","content":"Which release?"},{"role":"user","content":"Find its latest release"}]})).unwrap();
        assert!(llm_proxy::adapters::outbound::codex::search::query(&request).is_none());
        let config = Config {
            default_model: Some(model_id.into()),
            ..Default::default()
        };
        let native = translate(
            &request,
            &config.resolve("sonnet").unwrap(),
            "a",
            "g",
            &ReasoningSigner::new([0; 32]),
            None,
        )
        .unwrap();
        let serialized = native.to_string();
        assert!(serialized.contains("Earlier context about Rust"));
        assert!(serialized.contains("Find its latest release"));
        assert!(serialized.contains("web_search"));
    }
}

#[test]
fn dedicated_search_marker_keeps_the_direct_search_bridge() {
    let mut request = Anthropic.decode(json!({"model":"sonnet","tools":[{"type":"web_search_20250305","name":"web_search"}],"tool_choice":{"type":"tool","name":"web_search"},"messages":[{"role":"user","content":"Perform a web search for the query: Rust releases"}]})).unwrap();
    assert_eq!(
        llm_proxy::adapters::outbound::codex::search::query(&request).as_deref(),
        Some("Rust releases")
    );
    request.messages.push(llm_proxy::domain::Message {
        role: llm_proxy::domain::Role::User,
        content: vec![Content::Text("Follow up using earlier context".into())],
    });
    assert!(llm_proxy::adapters::outbound::codex::search::query(&request).is_none());
}

#[test]
fn direct_search_checks_query_and_structured_results_before_copying() {
    let request = Anthropic
        .decode(json!({"model":"sonnet","messages":[{"role":"user","content":"search"}]}))
        .unwrap();
    assert!(
        llm_proxy::adapters::outbound::codex::search::events(
            json!({"output":"answer"}),
            &request,
            &"x".repeat(16 * 1024 * 1024 + 1),
            1
        )
        .is_err()
    );
    let response = json!({"output":"answer","results":[{"url":"https://example.com","title":"x".repeat(16 * 1024 * 1024 + 1)}]});
    assert!(
        llm_proxy::adapters::outbound::codex::search::events(response, &request, "q", 1).is_err()
    );
}

#[test]
fn direct_search_preserves_normal_hosted_results_and_text() {
    let request = Anthropic
        .decode(json!({"model":"sonnet","messages":[{"role":"user","content":"search"}]}))
        .unwrap();
    let events = llm_proxy::adapters::outbound::codex::search::events(json!({"output":"answer [source](https://example.com)","results":[{"url":"https://example.com","title":"Source"}]}), &request, "q", 1).unwrap();
    let mut accumulator = llm_proxy::domain::accumulator::Accumulator::default();
    for event in events {
        accumulator.accept(event).unwrap();
    }
    let output = accumulator.finish().unwrap();
    assert!(
        matches!(&output.content[0], Content::HostedCall { input, .. } if input["query"] == "q")
    );
    assert!(
        matches!(&output.content[1], Content::HostedResult { results, .. } if results.as_array().unwrap().len() == 1 && results[0]["title"] == "Source")
    );
    assert!(
        matches!(&output.content[2], Content::Text(text) if text == "answer [source](https://example.com)")
    );
}

#[test]
fn reasoning_replays_only_for_same_account_generation_and_model() {
    let signer = ReasoningSigner::new([4; 32]);
    let signature = signer
        .sign("a", "gen1", "gpt-5.5", "rs_1", "encrypted")
        .unwrap();
    assert!(signer.verify(&signature, "a", "gen1", "gpt-5.5").is_some());
    assert!(signer.verify(&signature, "b", "gen1", "gpt-5.5").is_none());
    assert!(signer.verify(&signature, "a", "gen2", "gpt-5.5").is_none());
    assert!(
        signer
            .verify(&signature, "a", "gen1", "gpt-6-sol")
            .is_none()
    );
    let mut forged = signature.into_bytes();
    let last = forged.len() - 2;
    forged[last] ^= 1;
    assert!(
        signer
            .verify(&String::from_utf8(forged).unwrap(), "a", "gen1", "gpt-5.5")
            .is_none()
    );
}
#[test]
fn lite_lane_moves_tools_and_instructions_without_parallel_calls() {
    let request = Anthropic.decode(json!({"model":"sonnet","system":"instructions","messages":[{"role":"user","content":"hello"}],"tools":[{"name":"Bash","input_schema":{"type":"object"}}]})).unwrap();
    let config = Config {
        default_model: Some("gpt-6.1-sol".into()),
        ..Default::default()
    };
    let value = translate(
        &request,
        &config.resolve("sonnet").unwrap(),
        "a",
        "g",
        &ReasoningSigner::new([0; 32]),
        None,
    )
    .unwrap();
    assert_eq!(value["parallel_tool_calls"], false);
    assert_eq!(value["input"][0]["type"], "additional_tools");
    assert_eq!(value["input"][1]["role"], "developer");
}

#[test]
fn claude_code_system_messages_keep_their_position_as_codex_developer_instructions() {
    for (model_id, lite) in [("gpt-5.5", false), ("gpt-6.1-sol", true)] {
        let request = Anthropic.decode(json!({
            "model": "claude-sonnet-5-5", "system": "global instructions",
            "messages": [
                {"role":"user","content":"first user turn"},
                {"role":"system","content":[{"type":"text","text":"in-turn instruction","cache_control":{"type":"ephemeral"}}]},
                {"role":"assistant","content":"assistant reply"},
                {"role":"system","content":"later instruction"},
                {"role":"user","content":"next user turn"}
            ]
        })).unwrap();
        let config = Config {
            default_model: Some(model_id.into()),
            ..Default::default()
        };
        let value = translate(
            &request,
            &config.resolve(&request.model).unwrap(),
            "a",
            "g",
            &ReasoningSigner::new([0; 32]),
            None,
        )
        .unwrap();
        let offset = usize::from(lite);
        let input = value["input"].as_array().unwrap();
        let roles: Vec<_> = input[offset..]
            .iter()
            .map(|item| item["role"].as_str().unwrap())
            .collect();
        assert_eq!(
            roles,
            ["user", "developer", "assistant", "developer", "user"]
        );
        assert_eq!(
            input[offset + 1]["content"][0],
            json!({"type":"input_text","text":"in-turn instruction"})
        );
        assert_eq!(input[offset + 3]["content"][0]["text"], "later instruction");
    }
}

#[test]
fn system_messages_reject_tool_calls_and_unknown_roles_remain_invalid() {
    let error = Anthropic.decode(json!({"model":"sonnet","messages":[{"role":"system","content":[{"type":"tool_use","id":"t","name":"Bash","input":{}}]}]})).unwrap_err();
    assert_eq!(
        error.message,
        "System message content must contain only text"
    );
    assert!(
        Anthropic
            .decode(json!({"model":"sonnet","messages":[{"role":"unknown","content":"hello"}]}))
            .is_err()
    );
}

#[test]
fn ordinary_claude_context_instructions_do_not_downgrade_requested_reasoning() {
    let config = Config {
        default_model: Some("gpt-6.1-sol".into()),
        ..Default::default()
    };
    for effort in ["medium", "high"] {
        let request = Anthropic.decode(json!({
            "model":"sonnet",
            "system":"Manage the context window. Automatic compaction may summarize earlier conversation.",
            "output_config":{"effort":effort},
            "messages":[{"role":"user","content":"Implement the feature."}]
        })).unwrap();
        assert!(!request.compact);
        let native = translate(
            &request,
            &config.resolve("sonnet").unwrap(),
            "a",
            "g",
            &ReasoningSigner::new([0; 32]),
            None,
        )
        .unwrap();
        assert_eq!(native["reasoning"]["effort"], effort);
    }
}

#[test]
fn actual_compaction_markers_keep_the_low_reasoning_fast_path() {
    let config = Config {
        default_model: Some("gpt-6.1-sol".into()),
        ..Default::default()
    };
    for (system, user, metadata) in [
        (
            "global instructions",
            "Summarize.",
            json!({"is_compaction":true}),
        ),
        (
            "You are a helpful AI assistant tasked with summarizing conversations.",
            "History.",
            json!({}),
        ),
        (
            "global instructions",
            "CRITICAL: Respond with TEXT ONLY. Do NOT call any tools. Your task is to create a detailed summary of the conversation so far",
            json!({}),
        ),
    ] {
        let request = Anthropic
            .decode(json!({"model":"sonnet","system":system,"metadata":metadata,
            "output_config":{"effort":"high"},"messages":[{"role":"user","content":user}]}))
            .unwrap();
        assert!(request.compact);
        let native = translate(
            &request,
            &config.resolve("sonnet").unwrap(),
            "a",
            "g",
            &ReasoningSigner::new([0; 32]),
            None,
        )
        .unwrap();
        assert_eq!(native["reasoning"]["effort"], "low");
    }
}
