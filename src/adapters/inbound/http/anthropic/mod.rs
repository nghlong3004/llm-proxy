//! Anthropic Messages adapter. Client-specific field names stop at this boundary.
pub mod sse;
use crate::ports::protocol_adapter::ProtocolAdapter;
use crate::{
    domain::error::{FailureKind, ProxyError},
    domain::*,
};
use serde_json::{Value, json};
pub struct Anthropic;
impl ProtocolAdapter for Anthropic {
    fn request_purpose(&self, value: &Value) -> RequestPurpose {
        const PREFIX: &str = "You are a security monitor for autonomous AI coding agents.";
        if !matches!(value.get("stream"), None | Some(Value::Bool(false))) {
            return RequestPurpose::Conversation;
        }
        match value.get("tools") {
            None => {}
            Some(Value::Array(tools)) if tools.is_empty() => {}
            _ => return RequestPurpose::Conversation,
        }
        let is_review = match value.get("system") {
            Some(Value::String(text)) => text.starts_with(PREFIX),
            Some(Value::Array(blocks)) => blocks.iter().any(|block| {
                block.get("type").and_then(Value::as_str) == Some("text")
                    && block
                        .get("text")
                        .and_then(Value::as_str)
                        .is_some_and(|text| text.starts_with(PREFIX))
            }),
            _ => false,
        };
        if is_review {
            RequestPurpose::ActionReview
        } else {
            RequestPurpose::Conversation
        }
    }
    fn decode(&self, value: Value) -> Result<GenerationRequest, ProxyError> {
        let object = value
            .as_object()
            .ok_or_else(|| ProxyError::invalid("Request body must be a JSON object"))?;
        let model = required_str(&value, "model")?.to_owned();
        if model.len() > 256 {
            return Err(ProxyError::invalid("Model identifier is too long"));
        }
        let raw_messages = object
            .get("messages")
            .and_then(Value::as_array)
            .ok_or_else(|| ProxyError::invalid("messages must be an array"))?;
        if raw_messages.is_empty() {
            return Err(ProxyError::invalid("messages cannot be empty"));
        }
        let mut messages = vec![];
        for message in raw_messages {
            let role = match required_str(message, "role")? {
                "system" => Role::System,
                "user" => Role::User,
                "assistant" => Role::Assistant,
                _ => {
                    return Err(ProxyError::invalid(
                        "Message role must be system, user or assistant",
                    )
                    .with_code("invalid_message_role"));
                }
            };
            let content = parse_content(
                message
                    .get("content")
                    .ok_or_else(|| ProxyError::invalid("Message content is required"))?,
                0,
            )?;
            if role == Role::System && content.iter().any(|part| !matches!(part, Content::Text(_)))
            {
                return Err(
                    ProxyError::invalid("System message content must contain only text")
                        .with_code("unsupported_system_content"),
                );
            }
            messages.push(Message { role, content });
        }
        let system = match value.get("system") {
            None => String::new(),
            Some(Value::String(text)) => text.clone(),
            Some(Value::Array(blocks)) => blocks
                .iter()
                .map(|block| required_str(block, "text").map(str::to_owned))
                .collect::<Result<Vec<_>, _>>()?
                .join("\n\n"),
            _ => {
                return Err(ProxyError::invalid(
                    "system must be text or an array of text blocks",
                ));
            }
        };
        let mut tools = vec![];
        if let Some(raw_tools) = value.get("tools") {
            for tool in raw_tools
                .as_array()
                .ok_or_else(|| ProxyError::invalid("tools must be an array"))?
            {
                let name = required_str(tool, "name")?.to_owned();
                let kind = tool.get("type").and_then(Value::as_str).unwrap_or("custom");
                if kind.starts_with("web_search_") {
                    tools.push(Tool::WebSearch {
                        name,
                        allowed_domains: strings(tool.get("allowed_domains"))?,
                        blocked_domains: strings(tool.get("blocked_domains"))?,
                    });
                } else if matches!(kind, "custom" | "function")
                    || kind.starts_with("bash_")
                    || kind.starts_with("text_editor_")
                {
                    let parameters = tool
                        .get("input_schema")
                        .cloned()
                        .unwrap_or_else(|| json!({"type":"object","properties":{}}));
                    if !parameters.is_object() {
                        return Err(ProxyError::invalid("Tool input_schema must be an object"));
                    }
                    tools.push(Tool::Function {
                        name,
                        description: tool
                            .get("description")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .into(),
                        parameters,
                    });
                } else {
                    return Err(ProxyError::invalid(format!(
                        "Unsupported hosted tool type '{kind}'"
                    ))
                    .with_code("unsupported_hosted_tool"));
                }
            }
        }
        let choice = value.get("tool_choice");
        let tool_choice = match choice
            .and_then(|c| c.get("type"))
            .and_then(Value::as_str)
            .unwrap_or("auto")
        {
            "auto" => ToolChoice::Auto,
            "none" => ToolChoice::None,
            "any" => ToolChoice::Required,
            "tool" => {
                ToolChoice::Named(required_str(choice.unwrap_or(&Value::Null), "name")?.into())
            }
            _ => return Err(ProxyError::invalid("Unsupported tool_choice")),
        };
        if let ToolChoice::Named(name) = &tool_choice
            && !tools.iter().any(|tool| match tool {
                Tool::Function { name: n, .. } | Tool::WebSearch { name: n, .. } => n == name,
            })
        {
            return Err(ProxyError::invalid(
                "tool_choice names an unregistered tool",
            ));
        }
        let parallel_tools = !choice
            .and_then(|c| c.get("disable_parallel_tool_use"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let effort = value
            .pointer("/output_config/effort")
            .or_else(|| value.get("reasoning_effort"))
            .map(|effort| {
                effort
                    .as_str()
                    .ok_or_else(|| {
                        ProxyError::invalid("Reasoning effort must be a string")
                            .with_code("invalid_reasoning_effort")
                    })?
                    .parse::<ReasoningEffort>()
            })
            .transpose()?;
        let output_format = value
            .pointer("/output_config/format")
            .or_else(|| value.get("output_format"))
            .cloned();
        let compact = value
            .get("metadata")
            .and_then(|m| m.get("is_compaction"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        // Normal Claude Code instructions also discuss summaries and the context window.
        let compact = compact || system.contains("You are a helpful AI assistant tasked with summarizing conversations")
            || messages.last().is_some_and(|message| message.role == Role::User && message.content.iter().any(|part| matches!(part, Content::Text(text) if text.contains("CRITICAL: Respond with TEXT ONLY. Do NOT call any tools.") && text.contains("Your task is to create a detailed summary of the conversation so far"))));
        if let Some(stream) = value.get("stream")
            && !stream.is_boolean()
        {
            return Err(ProxyError::invalid("stream must be a boolean"));
        }
        Ok(GenerationRequest {
            purpose: RequestPurpose::Conversation,
            model,
            system,
            messages,
            tools,
            tool_choice,
            parallel_tools,
            effort,
            output_format,
            stream: value
                .get("stream")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            compact,
        })
    }
    fn encode_event(&self, event: GenerationEvent) -> Vec<(String, Value)> {
        let (name, value) = match event {
            GenerationEvent::Start {
                id,
                model,
                input_tokens,
            } => (
                "message_start",
                json!({"type":"message_start","message":{"id":id,"type":"message","role":"assistant","model":model,"content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":input_tokens,"output_tokens":0}}}),
            ),
            GenerationEvent::BlockStart { index, block } => (
                "content_block_start",
                json!({"type":"content_block_start","index":index,"content_block":content_json(&block)}),
            ),
            GenerationEvent::TextDelta { index, text } => (
                "content_block_delta",
                json!({"type":"content_block_delta","index":index,"delta":{"type":"text_delta","text":text}}),
            ),
            GenerationEvent::ThinkingDelta { index, text } => (
                "content_block_delta",
                json!({"type":"content_block_delta","index":index,"delta":{"type":"thinking_delta","thinking":text}}),
            ),
            GenerationEvent::Signature { index, signature } => (
                "content_block_delta",
                json!({"type":"content_block_delta","index":index,"delta":{"type":"signature_delta","signature":signature}}),
            ),
            GenerationEvent::ToolArgumentsDelta {
                index,
                json: partial,
            } => (
                "content_block_delta",
                json!({"type":"content_block_delta","index":index,"delta":{"type":"input_json_delta","partial_json":partial}}),
            ),
            GenerationEvent::BlockEnd { index } => (
                "content_block_stop",
                json!({"type":"content_block_stop","index":index}),
            ),
            GenerationEvent::Finish { usage, reason } => {
                return vec![
                    (
                        "message_delta".into(),
                        json!({"type":"message_delta","delta":{"stop_reason":stop_reason(reason),"stop_sequence":null},"usage":usage_json(&usage)}),
                    ),
                    ("message_stop".into(), json!({"type":"message_stop"})),
                ];
            }
            GenerationEvent::Ping => ("ping", json!({"type":"ping"})),
            GenerationEvent::QuotaSnapshot(_) => return vec![],
        };
        vec![(name.into(), value)]
    }
    fn encode_response(&self, response: GenerationOutput) -> Value {
        json!({"id":response.id,"type":"message","role":"assistant","model":response.model,"content":response.content.iter().map(content_json).collect::<Vec<_>>(),"stop_reason":stop_reason(response.reason),"stop_sequence":null,"usage":usage_json(&response.usage)})
    }
    fn encode_error(&self, error: &ProxyError) -> Value {
        let kind = match error.kind {
            FailureKind::Quota { .. } | FailureKind::RateLimit => "rate_limit_error",
            FailureKind::Authentication => "authentication_error",
            FailureKind::Permission => "permission_error",
            FailureKind::InvalidRequest => "invalid_request_error",
            FailureKind::NoAccounts => "overloaded_error",
            _ => "api_error",
        };
        json!({"type":"error","error":{"type":kind,"message":error.message}})
    }
    fn encode_stream_error(&self, error: &ProxyError) -> Vec<(String, Value)> {
        vec![("error".into(), self.encode_error(error))]
    }
}
pub fn usage_json(usage: &Usage) -> Value {
    json!({"input_tokens":usage.input.saturating_sub(usage.cached),"output_tokens":usage.output,"cache_creation_input_tokens":0,"cache_read_input_tokens":usage.cached,"output_tokens_details":{"reasoning_tokens":usage.reasoning,"thinking_tokens":usage.reasoning}})
}
fn stop_reason(reason: StopReason) -> &'static str {
    match reason {
        StopReason::EndTurn => "end_turn",
        StopReason::ToolUse => "tool_use",
        StopReason::MaxTokens => "max_tokens",
    }
}
pub fn content_json(content: &Content) -> Value {
    match content {
        Content::Text(text) => json!({"type":"text","text":text}),
        Content::Image { media_type, data } => {
            json!({"type":"image","source":{"type":"base64","media_type":media_type,"data":data}})
        }
        Content::ToolCall {
            id,
            name,
            arguments,
        } => json!({"type":"tool_use","id":id,"name":name,"input":arguments}),
        Content::ToolResult {
            id,
            content,
            is_error,
        } => {
            json!({"type":"tool_result","tool_use_id":id,"content":content.iter().map(content_json).collect::<Vec<_>>(),"is_error":is_error})
        }
        Content::Thinking { summary, signature } => {
            json!({"type":"thinking","thinking":summary,"signature":signature.as_deref().unwrap_or("")})
        }
        Content::HostedCall { id, name, input } => {
            json!({"type":"server_tool_use","id":id,"name":name,"input":input})
        }
        Content::HostedResult { id, results } => {
            json!({"type":"web_search_tool_result","tool_use_id":id,"content":results})
        }
    }
}
fn required_str<'a>(value: &'a Value, field: &str) -> Result<&'a str, ProxyError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ProxyError::invalid(format!("'{field}' must be a nonempty string")))
}
fn strings(value: Option<&Value>) -> Result<Vec<String>, ProxyError> {
    match value {
        None => Ok(vec![]),
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| ProxyError::invalid("Expected an array of strings"))
            })
            .collect(),
        _ => Err(ProxyError::invalid("Expected an array of strings")),
    }
}
fn parse_content(value: &Value, depth: usize) -> Result<Vec<Content>, ProxyError> {
    if depth > 4 {
        return Err(ProxyError::invalid("Content nesting is too deep"));
    }
    if let Some(text) = value.as_str() {
        return Ok(vec![Content::Text(text.into())]);
    }
    let array = value
        .as_array()
        .ok_or_else(|| ProxyError::invalid("Content must be text or an array"))?;
    array
        .iter()
        .map(|block| match required_str(block, "type")? {
            "text" => Ok(Content::Text(
                block
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| ProxyError::invalid("Missing text"))?
                    .into(),
            )),
            "image" => {
                let source = block
                    .get("source")
                    .ok_or_else(|| ProxyError::invalid("Missing image source"))?;
                if source.get("type").and_then(Value::as_str) != Some("base64") {
                    return Err(ProxyError::invalid(
                        "Only base64 image sources are supported",
                    ));
                }
                let media_type = required_str(source, "media_type")?.to_owned();
                if !["image/png", "image/jpeg", "image/webp", "image/gif"]
                    .contains(&media_type.as_str())
                {
                    return Err(ProxyError::invalid("Unsupported image media_type"));
                }
                Ok(Content::Image {
                    media_type,
                    data: required_str(source, "data")?.into(),
                })
            }
            "tool_use" => Ok(Content::ToolCall {
                id: required_str(block, "id")?.into(),
                name: required_str(block, "name")?.into(),
                arguments: block
                    .get("input")
                    .filter(|v| v.is_object())
                    .cloned()
                    .ok_or_else(|| ProxyError::invalid("Tool input must be an object"))?,
            }),
            "tool_result" => Ok(Content::ToolResult {
                id: required_str(block, "tool_use_id")?.into(),
                content: parse_content(
                    block
                        .get("content")
                        .unwrap_or(&Value::String(String::new())),
                    depth + 1,
                )?,
                is_error: block
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            }),
            "thinking" => Ok(Content::Thinking {
                summary: block
                    .get("thinking")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .into(),
                signature: block
                    .get("signature")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            }),
            "redacted_thinking" => Ok(Content::Thinking {
                summary: String::new(),
                signature: None,
            }),
            "server_tool_use" => Ok(Content::HostedCall {
                id: required_str(block, "id")?.into(),
                name: required_str(block, "name")?.into(),
                input: block.get("input").cloned().unwrap_or(Value::Null),
            }),
            "web_search_tool_result" => Ok(Content::HostedResult {
                id: required_str(block, "tool_use_id")?.into(),
                results: block.get("content").cloned().unwrap_or(Value::Null),
            }),
            other => Err(
                ProxyError::invalid(format!("Unsupported content block '{other}'"))
                    .with_code("unsupported_content_type"),
            ),
        })
        .collect()
}
