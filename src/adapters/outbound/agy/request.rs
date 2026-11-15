//! Lossless neutral-to-Cloud-Code translation, rejecting unsupported capabilities before dispatch.
use super::reasoning::ReasoningSigner;
use crate::{
    accounts::Account,
    config::Config,
    domain::{
        Content, GenerationRequest, RequestContext, Role, Tool, ToolChoice, error::ProxyError,
    },
};
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

pub fn translate(
    request: &GenerationRequest,
    model: &str,
    account: &Account,
    config: &Config,
    signer: &ReasoningSigner,
    context: &RequestContext,
) -> Result<Value, ProxyError> {
    validate(request, model)?;
    let claude = model.starts_with("claude-");
    let mut names = HashMap::new();
    let mut contents = Vec::new();
    for message in &request.messages {
        if message.role == Role::System {
            return Err(unsupported(
                "AGY requires system instructions in the system field",
            ));
        }
        let mut parts = Vec::new();
        for content in &message.content {
            match content {
                Content::Text(text) => parts.push(json!({"text": text})),
                Content::Image { media_type, data } if message.role == Role::User => {
                    if !matches!(
                        media_type.as_str(),
                        "image/png" | "image/jpeg" | "image/webp" | "image/gif"
                    ) {
                        return Err(unsupported("AGY does not support this image media type"));
                    }
                    parts.push(json!({"inlineData": {"mimeType": media_type, "data": data}}));
                }
                Content::ToolCall {
                    id,
                    name,
                    arguments,
                } if message.role == Role::Assistant => {
                    validate_name(name)?;
                    if id.is_empty()
                        || !arguments.is_object()
                        || names.insert(id.as_str(), name.as_str()).is_some()
                    {
                        return Err(unsupported(
                            "AGY requires unique tool IDs and object arguments",
                        ));
                    }
                    let mut call = json!({"name": name, "args": arguments});
                    if claude {
                        call["id"] = json!(id);
                    }
                    parts.push(json!({"functionCall": call}));
                }
                Content::ToolResult {
                    id,
                    content,
                    is_error,
                } if message.role == Role::User => {
                    let name = names
                        .get(id.as_str())
                        .ok_or_else(|| unsupported("AGY tool result has no matching prior call"))?;
                    let mut text = String::new();
                    for block in content {
                        let Content::Text(value) = block else {
                            return Err(unsupported(
                                "AGY tool results currently support text or JSON text only",
                            ));
                        };
                        text.push_str(value);
                    }
                    let result =
                        serde_json::from_str::<Value>(&text).unwrap_or(Value::String(text));
                    let mut response = json!({"result": result});
                    if *is_error {
                        response["error"] = json!(true);
                    }
                    let mut function = json!({"name": name, "response": response});
                    if claude {
                        function["id"] = json!(id);
                    }
                    parts.push(json!({"functionResponse": function}));
                }
                Content::Thinking { signature, .. } if message.role == Role::Assistant => {
                    // Unverified/foreign reasoning is omitted, never forwarded as a raw signature.
                    if let Some(replay) = signature.as_deref().and_then(|signature| {
                        signer.verify(signature, &account.id, &account.generation, model)
                    }) {
                        parts.push(json!({
                            "thought": true,
                            "text": replay.summary,
                            "thoughtSignature": replay.thought_signature,
                        }));
                    }
                }
                _ => return Err(unsupported("Unsupported AGY content or content role")),
            }
        }
        if !parts.is_empty() {
            let role = if message.role == Role::Assistant {
                "model"
            } else {
                "user"
            };
            contents.push(json!({"role": role, "parts": parts}));
        }
    }
    if contents.is_empty() {
        return Err(unsupported("AGY requires nonempty conversation content"));
    }
    let conversation = context.session_id.as_deref().unwrap_or(&context.request_id);
    let session = signer
        .session_id(&account.id, &account.workspace, conversation)
        .ok_or_else(|| ProxyError::protocol("Cannot derive AGY session identity"))?;
    let mut generation = json!({"maxOutputTokens": config.agy_max_output_tokens});
    if claude && model.ends_with("-thinking") {
        if config.agy_thinking_budget >= config.agy_max_output_tokens {
            return Err(unsupported(
                "AGY thinking budget must be smaller than max output tokens",
            ));
        }
        generation["thinkingConfig"] = json!({
            "include_thoughts": true,
            "thinking_budget": config.agy_thinking_budget,
        });
    }
    let mut upstream = json!({
        "sessionId": session,
        "contents": contents,
        "generationConfig": generation,
    });
    if !request.system.is_empty() {
        upstream["systemInstruction"] =
            json!({"role": "user", "parts": [{"text": request.system}]});
    }
    if !request.tools.is_empty() {
        let mut declarations = Vec::new();
        let mut declared = HashSet::new();
        for tool in &request.tools {
            let Tool::Function {
                name,
                description,
                parameters,
            } = tool
            else {
                return Err(unsupported("AGY hosted web search is unsupported"));
            };
            validate_name(name)?;
            if !declared.insert(name) {
                return Err(unsupported("AGY function names must be unique"));
            }
            if parameters.get("type").and_then(Value::as_str) != Some("object") {
                return Err(unsupported(
                    "AGY function parameters require an object-root JSON schema",
                ));
            }
            let mut declaration = json!({"name": name, "description": description});
            match schema(parameters, 0) {
                Ok(parameters) => declaration["parameters"] = parameters,
                // The public Google FunctionDeclaration contract supports full JSON Schema
                // through a mutually exclusive field. Preserve constraints verbatim rather
                // than weakening them to the smaller Google Schema vocabulary.
                Err(_) => declaration["parametersJsonSchema"] = parameters.clone(),
            }
            declarations.push(declaration);
        }
        upstream["tools"] = json!([{"functionDeclarations": declarations}]);
        upstream["toolConfig"] = json!({"functionCallingConfig": {"mode": "VALIDATED"}});
    }
    Ok(json!({
        "project": account.workspace,
        "model": model,
        "userAgent": "antigravity",
        "requestType": "agent",
        "requestId": format!("agent-{}", Uuid::new_v4()),
        "request": upstream,
    }))
}

