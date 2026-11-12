use llm_proxy::{
    domain::accumulator::Accumulator,
    domain::{Content, GenerationEvent, StopReason},
    providers::codex::{reasoning::ReasoningSigner, stream::Translator},
};
use serde_json::json;
fn translator() -> Translator {
    Translator::new(
        "sonnet".into(),
        "gpt-5.5".into(),
        "account".into(),
        "generation".into(),
        ReasoningSigner::new([2; 32]),
        10,
    )
}
#[test]
fn interleaved_tool_chunks_preserve_ids_and_buffered_arguments() {
    let mut t = translator();
    let mut accumulator = Accumulator::default();
    let events = vec![
        json!({"type":"response.created"}),
        json!({"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","call_id":"t1","name":"Read","arguments":""}}),
        json!({"type":"response.output_item.added","output_index":1,"item":{"type":"function_call","call_id":"t2","name":"Bash","arguments":""}}),
        json!({"type":"response.function_call_arguments.delta","output_index":0,"delta":"{\"path\":"}),
        json!({"type":"response.function_call_arguments.delta","output_index":1,"delta":"{\"command\":\"pwd\"}"}),
        json!({"type":"response.function_call_arguments.delta","output_index":0,"delta":"\"file\"}"}),
        json!({"type":"response.output_item.done","output_index":1,"item":{"type":"function_call","call_id":"t2","name":"Bash","arguments":"{\"command\":\"pwd\"}"}}),
        json!({"type":"response.output_item.done","output_index":0,"item":{"type":"function_call","call_id":"t1","name":"Read","arguments":"{\"path\":\"file\"}"}}),
        json!({"type":"response.completed","response":{"usage":{"input_tokens":100,"output_tokens":20,"input_tokens_details":{"cached_tokens":40},"output_tokens_details":{"reasoning_tokens":5}}}}),
    ];
    for event in events {
        for event in t.accept(&event).unwrap() {
            accumulator.accept(event).unwrap();
        }
    }
    let output = accumulator.finish().unwrap();
    assert_eq!(output.reason, StopReason::ToolUse);
    assert!(
        matches!(&output.content[0],Content::ToolCall {id,arguments,..} if id=="t1" && arguments["path"]=="file")
    );
    assert_eq!(output.usage.cached, 40);
    assert_eq!(output.usage.output, 20);
}
#[test]
fn created_is_only_ping_and_failure_is_not_success() {
    let mut t = translator();
    assert!(
        t.accept(&json!({"type":"response.created"}))
            .unwrap()
            .iter()
            .all(|e| matches!(e, GenerationEvent::Ping))
    );
    assert!(
        t.accept(&json!({"type":"response.failed","response":{"error":{"code":"server_error"}}}))
            .is_err()
    );
    assert!(!t.finished());
}
#[test]
fn terminal_only_output_is_preserved_and_max_tokens_is_normal_completion() {
    let mut t = translator();
    let events=t.accept(&json!({"type":"response.incomplete","response":{"incomplete_details":{"reason":"max_output_tokens"},"output":[{"type":"message","content":[{"type":"output_text","text":"partial"}]}],"usage":{"output_tokens":5}}})).unwrap();
    let mut a = Accumulator::default();
    for event in events {
        a.accept(event).unwrap();
    }
    let output = a.finish().unwrap();
    assert_eq!(output.reason, StopReason::MaxTokens);
    assert!(matches!(&output.content[0],Content::Text(s) if s=="partial"));
}

#[test]
fn repeated_done_and_terminal_items_never_emit_deltas_after_block_stop() {
    let mut t = translator();
    let tool =
        json!({"type":"function_call","call_id":"c","name":"Read","arguments":"{\"path\":\"a\"}"});
    let reasoning = json!({"type":"reasoning","id":"rs","encrypted_content":"opaque"});
    let mut stopped = std::collections::HashSet::new();
    for event in [
        json!({"type":"response.output_item.done","output_index":0,"item":reasoning}),
        json!({"type":"response.output_item.done","output_index":1,"item":tool}),
        json!({"type":"response.completed","response":{"output":[reasoning,tool]}}),
    ] {
        for event in t.accept(&event).unwrap() {
            match event {
                GenerationEvent::BlockEnd { index } => {
                    stopped.insert(index);
                }
                GenerationEvent::ToolArgumentsDelta { index, .. }
                | GenerationEvent::Signature { index, .. } => {
                    assert!(!stopped.contains(&index), "delta emitted after block stop")
                }
                _ => {}
            }
        }
    }
}

