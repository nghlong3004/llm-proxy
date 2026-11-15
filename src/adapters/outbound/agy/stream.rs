//! Incremental Cloud Code SSE-to-neutral event translation.
use super::{error, reasoning::ReasoningSigner, request::validate_name};
use crate::domain::{
    Content, GenerationEvent, MAX_GENERATED_BYTES, StopReason, Usage, error::ProxyError,
    json_retained_bytes,
};
use serde_json::{Value, json};
use std::collections::HashSet;
use uuid::Uuid;

struct Active {
    index: usize,
    thinking: bool,
    summary: String,
    signature: Option<String>,
}

pub struct Translator {
    response_model: String,
    canonical_model: String,
    account: String,
    generation: String,
    signer: ReasoningSigner,
    started: bool,
    finished: bool,
    next_index: usize,
    active: Option<Active>,
    tool_ids: HashSet<String>,
    used_tools: bool,
    parallel_tools: bool,
    usage: Usage,
    retained: usize,
}

impl Translator {
    pub fn new(
        response_model: String,
        canonical_model: String,
        account: String,
        generation: String,
        signer: ReasoningSigner,
    ) -> Self {
        Self {
            response_model,
            canonical_model,
            account,
            generation,
            signer,
            started: false,
            finished: false,
            next_index: 0,
            active: None,
            tool_ids: HashSet::new(),
            used_tools: false,
            parallel_tools: true,
            usage: Usage::default(),
            retained: 0,
        }
    }

    pub fn with_parallel_tools(mut self, parallel_tools: bool) -> Self {
        self.parallel_tools = parallel_tools;
        self
    }

    pub fn finished(&self) -> bool {
        self.finished
    }

    pub fn accept(&mut self, value: &Value) -> Result<Vec<GenerationEvent>, ProxyError> {
        if self.finished {
            return Err(ProxyError::protocol("AGY sent data after completion"));
        }
        json_retained_bytes(value)?;
        if let Some(error) = error::classify(200, value) {
            return Err(error);
        }
        let response = value.get("response").unwrap_or(value);
        if response
            .pointer("/promptFeedback/blockReason")
            .and_then(Value::as_str)
            .is_some_and(|reason| !reason.is_empty() && reason != "BLOCK_REASON_UNSPECIFIED")
        {
            return Err(ProxyError::protocol("AGY blocked the prompt"));
        }
        if let Some(usage) = response.get("usageMetadata") {
            let prompt = count(usage, "promptTokenCount")?;
            let cached = count(usage, "cachedContentTokenCount")?;
            let candidates = count(usage, "candidatesTokenCount")?;
            let thoughts = count(usage, "thoughtsTokenCount")?;
            if cached > prompt {
                return Err(ProxyError::protocol("Invalid AGY cached token count"));
            }
            self.usage = Usage {
                input: prompt,
                cached,
                output: candidates
                    .checked_add(thoughts)
                    .ok_or_else(|| ProxyError::protocol("AGY usage overflow"))?,
                reasoning: thoughts,
            };
        }
        let Some(candidates) = response.get("candidates") else {
            return Ok(Vec::new());
        };
        let candidates = candidates
            .as_array()
            .ok_or_else(|| ProxyError::protocol("Invalid AGY candidates"))?;
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        if candidates.len() != 1 {
            return Err(ProxyError::protocol("AGY returned multiple candidates"));
        }
        let candidate = &candidates[0];
        let finish = match candidate.get("finishReason") {
            None => None,
            Some(Value::String(reason)) if reason == "STOP" => Some(StopReason::EndTurn),
            Some(Value::String(reason)) if reason == "MAX_TOKENS" => Some(StopReason::MaxTokens),
            Some(_) => {
                return Err(ProxyError::protocol(
                    "AGY stopped for an unsupported or blocked reason",
                ));
            }
        };
        let mut events = Vec::new();
        if !self.started {
            self.started = true;
            events.push(GenerationEvent::Start {
                id: format!("msg_agy_{}", Uuid::new_v4().simple()),
                model: self.response_model.clone(),
                input_tokens: self.usage.input,
            });
        }
        if let Some(content) = candidate.get("content") {
            if content
                .get("role")
                .and_then(Value::as_str)
                .is_some_and(|role| role != "model")
            {
                return Err(ProxyError::protocol("Invalid AGY response role"));
            }
            let parts = content
                .get("parts")
                .and_then(Value::as_array)
                .ok_or_else(|| ProxyError::protocol("Invalid AGY response parts"))?;
            for part in parts {
                self.part(part, &mut events)?;
            }
        }
        if let Some(mut reason) = finish {
            self.close(&mut events)?;
            if self.used_tools {
                reason = StopReason::ToolUse;
            }
            self.finished = true;
            events.push(GenerationEvent::Finish {
                usage: self.usage.clone(),
                reason,
            });
        }
        Ok(events)
    }

