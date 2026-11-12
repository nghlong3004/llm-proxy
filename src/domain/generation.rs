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
