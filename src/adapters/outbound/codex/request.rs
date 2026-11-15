//! Shared generation request → Codex Responses wire format.
//! Protocol rules follow raine/claude-code-proxy; see LICENSE and schema.rs.
use super::{
    reasoning::ReasoningSigner,
    schema::{normalize_strict_json_schema, strip_tool_schema_patterns},
};
use crate::{
    config::DEFAULT_CODEX_EFFORT,
    domain::error::ProxyError,
    domain::*,
    ports::request_routing::{RequestRoute, RouteOptions},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};

pub fn translate(
    request: &GenerationRequest,
    route: &RequestRoute,
    account: &str,
    generation: &str,
    signer: &ReasoningSigner,
    session: Option<&str>,
) -> Result<Value, ProxyError> {
    let RouteOptions::Codex(model) = &route.options else {
        return Err(ProxyError::invalid("Codex requires Codex route options")
            .with_code("invalid_request_route"));
    };
    if route.provider != "codex" {
        return Err(
            ProxyError::invalid("Codex route provider mismatch").with_code("invalid_request_route")
        );
    }
    let hosted_search = request
        .tools
        .iter()
        .any(|t| matches!(t, Tool::WebSearch { .. }));
    let lite = model.responses_lite && !hosted_search;
    let actual_model = if hosted_search {
        model.search_model.as_str()
    } else {
        route.model.as_str()
    };
    let mut input = vec![];
    for message in &request.messages {
        let role = match message.role {
            Role::System => "developer",
            Role::User => "user",
            Role::Assistant => "assistant",
        };
        let mut text = vec![];
        for content in &message.content {
            match content {
                Content::Text(value) => text.push(text_part(value, role)),
                Content::Image { media_type, data } => text.push(image_part(media_type, data)?),
                Content::Thinking { summary, signature } => {
                    if let Some(replay) = signature
                        .as_ref()
                        .filter(|_| request.purpose == RequestPurpose::Conversation)
                        .and_then(|s| signer.verify(s, account, generation, actual_model))
                    {
                        flush(&mut input, &mut text, role);
                        input.push(json!({"type":"reasoning","id":replay.id,"summary":[],"encrypted_content":replay.encrypted_content}));
                    } else if !summary.is_empty() {
                        text.push(text_part(
                            &format!("Prior reasoning summary:\n{summary}"),
                            role,
                        ));
                    }
                }
                Content::ToolCall {
                    id,
                    name,
                    arguments,
                } => {
                    flush(&mut input, &mut text, role);
                    input.push(json!({"type":"function_call","call_id":id,"name":name,"arguments":arguments.to_string()}));
                }
                Content::ToolResult {
                    id,
                    content,
                    is_error,
                } => {
                    flush(&mut input, &mut text, role);
                    let parts = tool_result_parts(content, *is_error)?;
                    let output = if parts.iter().any(|v| v["type"] == "input_image") {
                        Value::Array(parts)
                    } else {
                        Value::String(
                            parts
                                .iter()
                                .filter_map(|v| v["text"].as_str())
                                .collect::<Vec<_>>()
                                .join("\n"),
                        )
                    };
                    input.push(json!({"type":"function_call_output","call_id":id,"output":output}));
                }
                Content::HostedCall {
                    name, input: args, ..
                } => text.push(text_part(&format!("Hosted tool {name}: {args}"), role)),
                Content::HostedResult { results, .. } => {
                    text.push(text_part(&format!("Web search results: {results}"), role))
                }
            }
        }
        flush(&mut input, &mut text, role);
    }
    let tools=request.tools.iter().map(|tool|match tool {
        Tool::Function { name, description, parameters } => {
            let mut parameters = parameters.clone();
            strip_tool_schema_patterns(&mut parameters);
            json!({"type":"function","name":name,"description":description,"parameters":parameters,"strict":false})
        },
        Tool::WebSearch {allowed_domains,blocked_domains,..}=>{
            let mut filters=serde_json::Map::new();
            if !allowed_domains.is_empty(){filters.insert("allowed_domains".into(),json!(allowed_domains));}
            if !blocked_domains.is_empty(){filters.insert("blocked_domains".into(),json!(blocked_domains));}
            let mut tool=json!({"type":"web_search","external_web_access":true,"search_content_types":["text","image"]});
            if !filters.is_empty(){tool["filters"]=Value::Object(filters);}
            tool
        }
    }).collect::<Vec<_>>();
    let mut choice = match &request.tool_choice {
        ToolChoice::Auto => json!("auto"),
        ToolChoice::None => json!("none"),
        ToolChoice::Required => json!("required"),
        ToolChoice::Named(name) => json!({"type":"function","name":name}),
    };
    if let ToolChoice::Named(name) = &request.tool_choice
        && request
            .tools
            .iter()
            .any(|t| matches!(t,Tool::WebSearch {name:n,..} if n==name))
    {
        choice = json!({"type":"allowed_tools","mode":"required","tools":[{"type":"web_search"}]});
    }
    let mut body = json!({"model":actual_model,"input":input,"stream":true,"store":false,"parallel_tool_calls":request.parallel_tools && !lite,"text":{"verbosity":"low"}});
    if request.tool_choice != ToolChoice::Auto {
        body["tool_choice"] = choice;
    }
    if lite {
        let mut prefix = vec![];
        if !tools.is_empty() {
            prefix.push(json!({"type":"additional_tools","role":"developer","tools":tools}));
        }
        if !request.system.is_empty() {
            prefix.push(json!({"type":"message","role":"developer","content":[{"type":"input_text","text":request.system}]}));
        }
        let mut all = prefix;
        all.append(
            body["input"]
                .as_array_mut()
                .ok_or_else(|| ProxyError::protocol("Internal input encoding error"))?,
        );
        body["input"] = json!(all);
        body["client_metadata"] =
            json!({"ws_request_header_x_openai_internal_codex_responses_lite":"true"});
    } else {
        if !request.system.is_empty() {
            body["instructions"] = json!(request.system);
        }
        if !tools.is_empty() {
            body["tools"] = json!(tools);
        }
    }
    if let Some(session) = session {
        body["prompt_cache_key"] = json!(session);
    }
    if let Some(tier) = &model.service_tier {
        body["service_tier"] = json!(tier.as_str());
    }
    let effort = request.effort.unwrap_or(DEFAULT_CODEX_EFFORT);
    let effort =
        if request.compact && !matches!(effort, ReasoningEffort::None | ReasoningEffort::Low) {
            ReasoningEffort::Low
        } else {
            effort
        }
        .as_str();
    if effort != "none" {
        body["reasoning"] = json!({"effort":effort,"summary":"auto"});
        body["include"] = json!(["reasoning.encrypted_content"]);
    } else {
        body["reasoning"] = json!({"effort":"none"});
    }
    if lite {
        body["reasoning"]["context"] = json!("all_turns");
    }
    if let Some(format) = &request.output_format {
        if format.get("type").and_then(Value::as_str) != Some("json_schema")
            || !format.get("schema").is_some_and(Value::is_object)
        {
            return Err(ProxyError::invalid(
                "Only json_schema output format is supported",
            ));
        }
        let mut format = format.clone();
        format["name"] = json!(
            format
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("response")
        );
        format["strict"] = json!(true);
        format["schema"] = normalize_strict_json_schema(&format["schema"]);
        body["text"]["format"] = format;
    }
    Ok(body)
}
fn text_part(text: &str, role: &str) -> Value {
    json!({"type":if role=="assistant" {"output_text"} else {"input_text"},"text":text})
}
fn flush(input: &mut Vec<Value>, parts: &mut Vec<Value>, role: &str) {
    if !parts.is_empty() {
        input.push(json!({"type":"message","role":role,"content":std::mem::take(parts)}));
    }
}
fn image_part(media_type: &str, data: &str) -> Result<Value, ProxyError> {
    if data.len() > 20 * 1024 * 1024 {
        return Err(ProxyError::invalid("Image is too large"));
    }
    let mut data = data.to_owned();
    while !data.len().is_multiple_of(4) {
        data.push('=');
    }
    let bytes = STANDARD
        .decode(&data)
        .map_err(|_| ProxyError::invalid("Invalid base64 image"))?;
    if bytes.is_empty() {
        return Err(ProxyError::invalid("Image is empty"));
    }
    Ok(
        json!({"type":"input_image","image_url":format!("data:{media_type};base64,{}",STANDARD.encode(bytes))}),
    )
}
fn tool_result_parts(content: &[Content], is_error: bool) -> Result<Vec<Value>, ProxyError> {
    let mut parts = vec![];
    if is_error {
        parts.push(json!({"type":"input_text","text":"[tool execution error]"}));
    }
    for block in content {
        match block {
            Content::Text(text) => parts.push(json!({"type":"input_text","text":text})),
            Content::Image { media_type, data } => parts.push(image_part(media_type, data)?),
            _ => {
                return Err(ProxyError::invalid(
                    "Unsupported nested tool-result content",
                ));
            }
        }
    }
    Ok(parts)
}
pub fn estimate_tokens(request: &GenerationRequest) -> u64 {
    let tokenizer = tiktoken_rs::o200k_base_singleton();
    let mut total = tokenizer.encode_with_special_tokens(&request.system).len() as u64 + 16;
    for message in &request.messages {
        total += 4;
        for block in &message.content {
            total += content_tokens(block);
        }
    }
    for tool in &request.tools {
        total += match tool {
            Tool::Function {
                name,
                description,
                parameters,
            } => {
                tokenizer
                    .encode_with_special_tokens(&format!("{name}\n{description}\n{parameters}"))
                    .len() as u64
                    + 16
            }
            Tool::WebSearch { .. } => 64,
        };
    }
    total
}
fn content_tokens(content: &Content) -> u64 {
    let tokenizer = tiktoken_rs::o200k_base_singleton();
    match content {
        Content::Text(text) => tokenizer.encode_with_special_tokens(text).len() as u64,
        // shortcut: conservative local image estimate; upstream billing may differ.
        Content::Image { .. } => 4096,
        Content::Thinking { summary, signature } => {
            tokenizer.encode_with_special_tokens(summary).len() as u64
                + signature.as_ref().map_or(0, |s| s.len() as u64 / 4)
        }
        Content::ToolCall {
            name, arguments, ..
        } => {
            tokenizer
                .encode_with_special_tokens(&format!("{name}{arguments}"))
                .len() as u64
                + 8
        }
        Content::ToolResult { content, .. } => content.iter().map(content_tokens).sum::<u64>() + 8,
        Content::HostedCall { input, .. } => tokenizer
            .encode_with_special_tokens(&input.to_string())
            .len() as u64,
        Content::HostedResult { results, .. } => tokenizer
            .encode_with_special_tokens(&results.to_string())
            .len() as u64,
    }
}