    fn part(&mut self, part: &Value, events: &mut Vec<GenerationEvent>) -> Result<(), ProxyError> {
        if let Some(call) = part.get("functionCall") {
            if self.used_tools && !self.parallel_tools {
                return Err(ProxyError::protocol(
                    "AGY returned parallel tool calls despite serial policy",
                ));
            }
            if self.canonical_model.starts_with("gemini-") {
                return Err(ProxyError::protocol(
                    "AGY Gemini tool signatures are unsupported",
                ));
            }
            self.close(events)?;
            let name = call
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| ProxyError::protocol("AGY tool call has no name"))?;
            validate_name(name).map_err(|_| ProxyError::protocol("Invalid AGY function name"))?;
            let arguments = call
                .get("args")
                .filter(|value| value.is_object())
                .ok_or_else(|| ProxyError::protocol("AGY tool call arguments must be an object"))?;
            let id = match call.get("id") {
                Some(Value::String(id)) if !id.is_empty() => id.clone(),
                None => format!("toolu_agy_{}", Uuid::new_v4().simple()),
                _ => return Err(ProxyError::protocol("Invalid AGY tool call ID")),
            };
            if !self.tool_ids.insert(id.clone()) {
                return Err(ProxyError::protocol("Duplicate AGY tool call ID"));
            }
            self.retain(
                id.len()
                    .saturating_add(name.len())
                    .saturating_add(json_retained_bytes(arguments)?),
            )?;
            let index = self.index()?;
            events.push(GenerationEvent::BlockStart {
                index,
                block: Content::ToolCall {
                    id,
                    name: name.into(),
                    arguments: json!({}),
                },
            });
            events.push(GenerationEvent::ToolArgumentsDelta {
                index,
                json: arguments.to_string(),
            });
            events.push(GenerationEvent::BlockEnd { index });
            self.used_tools = true;
            return Ok(());
        }
        let thinking = match part.get("thought") {
            None => false,
            Some(Value::Bool(thinking)) => *thinking,
            _ => return Err(ProxyError::protocol("Invalid AGY thought marker")),
        };
        let text = part.get("text").and_then(Value::as_str);
        let signature = part.get("thoughtSignature").and_then(Value::as_str);
        if text.is_none() && !(thinking && signature.is_some()) {
            return Err(ProxyError::protocol("Unsupported AGY response part"));
        }
        if signature.is_some() && !thinking {
            return Err(ProxyError::protocol(
                "AGY returned a signature outside a thought",
            ));
        }
        if self
            .active
            .as_ref()
            .is_some_and(|active| active.thinking != thinking)
        {
            self.close(events)?;
        }
        if self.active.is_none() {
            let index = self.index()?;
            let block = if thinking {
                Content::Thinking {
                    summary: String::new(),
                    signature: None,
                }
            } else {
                Content::Text(String::new())
            };
            events.push(GenerationEvent::BlockStart { index, block });
            self.active = Some(Active {
                index,
                thinking,
                summary: String::new(),
                signature: None,
            });
        }
        if let Some(text) = text {
            self.retain(text.len())?;
            let active = self
                .active
                .as_mut()
                .ok_or_else(|| ProxyError::protocol("Missing AGY block"))?;
            if thinking {
                active.summary.push_str(text);
                events.push(GenerationEvent::ThinkingDelta {
                    index: active.index,
                    text: text.into(),
                });
            } else {
                events.push(GenerationEvent::TextDelta {
                    index: active.index,
                    text: text.into(),
                });
            }
        }
        if let Some(signature) = signature {
            self.retain(signature.len())?;
            let active = self
                .active
                .as_mut()
                .ok_or_else(|| ProxyError::protocol("Missing AGY thinking block"))?;
            active.signature = Some(signature.to_owned());
        }
        Ok(())
    }

    fn close(&mut self, events: &mut Vec<GenerationEvent>) -> Result<(), ProxyError> {
        if let Some(active) = self.active.take() {
            // Emit one envelope only when the whole block is known. A signature can
            // arrive before its final text; signing each chunk would truncate replay
            // and concatenate incompatible envelopes in streaming clients.
            if let Some(signature) = active.signature {
                let signature = self
                    .signer
                    .sign(
                        &self.account,
                        &self.generation,
                        &self.canonical_model,
                        &active.summary,
                        &signature,
                    )
                    .ok_or_else(|| {
                        ProxyError::protocol("AGY reasoning signature exceeds limits")
                    })?;
                self.retain(signature.len())?;
                events.push(GenerationEvent::Signature {
                    index: active.index,
                    signature,
                });
            }
            events.push(GenerationEvent::BlockEnd {
                index: active.index,
            });
        }
        Ok(())
    }

    fn index(&mut self) -> Result<usize, ProxyError> {
        if self.next_index >= 4096 {
            return Err(ProxyError::protocol("AGY returned too many content blocks"));
        }
        self.retain(std::mem::size_of::<Content>())?;
        let index = self.next_index;
        self.next_index += 1;
        Ok(index)
    }

    fn retain(&mut self, bytes: usize) -> Result<(), ProxyError> {
        self.retained = self.retained.saturating_add(bytes);
        if self.retained > MAX_GENERATED_BYTES {
            return Err(ProxyError::protocol("AGY retained response exceeds 16 MiB"));
        }
        Ok(())
    }
}

fn count(value: &Value, key: &str) -> Result<u64, ProxyError> {
    match value.get(key) {
        None => Ok(0),
        Some(value) => value
            .as_u64()
            .ok_or_else(|| ProxyError::protocol("Invalid AGY token count")),
    }
}
