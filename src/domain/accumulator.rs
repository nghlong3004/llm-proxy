//! Buffer the same normalized events used for SSE; there is no second generation path.
use crate::{domain::error::ProxyError, domain::*};
use std::collections::HashMap;
#[derive(Default)]
pub struct Accumulator {
    id: Option<String>,
    model: String,
    content: Vec<Content>,
    arguments: HashMap<usize, String>,
    terminal: Option<(Usage, StopReason)>,
    bytes: usize,
}
impl Accumulator {
    fn reserve(&mut self, bytes: usize) -> Result<(), ProxyError> {
        self.bytes = self.bytes.saturating_add(bytes);
        if self.bytes > MAX_GENERATED_BYTES {
            Err(ProxyError::protocol("Buffered response exceeds 16 MiB"))
        } else {
            Ok(())
        }
    }

    pub fn accept(&mut self, event: GenerationEvent) -> Result<(), ProxyError> {
        match event {
            GenerationEvent::Start { id, model, .. } => {
                if self.id.is_some() {
                    return Err(ProxyError::protocol("Duplicate message start"));
                }
                self.reserve(id.capacity().saturating_add(model.capacity()))?;
                self.id = Some(id);
                self.model = model;
            }
            GenerationEvent::BlockStart { index, block } => {
                if index >= 4096 {
                    return Err(ProxyError::protocol("Too many response content blocks"));
                }
                if index != self.content.len() {
                    return Err(ProxyError::protocol("Nonsequential content block"));
                }
                self.reserve(block.retained_bytes()?)?;
                self.content.push(block);
            }
            GenerationEvent::TextDelta { index, text } => {
                self.reserve(text.len())?;
                if let Some(Content::Text(value)) = self.content.get_mut(index) {
                    value.push_str(&text);
                } else {
                    return Err(ProxyError::protocol("Text delta has no text block"));
                }
            }
            GenerationEvent::ThinkingDelta { index, text } => {
                self.reserve(text.len())?;
                if let Some(Content::Thinking { summary, .. }) = self.content.get_mut(index) {
                    summary.push_str(&text);
                } else {
                    return Err(ProxyError::protocol("Thinking delta has no thinking block"));
                }
            }
            GenerationEvent::Signature { index, signature } => {
                self.reserve(signature.capacity())?;
                if let Some(Content::Thinking {
                    signature: value, ..
                }) = self.content.get_mut(index)
                {
                    *value = Some(signature);
                } else {
                    return Err(ProxyError::protocol("Signature has no thinking block"));
                }
            }
            GenerationEvent::ToolArgumentsDelta { index, json } => {
                if !matches!(self.content.get(index), Some(Content::ToolCall { .. })) {
                    return Err(ProxyError::protocol("Tool delta has no tool block"));
                }
                self.reserve(json.len())?;
                self.arguments.entry(index).or_default().push_str(&json);
            }
            GenerationEvent::BlockEnd { index } => {
                if index >= self.content.len() {
                    return Err(ProxyError::protocol("Block end has no content block"));
                }
                if let Some(args) = self.arguments.remove(&index) {
                    self.reserve(json_argument_bytes(&args)?.saturating_sub(args.len()))?;
                    let parsed = serde_json::from_str(&args)
                        .map_err(|_| ProxyError::protocol("Invalid JSON tool arguments"))?;
                    if let Some(Content::ToolCall { arguments, .. }) = self.content.get_mut(index) {
                        *arguments = parsed;
                    }
                }
            }
            GenerationEvent::Finish { usage, reason } => self.terminal = Some((usage, reason)),
            GenerationEvent::Ping | GenerationEvent::QuotaSnapshot(_) => {}
        }
        Ok(())
    }
    pub fn finish(self) -> Result<GenerationOutput, ProxyError> {
        let (usage, reason) = self
            .terminal
            .ok_or_else(|| ProxyError::protocol("Upstream stream ended before completion"))?;
        Ok(GenerationOutput {
            id: self
                .id
                .ok_or_else(|| ProxyError::protocol("Response has no message start"))?,
            model: self.model,
            content: self.content,
            usage,
            reason,
        })
    }
}