#[test]
fn terminal_completion_rejects_unfinished_tool_arguments() {
    let mut t = translator();
    t.accept(&json!({"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","call_id":"c","name":"Read"}})).unwrap();
    t.accept(
        &json!({"type":"response.function_call_arguments.delta","output_index":0,"delta":"{"}),
    )
    .unwrap();
    assert!(
        t.accept(&json!({"type":"response.completed","response":{}}))
            .is_err()
    );
    assert!(!t.finished());
}
#[test]
fn deltas_after_block_stop_are_rejected_for_each_content_kind() {
    let mut text = translator();
    text.accept(&json!({"type":"response.output_text.done","text":"done"}))
        .unwrap();
    assert!(
        text.accept(&json!({"type":"response.output_text.delta","delta":"late"}))
            .is_err()
    );
    let mut tool = translator();
    tool.accept(&json!({"type":"response.output_item.done","item":{"type":"function_call","call_id":"c","name":"Read","arguments":"{}"}})).unwrap();
    assert!(
        tool.accept(&json!({"type":"response.function_call_arguments.delta","delta":"late"}))
            .is_err()
    );
    let mut thinking = translator();
    thinking.accept(&json!({"type":"response.output_item.done","item":{"type":"reasoning","id":"r","encrypted_content":"cipher"}})).unwrap();
    assert!(
        thinking
            .accept(&json!({"type":"response.reasoning_summary_text.delta","delta":"late"}))
            .is_err()
    );
}
#[test]
fn retained_reasoning_metadata_has_a_global_budget() {
    let mut t = translator();
    let encrypted = "x".repeat(1024 * 1024);
    let mut rejected = false;
    for index in 0..20 {
        if t.accept(&json!({"type":"response.output_item.added","output_index":index,"item":{"type":"reasoning","id":format!("r{index}"),"encrypted_content":encrypted}})).is_err() {rejected=true;break;}
    }
    assert!(rejected, "reasoning metadata bypassed output budget");
}
#[test]
fn retained_native_items_have_a_count_limit_even_without_visible_content() {
    let mut t = translator();
    let mut rejected = false;
    for index in 0..4200 {
        if t.accept(&json!({"type":"response.output_item.added","output_index":index,"item":{"type":"reasoning","id":"r"}})).is_err(){rejected=true;break;}
    }
    assert!(rejected, "unbounded native item table");
}

#[test]
fn terminal_reasoning_summaries_and_native_refusals_are_not_lost() {
    let mut t = translator();
    let mut a = Accumulator::default();
    let event = json!({"type":"response.completed","response":{"output":[{"type":"reasoning","summary":[{"type":"summary_text","text":"portable summary"}]},{"type":"message","content":[{"type":"refusal","refusal":"Cannot do that"}]}]}});
    for event in t.accept(&event).unwrap() {
        a.accept(event).unwrap();
    }
    let output = a.finish().unwrap();
    assert!(
        matches!(&output.content[0],Content::Thinking{summary,..} if summary=="portable summary")
    );
    assert!(matches!(&output.content[1],Content::Text(text) if text=="Cannot do that"));
}
#[test]
fn completed_summary_reconciles_with_deltas_without_duplication() {
    let mut t = translator();
    let mut a = Accumulator::default();
    for event in [
        json!({"type":"response.reasoning_summary_text.delta","delta":"first","summary_index":0}),
        json!({"type":"response.reasoning_summary_text.done","text":"first summary","summary_index":0}),
        json!({"type":"response.completed","response":{"output":[{"type":"reasoning","summary":[{"text":"first summary"},{"text":"second summary"}]}]}}),
    ] {
        for event in t.accept(&event).unwrap() {
            a.accept(event).unwrap();
        }
    }
    assert!(
        matches!(&a.finish().unwrap().content[0],Content::Thinking{summary,..} if summary=="first summary\n\nsecond summary")
    );
}
#[test]
fn streamed_refusal_is_preserved_once() {
    let mut t = translator();
    let mut a = Accumulator::default();
    for event in [
        json!({"type":"response.refusal.delta","delta":"Cannot "}),
        json!({"type":"response.refusal.done","refusal":"Cannot help"}),
        json!({"type":"response.completed","response":{"output":[{"type":"message","content":[{"type":"refusal","refusal":"Cannot help"}]}]}}),
    ] {
        for event in t.accept(&event).unwrap() {
            a.accept(event).unwrap();
        }
    }
    assert!(matches!(&a.finish().unwrap().content[0],Content::Text(text) if text=="Cannot help"));
}
#[test]
fn citation_storage_and_repeated_hosted_result_copies_are_bounded() {
    let mut t = translator();
    let long_url = format!("https://example.com/{}", "x".repeat(256 * 1024));
    for index in 0..64 {
        t.accept(&json!({"type":"response.output_item.done","output_index":index,"item":{"type":"web_search_call","id":format!("s{index}"),"action":{"query":"q"}}})).unwrap();
    }
    t.accept(&json!({"type":"response.content_part.done","output_index":64,"part":{"type":"output_text","text":"answer","annotations":[{"url":long_url,"title":"long result"}]}})).unwrap();
    assert!(
        t.accept(&json!({"type":"response.completed","response":{}}))
            .is_err()
    );
}
