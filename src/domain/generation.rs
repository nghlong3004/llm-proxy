//! Agent- and provider-neutral messages/events exchanged by protocol and provider strategies.
use crate::domain::error::ProxyError;
use serde_json::Value;
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Role {
    System,
    User,
    Assistant,
}
#[derive(Clone, Debug)]
pub enum Content {
    Text(String),
    Image {
        media_type: String,
        data: String,
    },
    ToolCall {
        id: String,
        name: String,
        arguments: Value,
    },
    ToolResult {
        id: String,
        content: Vec<Content>,
        is_error: bool,
    },
    Thinking {
        summary: String,
        signature: Option<String>,
    },
    HostedCall {
        id: String,
        name: String,
        input: Value,
    },
    HostedResult {
        id: String,
        results: Value,
    },
}
pub(crate) const MAX_GENERATED_BYTES: usize = 16 * 1024 * 1024;

impl Content {
    pub(crate) fn retained_bytes(&self) -> Result<usize, ProxyError> {
        self.retained_bytes_at_depth(0)
    }

    fn retained_bytes_at_depth(&self, depth: usize) -> Result<usize, ProxyError> {
        if depth >= 128 {
            return Err(ProxyError::protocol(
                "Response content is too deeply nested",
            ));
        }
        let mut bytes = std::mem::size_of::<Self>();
        match self {
            Self::Text(text) => bytes = bytes.saturating_add(text.capacity()),
            Self::Image { media_type, data } => {
                bytes = bytes
                    .saturating_add(media_type.capacity())
                    .saturating_add(data.capacity());
            }
            Self::ToolCall {
                id,
                name,
                arguments,
            }
            | Self::HostedCall {
                id,
                name,
                input: arguments,
            } => {
                bytes = bytes
                    .saturating_add(id.capacity())
                    .saturating_add(name.capacity())
                    .saturating_add(json_retained_bytes(arguments)?);
            }
            Self::ToolResult { id, content, .. } => {
                bytes = bytes.saturating_add(id.capacity());
                for block in content {
                    bytes = bytes.saturating_add(block.retained_bytes_at_depth(depth + 1)?);
                    check_retained_bytes(bytes)?;
                }
            }
            Self::Thinking { summary, signature } => {
                bytes = bytes
                    .saturating_add(summary.capacity())
                    .saturating_add(signature.as_ref().map_or(0, String::capacity));
            }
            Self::HostedResult { id, results } => {
                bytes = bytes
                    .saturating_add(id.capacity())
                    .saturating_add(json_retained_bytes(results)?);
            }
        }
        check_retained_bytes(bytes)
    }
}

pub(crate) fn json_argument_bytes(json: &str) -> Result<usize, ProxyError> {
    let mut bytes = json.len().saturating_add(std::mem::size_of::<Value>());
    let mut quoted = false;
    let mut escaped = false;
    // Bound the eventual tree before deserializing, without allocating a second tree to count it.
    for byte in json.bytes() {
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
        } else {
            bytes = bytes.saturating_add(match byte {
                b'"' => {
                    quoted = true;
                    0
                }
                b'[' | b'{' | b',' => std::mem::size_of::<Value>(),
                b':' => std::mem::size_of::<Value>() + std::mem::size_of::<String>(),
                _ => 0,
            });
            check_retained_bytes(bytes)?;
        }
    }
    check_retained_bytes(bytes)
}

pub(crate) fn json_retained_bytes(value: &Value) -> Result<usize, ProxyError> {
    json_retained_bytes_at_depth(value, 0)
}

fn json_retained_bytes_at_depth(value: &Value, depth: usize) -> Result<usize, ProxyError> {
    if depth >= 128 {
        return Err(ProxyError::protocol("Response JSON is too deeply nested"));
    }
    // Count nodes as well as strings: a small JSON array of nulls can retain much more memory.
    let mut bytes = std::mem::size_of::<Value>();
    match value {
        Value::String(text) => bytes = bytes.saturating_add(text.capacity()),
        Value::Array(values) => {
            for value in values {
                bytes = bytes.saturating_add(json_retained_bytes_at_depth(value, depth + 1)?);
                check_retained_bytes(bytes)?;
            }
        }
        Value::Object(values) => {
            for (key, value) in values {
                bytes = bytes
                    .saturating_add(std::mem::size_of::<String>())
                    .saturating_add(key.capacity())
                    .saturating_add(json_retained_bytes_at_depth(value, depth + 1)?);
                check_retained_bytes(bytes)?;
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
    check_retained_bytes(bytes)
}

fn check_retained_bytes(bytes: usize) -> Result<usize, ProxyError> {
    if bytes > MAX_GENERATED_BYTES {
        Err(ProxyError::protocol("Retained response exceeds 16 MiB"))
    } else {
        Ok(bytes)
    }
}

#[derive(Clone, Debug)]
pub struct Message {
    pub role: Role,
    pub content: Vec<Content>,
}
#[derive(Clone, Debug)]
pub enum Tool {
    Function {
        name: String,
        description: String,
        parameters: Value,
    },
    WebSearch {
        name: String,
        allowed_domains: Vec<String>,
        blocked_domains: Vec<String>,
    },
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ToolChoice {
    #[default]
    Auto,
    None,
    Required,
    Named(String),
}
#[derive(Clone, Debug)]
pub struct GenerationRequest {
    pub model: String,
    pub system: String,
    pub messages: Vec<Message>,
    pub tools: Vec<Tool>,
    pub tool_choice: ToolChoice,
    pub parallel_tools: bool,
    pub effort: String,
    pub output_format: Option<Value>,
    pub stream: bool,
    pub compact: bool,
}
#[derive(Clone, Debug, Default)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cached: u64,
    pub reasoning: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
}
#[derive(Clone, Debug)]
pub enum GenerationEvent {
    Start {
        id: String,
        model: String,
        input_tokens: u64,
    },
    BlockStart {
        index: usize,
        block: Content,
    },
    TextDelta {
        index: usize,
        text: String,
    },
    ThinkingDelta {
        index: usize,
        text: String,
    },
    Signature {
        index: usize,
        signature: String,
    },
    ToolArgumentsDelta {
        index: usize,
        json: String,
    },
    BlockEnd {
        index: usize,
    },
    Finish {
        usage: Usage,
        reason: StopReason,
    },
    QuotaSnapshot(Value),
    Ping,
}
#[derive(Clone)]
pub struct RequestContext {
    pub request_id: String,
    pub session_id: Option<String>,
}
#[derive(Clone)]
pub struct GenerationOutput {
    pub id: String,
    pub model: String,
    pub content: Vec<Content>,
    pub usage: Usage,
    pub reason: StopReason,
}
/// The stream is lazy: dropping it must cancel its upstream work.
pub type EventStream =
    futures_util::stream::BoxStream<'static, Result<GenerationEvent, ProxyError>>;
