use llm_proxy::domain::{Content, GenerationEvent, accumulator::Accumulator};
use serde_json::json;

const LIMIT: usize = 16 * 1024 * 1024;

#[test]
fn initial_content_payloads_are_bounded_for_every_content_kind() {
    for kind in 0..15 {
        let large = "x".repeat(LIMIT + 1);
        let block = match kind {
            0 => Content::Text(large),
            1 => Content::Thinking {
                summary: large,
                signature: None,
            },
            2 => Content::Thinking {
                summary: String::new(),
                signature: Some(large),
            },
            3 => Content::ToolCall {
                id: large,
                name: "Read".into(),
                arguments: json!({}),
            },
            4 => Content::ToolCall {
                id: "c".into(),
                name: large,
                arguments: json!({}),
            },
            5 => Content::ToolCall {
                id: "c".into(),
                name: "Read".into(),
                arguments: json!({"nested":{"text":large}}),
            },
            6 => Content::HostedCall {
                id: "c".into(),
                name: "web_search".into(),
                input: json!({"query":large}),
            },
            7 => Content::HostedResult {
                id: "c".into(),
                results: json!([{"nested":{"text":large}}]),
            },
            8 => Content::ToolResult {
                id: "c".into(),
                content: vec![Content::Text(large)],
                is_error: false,
            },
            9 => Content::Image {
                media_type: "image/png".into(),
                data: large,
            },
            10 => Content::HostedCall {
                id: large,
                name: "web_search".into(),
                input: json!({}),
            },
            11 => Content::HostedCall {
                id: "c".into(),
                name: large,
                input: json!({}),
            },
            12 => Content::HostedResult {
                id: large,
                results: json!([]),
            },
            13 => Content::ToolResult {
                id: large,
                content: vec![],
                is_error: false,
            },
            _ => Content::Image {
                media_type: large,
                data: String::new(),
            },
        };
        assert!(
            Accumulator::default()
                .accept(GenerationEvent::BlockStart { index: 0, block })
                .is_err(),
            "content kind {kind} bypassed budget"
        );
    }
}

#[test]
fn initial_payloads_and_later_deltas_share_one_budget() {
    for kind in 0..3 {
        let mut a = Accumulator::default();
        let initial = "x".repeat(10 * 1024 * 1024);
        let block = match kind {
            0 => Content::Text(initial),
            1 => Content::Thinking {
                summary: initial,
                signature: None,
            },
            _ => Content::ToolCall {
                id: "c".into(),
                name: "Read".into(),
                arguments: json!({"initial": initial}),
            },
        };
        a.accept(GenerationEvent::BlockStart { index: 0, block })
            .unwrap();
        let extra = "y".repeat(7 * 1024 * 1024);
        let event = match kind {
            0 => GenerationEvent::TextDelta {
                index: 0,
                text: extra,
            },
            1 => GenerationEvent::ThinkingDelta {
                index: 0,
                text: extra,
            },
            _ => GenerationEvent::ToolArgumentsDelta {
                index: 0,
                json: extra,
            },
        };
        assert!(
            a.accept(event).is_err(),
            "delta kind {kind} bypassed initial payload budget"
        );
    }
}

#[test]
fn empty_blocks_and_invalid_delta_indexes_are_bounded() {
    let mut a = Accumulator::default();
    for index in 0..4096 {
        a.accept(GenerationEvent::BlockStart {
            index,
            block: Content::Text(String::new()),
        })
        .unwrap();
    }
    assert!(
        a.accept(GenerationEvent::BlockStart {
            index: 4096,
            block: Content::Text(String::new())
        })
        .is_err()
    );
    assert!(
        Accumulator::default()
            .accept(GenerationEvent::ToolArgumentsDelta {
                index: usize::MAX,
                json: "{}".into()
            })
            .is_err()
    );
}

#[test]
fn nested_json_nodes_are_bounded_before_retention() {
    let results = serde_json::Value::Array(vec![
        serde_json::Value::Null;
        LIMIT / std::mem::size_of::<serde_json::Value>()
            + 1
    ]);
    assert!(
        Accumulator::default()
            .accept(GenerationEvent::BlockStart {
                index: 0,
                block: Content::HostedResult {
                    id: "c".into(),
                    results
                }
            })
            .is_err()
    );
}