fn validate(request: &GenerationRequest, model: &str) -> Result<(), ProxyError> {
    if !(model.starts_with("claude-") || model.starts_with("gemini-")) {
        return Err(unsupported(
            "AGY requires an explicitly configured Claude or Gemini model ID",
        ));
    }
    if request.compact {
        return Err(unsupported("AGY compaction is unsupported"));
    }
    if request.output_format.is_some() {
        return Err(unsupported(
            "AGY structured output is unsupported; no verified schema-output contract",
        ));
    }
    if request.tool_choice != ToolChoice::Auto {
        return Err(unsupported("AGY supports automatic tool choice only"));
    }
    if !request.parallel_tools && request.tools.len() > 1 {
        return Err(unsupported(
            "AGY cannot guarantee serial selection among multiple tools",
        ));
    }
    if model.starts_with("gemini-")
        && (!request.tools.is_empty()
            || request
                .messages
                .iter()
                .flat_map(|message| &message.content)
                .any(|content| {
                    matches!(
                        content,
                        Content::ToolCall { .. } | Content::ToolResult { .. }
                    )
                }))
    {
        return Err(unsupported(
            "AGY Gemini tools require signature replay support, which is not implemented",
        ));
    }
    Ok(())
}

pub fn validate_name(name: &str) -> Result<(), ProxyError> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(unsupported(
            "AGY function names must match [A-Za-z0-9_-]{1,64}",
        ));
    }
    Ok(())
}

/// Google Schema is not arbitrary JSON Schema. Never erase constraints to make a schema fit.
fn schema(value: &Value, depth: usize) -> Result<Value, ProxyError> {
    if depth > 64 {
        return Err(unsupported("AGY tool schema nesting exceeds 64"));
    }
    let object = value
        .as_object()
        .ok_or_else(|| unsupported("AGY requires an object tool schema"))?;
    let mut result = Map::new();
    for (key, value) in object {
        let translated = match key.as_str() {
            "type" => {
                let kind = value
                    .as_str()
                    .ok_or_else(|| unsupported("AGY schema union types are unsupported"))?;
                if !matches!(
                    kind,
                    "object" | "array" | "string" | "number" | "integer" | "boolean"
                ) {
                    return Err(unsupported("Unsupported AGY schema type"));
                }
                json!(kind.to_ascii_uppercase())
            }
            "properties" => {
                let properties = value
                    .as_object()
                    .ok_or_else(|| unsupported("Invalid AGY schema properties"))?;
                let mut translated = Map::new();
                for (name, value) in properties {
                    translated.insert(name.clone(), schema(value, depth + 1)?);
                }
                Value::Object(translated)
            }
            "items" => schema(value, depth + 1)?,
            "required" => {
                let required = value
                    .as_array()
                    .ok_or_else(|| unsupported("Invalid AGY schema required list"))?;
                if !required.iter().all(Value::is_string) {
                    return Err(unsupported("AGY schema required entries must be strings"));
                }
                value.clone()
            }
            "enum" => {
                let values = value
                    .as_array()
                    .ok_or_else(|| unsupported("Invalid AGY schema enum"))?;
                if values.is_empty() || !values.iter().all(Value::is_string) {
                    return Err(unsupported(
                        "AGY supports nonempty string schema enums only",
                    ));
                }
                value.clone()
            }
            "description" if value.is_string() => value.clone(),
            // Other JSON Schema keywords are explicitly unsupported, rather than stripped.
            _ => return Err(unsupported("Unsupported AGY tool schema keyword or value")),
        };
        result.insert(key.clone(), translated);
    }
    Ok(Value::Object(result))
}

fn unsupported(message: &'static str) -> ProxyError {
    ProxyError::invalid(message).with_code("agy_unsupported_request")
}
