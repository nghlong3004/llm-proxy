//! Reduce native Responses events into protocol-neutral generation events.
use super::reasoning::ReasoningSigner;
use crate::{domain::error::ProxyError, domain::*};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashSet};
const MAX_OUTPUT: usize = 16 * 1024 * 1024;
struct Text {
    index: usize,
    text: String,
}
struct ToolCall {
    index: usize,
    args: String,
    had_delta: bool,
    done: bool,
}
#[derive(Default)]
struct Thinking {
    index: Option<usize>,
    id: Option<String>,
    encrypted: Option<String>,
    done: bool,
    summaries: BTreeMap<usize, String>,
}
pub struct Translator {
    client_model: String,
    model: String,
    account: String,
    generation: String,
    signer: ReasoningSigner,
    id: String,
    estimated_input: u64,
    started: bool,
    finished: bool,
    next: usize,
    bytes: usize,
    open: BTreeSet<usize>,
    texts: BTreeMap<(usize, usize), Text>,
    tools: BTreeMap<usize, ToolCall>,
    thinking: BTreeMap<usize, Thinking>,
    searches: Vec<String>,
    seen_searches: HashSet<String>,
    citations: Vec<Value>,
    seen_urls: HashSet<String>,
    citation_bytes: usize,
}
impl Translator {
    pub fn new(
        client_model: String,
        model: String,
        account: String,
        generation: String,
        signer: ReasoningSigner,
        estimated_input: u64,
    ) -> Self {
        Self {
            client_model,
            model,
            account,
            generation,
            signer,
            id: format!("msg_{}", uuid::Uuid::new_v4().simple()),
            estimated_input,
            started: false,
            finished: false,
            next: 0,
            bytes: 0,
            open: BTreeSet::new(),
            texts: BTreeMap::new(),
            tools: BTreeMap::new(),
            thinking: BTreeMap::new(),
            searches: vec![],
            seen_searches: HashSet::new(),
            citations: vec![],
            seen_urls: HashSet::new(),
            citation_bytes: 0,
        }
    }
    pub fn finished(&self) -> bool {
        self.finished
    }
    fn reserve(&mut self, bytes: usize) -> Result<(), ProxyError> {
        self.bytes = self.bytes.saturating_add(bytes);
        if self.bytes > MAX_OUTPUT {
            Err(ProxyError::protocol("Generated output exceeds 16 MiB"))
        } else {
            Ok(())
        }
    }
    fn start(&mut self, out: &mut Vec<GenerationEvent>) {
        if !self.started {
            self.started = true;
            out.push(GenerationEvent::Start {
                id: self.id.clone(),
                model: self.client_model.clone(),
                input_tokens: self.estimated_input,
            });
        }
    }
    fn block(
        &mut self,
        block: Content,
        out: &mut Vec<GenerationEvent>,
    ) -> Result<usize, ProxyError> {
        if self.next >= 4096 {
            return Err(ProxyError::protocol("Too many response content blocks"));
        }
        self.start(out);
        let index = self.next;
        self.next += 1;
        self.open.insert(index);
        out.push(GenerationEvent::BlockStart { index, block });
        Ok(index)
    }
    fn close(&mut self, index: usize, out: &mut Vec<GenerationEvent>) {
        if self.open.remove(&index) {
            out.push(GenerationEvent::BlockEnd { index });
        }
    }
    fn text_delta(
        &mut self,
        key: (usize, usize),
        delta: &str,
        out: &mut Vec<GenerationEvent>,
    ) -> Result<(), ProxyError> {
        if self
            .texts
            .get(&key)
            .is_some_and(|text| !self.open.contains(&text.index))
        {
            return Err(ProxyError::protocol(
                "Text delta arrived after its block stopped",
            ));
        }
        self.reserve(delta.len())?;
        if !self.texts.contains_key(&key) {
            let index = self.block(Content::Text(String::new()), out)?;
            self.texts.insert(
                key,
                Text {
                    index,
                    text: String::new(),
                },
            );
        }
        let text = self
            .texts
            .get_mut(&key)
            .ok_or_else(|| ProxyError::protocol("Missing text state"))?;
        text.text.push_str(delta);
        out.push(GenerationEvent::TextDelta {
            index: text.index,
            text: delta.into(),
        });
        Ok(())
    }
    fn text_done(
        &mut self,
        key: (usize, usize),
        full: Option<&str>,
        out: &mut Vec<GenerationEvent>,
    ) -> Result<(), ProxyError> {
        if let Some(full) = full {
            let current_len = self.texts.get(&key).map_or(0, |s| s.text.len());
            if !full.starts_with(self.texts.get(&key).map_or("", |s| s.text.as_str())) {
                return Err(ProxyError::protocol(
                    "Upstream text changed after being streamed",
                ));
            }
            let suffix = &full[current_len..];
            if !suffix.is_empty() || !self.texts.contains_key(&key) {
                self.text_delta(key, suffix, out)?;
            }
        }
        if let Some(index) = self.texts.get(&key).map(|s| s.index) {
            self.close(index, out);
        }
        Ok(())
    }
    fn thinking_index(
        &mut self,
        output: usize,
        out: &mut Vec<GenerationEvent>,
    ) -> Result<usize, ProxyError> {
        if let Some(index) = self.thinking.get(&output).and_then(|s| s.index) {
            return Ok(index);
        }
        let index = self.block(
            Content::Thinking {
                summary: String::new(),
                signature: None,
            },
            out,
        )?;
        self.thinking.entry(output).or_default().index = Some(index);
        Ok(index)
    }
    fn capture_thinking(&mut self, output: usize, item: &Value) -> Result<(), ProxyError> {
        let previous = self.thinking.get(&output);
        let id = item
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| previous.and_then(|s| s.id.as_deref()) != Some(*id));
        let encrypted = item
            .get("encrypted_content")
            .and_then(Value::as_str)
            .filter(|value| previous.and_then(|s| s.encrypted.as_deref()) != Some(*value));
        self.reserve(
            id.map_or(0, str::len)
                .saturating_add(encrypted.map_or(0, str::len)),
        )?;
        let state = self.thinking.entry(output).or_default();
        if let Some(id) = id {
            state.id = Some(id.into());
        }
        if let Some(encrypted) = encrypted {
            state.encrypted = Some(encrypted.into());
        }
        Ok(())
    }
    fn thinking_delta(
        &mut self,
        output: usize,
        part: usize,
        text: &str,
        out: &mut Vec<GenerationEvent>,
    ) -> Result<(), ProxyError> {
        if part >= 4096 || self.thinking.get(&output).is_some_and(|state| state.done) {
            return Err(ProxyError::protocol("Invalid reasoning summary lifecycle"));
        }
        let latest = self
            .thinking
            .get(&output)
            .and_then(|s| s.summaries.last_key_value().map(|(index, _)| *index));
        if latest.is_some_and(|latest| latest > part) {
            return Err(ProxyError::protocol(
                "Reasoning summary parts arrived out of order",
            ));
        }
        self.reserve(text.len())?;
        let index = self.thinking_index(output, out)?;
        if latest.is_some_and(|latest| latest < part) {
            self.reserve(2)?;
            out.push(GenerationEvent::ThinkingDelta {
                index,
                text: "\n\n".into(),
            });
        }
        self.thinking
            .entry(output)
            .or_default()
            .summaries
            .entry(part)
            .or_default()
            .push_str(text);
        out.push(GenerationEvent::ThinkingDelta {
            index,
            text: text.into(),
        });
        Ok(())
    }
    fn thinking_done(
        &mut self,
        output: usize,
        part: usize,
        full: &str,
        out: &mut Vec<GenerationEvent>,
    ) -> Result<(), ProxyError> {
        let current = self
            .thinking
            .get(&output)
            .and_then(|s| s.summaries.get(&part))
            .map(String::as_str)
            .unwrap_or("");
        if !full.starts_with(current) {
            return Err(ProxyError::protocol(
                "Reasoning summary changed after being streamed",
            ));
        }
        let suffix = &full[current.len()..];
        if !suffix.is_empty() {
            self.thinking_delta(output, part, suffix, out)?;
        }
        Ok(())
    }
    fn item(
        &mut self,
        output: usize,
        item: &Value,
        done: bool,
        out: &mut Vec<GenerationEvent>,
    ) -> Result<(), ProxyError> {
        if output >= 4096 {
            return Err(ProxyError::protocol("Too many native response items"));
        }
        match item.get("type").and_then(Value::as_str).unwrap_or("") {
            "function_call" => {
                if !self.tools.contains_key(&output) {
                    let id = item
                        .get("call_id")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| ProxyError::protocol("Tool call has no call_id"))?;
                    let name = item
                        .get("name")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| ProxyError::protocol("Tool call has no name"))?;
                    self.reserve(id.len().saturating_add(name.len()))?;
                    let index = self.block(
                        Content::ToolCall {
                            id: id.into(),
                            name: name.into(),
                            arguments: json!({}),
                        },
                        out,
                    )?;
                    self.tools.insert(
                        output,
                        ToolCall {
                            index,
                            args: String::new(),
                            had_delta: false,
                            done: false,
                        },
                    );
                }
                if done {
                    self.tool_done(output, item.get("arguments").and_then(Value::as_str), out)?;
                }
            }
            "reasoning" => {
                if self.thinking.get(&output).is_some_and(|s| s.done) {
                    return Ok(());
                }
                self.capture_thinking(output, item)?;
                if done {
                    if let Some(parts) = item.get("summary").and_then(Value::as_array) {
                        for (part, summary) in parts.iter().enumerate() {
                            if let Some(text) = summary.get("text").and_then(Value::as_str) {
                                self.thinking_done(output, part, text, out)?;
                            }
                        }
                    }
                    let signature = self.thinking.get(&output).and_then(|s| {
                        self.signer.sign(
                            &self.account,
                            &self.generation,
                            &self.model,
                            s.id.as_deref()?,
                            s.encrypted.as_deref()?,
                        )
                    });
                    if let Some(signature) = signature {
                        let index = self.thinking_index(output, out)?;
                        self.reserve(signature.len())?;
                        out.push(GenerationEvent::Signature { index, signature });
                    }
                    if let Some(index) = self.thinking.get(&output).and_then(|s| s.index) {
                        self.close(index, out);
                    }
                    if let Some(state) = self.thinking.get_mut(&output) {
                        state.done = true;
                    }
                }
            }
            "message" if done => {
                if let Some(parts) = item.get("content").and_then(Value::as_array) {
                    for (content_index, part) in parts.iter().enumerate() {
                        self.collect_citations(part)?;
                        if matches!(
                            part.get("type").and_then(Value::as_str),
                            Some("output_text" | "refusal")
                        ) {
                            self.text_done(
                                (output, content_index),
                                part.get("text")
                                    .or_else(|| part.get("refusal"))
                                    .and_then(Value::as_str),
                                out,
                            )?;
                        }
                    }
                }
            }
            "web_search_call" if done => {
                let id = item.get("id").and_then(Value::as_str).unwrap_or("search");
                if !self.seen_searches.contains(id) {
                    self.reserve(id.len().saturating_mul(2).saturating_add("srvtoolu_".len()))?;
                    self.seen_searches.insert(id.to_owned());
                    let id = format!(
                        "srvtoolu_{}",
                        id.replace(|c: char| !c.is_ascii_alphanumeric() && c != '_', "_")
                    );
                    let query = item
                        .pointer("/action/query")
                        .or_else(|| item.pointer("/action/queries/0"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    self.reserve(id.len().saturating_add(query.len()).saturating_add(512))?;
                    let index = self.block(
                        Content::HostedCall {
                            id: id.clone(),
                            name: "web_search".into(),
                            input: json!({"query":query}),
                        },
                        out,
                    )?;
                    self.close(index, out);
                    self.searches.push(id);
                }
            }
            _ => {}
        }
        Ok(())
    }
    fn tool_done(
        &mut self,
        output: usize,
        full: Option<&str>,
        out: &mut Vec<GenerationEvent>,
    ) -> Result<(), ProxyError> {
        if self.tools.get(&output).is_some_and(|state| state.done) {
            return Ok(());
        }
        if let Some(full) = full.filter(|_| {
            self.tools
                .get(&output)
                .is_some_and(|state| !state.had_delta)
        }) {
            self.reserve(full.len())?;
        }
        let state = self
            .tools
            .get(&output)
            .ok_or_else(|| ProxyError::protocol("Tool argument event preceded its call"))?;
        let args = full.filter(|_| !state.had_delta).unwrap_or(&state.args);
        self.reserve(json_argument_bytes(args)?.saturating_sub(args.len()))?;
        let state = self
            .tools
            .get_mut(&output)
            .ok_or_else(|| ProxyError::protocol("Tool argument event preceded its call"))?;
        if let Some(full) = full {
            if !state.had_delta {
                state.args = full.into();
                out.push(GenerationEvent::ToolArgumentsDelta {
                    index: state.index,
                    json: full.into(),
                });
            } else if state.args != full {
                return Err(ProxyError::protocol(
                    "Upstream tool arguments changed after being streamed",
                ));
            }
        }
        if !serde_json::from_str::<Value>(&state.args).is_ok_and(|value| value.is_object()) {
            return Err(ProxyError::protocol("Tool arguments are not a JSON object"));
        }
        state.done = true;
        let index = state.index;
        self.close(index, out);
        Ok(())
    }
    fn collect_citations(&mut self, part: &Value) -> Result<(), ProxyError> {
        if let Some(annotations) = part.get("annotations").and_then(Value::as_array) {
            for a in annotations {
                let a = a.get("url_citation").unwrap_or(a);
                if let Some(url) = a
                    .get("url")
                    .and_then(Value::as_str)
                    .filter(|s| s.starts_with("https://") || s.starts_with("http://"))
                    && !self.seen_urls.contains(url)
                {
                    if self.citations.len() >= 4096 {
                        return Err(ProxyError::protocol("Too many search citations"));
                    }
                    let title = a.get("title").and_then(Value::as_str).unwrap_or(url);
                    // Include worst-case JSON escaping before allocating result copies.
                    let bytes = url
                        .len()
                        .saturating_add(title.len())
                        .saturating_mul(6)
                        .saturating_add(512);
                    self.reserve(bytes.saturating_add(url.len()))?;
                    self.citation_bytes = self.citation_bytes.saturating_add(bytes);
                    self.seen_urls.insert(url.into());
                    self.citations.push(json!({"type":"web_search_result","url":url,"title":title,"encrypted_content":""}));
                }
            }
        }
        Ok(())
    }
    pub fn accept(&mut self, event: &Value) -> Result<Vec<GenerationEvent>, ProxyError> {
        if self.finished {
            return Ok(vec![]);
        }
        if let Some(error) = super::quota::classify(
            200,
            event,
            &axum::http::HeaderMap::new(),
            crate::accounts::now(),
        ) {
            return Err(error);
        }
        let mut out = vec![];
        let output = event
            .get("output_index")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        let content = event
            .get("content_index")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        if output >= 4096 || content >= 4096 {
            return Err(ProxyError::protocol(
                "Native response index exceeds the item limit",
            ));
        }
        let summary = event
            .get("summary_index")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        match event.get("type").and_then(Value::as_str).unwrap_or("") {
            "codex.rate_limits" => {
                self.reserve(json_retained_bytes(event)?)?;
                out.push(GenerationEvent::QuotaSnapshot(event.clone()));
            }
            "response.created" | "response.in_progress" | "keepalive" => {
                out.push(GenerationEvent::Ping)
            }
            "response.output_item.added" => self.item(
                output,
                event.get("item").unwrap_or(&Value::Null),
                false,
                &mut out,
            )?,
            "response.output_item.done" => self.item(
                output,
                event.get("item").unwrap_or(&Value::Null),
                true,
                &mut out,
            )?,
            "response.output_text.delta" | "response.refusal.delta" => {
                let delta = event
                    .get("delta")
                    .and_then(Value::as_str)
                    .ok_or_else(|| ProxyError::protocol("Missing text delta"))?;
                self.text_delta((output, content), delta, &mut out)?;
            }
            "response.output_text.done" | "response.refusal.done" => self.text_done(
                (output, content),
                event
                    .get("text")
                    .or_else(|| event.get("refusal"))
                    .and_then(Value::as_str),
                &mut out,
            )?,
            "response.content_part.done" => {
                let part = event.get("part").unwrap_or(&Value::Null);
                self.collect_citations(part)?;
                if matches!(
                    part.get("type").and_then(Value::as_str),
                    Some("output_text" | "refusal")
                ) {
                    self.text_done(
                        (output, content),
                        part.get("text")
                            .or_else(|| part.get("refusal"))
                            .and_then(Value::as_str),
                        &mut out,
                    )?;
                }
            }
            "response.reasoning_summary_text.delta" => {
                let text = event.get("delta").and_then(Value::as_str).unwrap_or("");
                self.thinking_delta(output, summary, text, &mut out)?;
            }
            "response.reasoning_summary_text.done" => {
                let text = event.get("text").and_then(Value::as_str).unwrap_or("");
                self.thinking_done(output, summary, text, &mut out)?;
            }
            "response.function_call_arguments.delta" => {
                let delta = event
                    .get("delta")
                    .and_then(Value::as_str)
                    .ok_or_else(|| ProxyError::protocol("Missing tool argument delta"))?;
                self.reserve(delta.len())?;
                let state = self
                    .tools
                    .get_mut(&output)
                    .ok_or_else(|| ProxyError::protocol("Tool delta preceded its call"))?;
                if state.done {
                    return Err(ProxyError::protocol(
                        "Tool delta arrived after its block stopped",
                    ));
                }
                state.had_delta = true;
                state.args.push_str(delta);
                out.push(GenerationEvent::ToolArgumentsDelta {
                    index: state.index,
                    json: delta.into(),
                });
            }
            "response.function_call_arguments.done" => self.tool_done(
                output,
                event.get("arguments").and_then(Value::as_str),
                &mut out,
            )?,
            "response.completed" | "response.incomplete" => {
                let response = event.get("response").unwrap_or(event);
                if let Some(items) = response.get("output").and_then(Value::as_array) {
                    for (index, item) in items.iter().enumerate() {
                        self.item(index, item, true, &mut out)?;
                    }
                }
                for output in self.tools.keys().copied().collect::<Vec<_>>() {
                    self.tool_done(output, None, &mut out)?;
                }
                for output in self.thinking.keys().copied().collect::<Vec<_>>() {
                    self.item(output, &json!({"type":"reasoning"}), true, &mut out)?;
                }
                self.start(&mut out);
                for search_index in 0..self.searches.len() {
                    self.reserve(
                        self.citation_bytes
                            .saturating_add(self.searches[search_index].len()),
                    )?;
                    let id = self.searches[search_index].clone();
                    let index = self.block(
                        Content::HostedResult {
                            id,
                            results: json!(self.citations),
                        },
                        &mut out,
                    )?;
                    self.close(index, &mut out);
                }
                for index in self.open.clone() {
                    self.close(index, &mut out);
                }
                let usage = response.get("usage").unwrap_or(&Value::Null);
                let usage = Usage {
                    input: usage
                        .get("input_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(self.estimated_input),
                    output: usage
                        .get("output_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                    cached: usage
                        .pointer("/input_tokens_details/cached_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                    reasoning: usage
                        .pointer("/output_tokens_details/reasoning_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                };
                let reason = if response
                    .pointer("/incomplete_details/reason")
                    .and_then(Value::as_str)
                    == Some("max_output_tokens")
                {
                    StopReason::MaxTokens
                } else if !self.tools.is_empty() {
                    StopReason::ToolUse
                } else {
                    StopReason::EndTurn
                };
                self.finished = true;
                out.push(GenerationEvent::Finish { usage, reason });
            }
            _ => {}
        }
        Ok(out)
    }
}
