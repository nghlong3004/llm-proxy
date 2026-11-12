//! Bridge Claude Code's explicit hosted search subrequest to Codex /alpha/search.
use crate::{domain::error::ProxyError, domain::*};
use serde_json::{Value, json};
pub fn query(request: &GenerationRequest) -> Option<String> {
    let ToolChoice::Named(name) = &request.tool_choice else {
        return None;
    };
    if !request
        .tools
        .iter()
        .any(|t| matches!(t,Tool::WebSearch{name:n,..}if n==name))
    {
        return None;
    }
    request
        .messages
        .iter()
        .rev()
        .filter(|m| m.role == Role::User)
        .find_map(|message| {
            let text = message
                .content
                .iter()
                .filter_map(|c| {
                    if let Content::Text(t) = c {
                        Some(t.as_str())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            let text = text
                .trim()
                .strip_prefix("Perform a web search for the query:")
                .unwrap_or(text.trim())
                .trim();
            (!text.is_empty()).then(|| text.to_owned())
        })
}
pub fn request_body(
    request: &GenerationRequest,
    model: &str,
    session: Option<&str>,
    query: &str,
) -> Value {
    let mut filters = serde_json::Map::new();
    for tool in &request.tools {
        if let Tool::WebSearch {
            allowed_domains,
            blocked_domains,
            ..
        } = tool
        {
            if !allowed_domains.is_empty() {
                filters.insert("allowed_domains".into(), json!(allowed_domains));
            }
            if !blocked_domains.is_empty() {
                filters.insert("blocked_domains".into(), json!(blocked_domains));
            }
        }
    }
    let mut settings = json!({"allowed_callers":["direct"],"external_web_access":true});
    if !filters.is_empty() {
        settings["filters"] = Value::Object(filters);
    }
    json!({"id":session.map(str::to_owned).unwrap_or_else(||format!("search-{}",uuid::Uuid::new_v4())),"model":model,"commands":{"search_query":[{"q":query}]},"settings":settings,"max_output_tokens":2500})
}
pub fn events(
    response: Value,
    request: &GenerationRequest,
    query: &str,
    input_tokens: u64,
) -> Result<Vec<GenerationEvent>, ProxyError> {
    let output = response
        .get("output")
        .and_then(Value::as_str)
        .ok_or_else(|| ProxyError::protocol("Search response has no output text"))?;
    let mut results = vec![];
    let mut seen = std::collections::HashSet::new();
    if let Some(items) = response.get("results").and_then(Value::as_array) {
        for item in items {
            let url = item
                .get("url")
                .or_else(|| item.get("link"))
                .and_then(Value::as_str);
            if let Some(url) = url.filter(|s| s.starts_with("https://") || s.starts_with("http://"))
                && seen.insert(url.to_owned())
            {
                results.push(json!({"type":"web_search_result","url":url,"title":item.get("title").and_then(Value::as_str).unwrap_or(url),"encrypted_content":""}));
            }
        }
    }
    // Search sometimes returns text only. Preserve links as a useful structured fallback.
    for chunk in output.split("](").skip(1) {
        if let Some(url) = chunk
            .split(')')
            .next()
            .filter(|s| s.starts_with("https://") || s.starts_with("http://"))
            && seen.insert(url.to_owned())
        {
            results.push(
                json!({"type":"web_search_result","url":url,"title":url,"encrypted_content":""}),
            );
        }
    }
    let id = format!("srvtoolu_ws_{}", uuid::Uuid::new_v4().simple());
    Ok(vec![
        GenerationEvent::Start {
            id: format!("msg_{}", uuid::Uuid::new_v4().simple()),
            model: request.model.clone(),
            input_tokens,
        },
        GenerationEvent::BlockStart {
            index: 0,
            block: Content::HostedCall {
                id: id.clone(),
                name: "web_search".into(),
                input: json!({"query":query}),
            },
        },
        GenerationEvent::BlockEnd { index: 0 },
        GenerationEvent::BlockStart {
            index: 1,
            block: Content::HostedResult {
                id,
                results: json!(results),
            },
        },
        GenerationEvent::BlockEnd { index: 1 },
        GenerationEvent::BlockStart {
            index: 2,
            block: Content::Text(String::new()),
        },
        GenerationEvent::TextDelta {
            index: 2,
            text: output.into(),
        },
        GenerationEvent::BlockEnd { index: 2 },
        GenerationEvent::Finish {
            usage: Usage {
                input: input_tokens,
                output: tiktoken_rs::o200k_base_singleton()
                    .encode_with_special_tokens(output)
                    .len() as u64,
                ..Usage::default()
            },
            reason: StopReason::EndTurn,
        },
    ])
}
