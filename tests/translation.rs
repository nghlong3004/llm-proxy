use llm_proxy::{
    config::Config,
    domain::Content,
    protocols::{ProtocolAdapter, anthropic::Anthropic},
    providers::codex::{reasoning::ReasoningSigner, request::translate},
};
use serde_json::json;

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
