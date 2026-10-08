//! LLM provider transport: request builders, SSE decoding and usage accounting
//! for the three protocols the app supports (`mobile/src/pi-agent.mjs` sends
//! every request through `providerStream`; spec §14.3 fixes the budgets).
//!
//! Everything that does not touch the network is a pure function of its inputs
//! so the exact JSON of each request is asserted byte-for-byte in tests: the
//! protocol shape is a contract with third-party gateways and a silent drift
//! (a renamed field, a dropped `stream_options`) would only show up as a
//! confusing failure on a user's endpoint.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::{json, Value};

use super::context::{Message, Role, ToolCall};
use super::tools::ToolDef;
use super::AgentConfig;
use super::Provider;

/// Provider request timeout (spec §14.3). It covers the request up to the
/// response headers; the body stream is bounded by [`PANEL_TIMER_MS`].
pub const PROVIDER_TIMEOUT_MS: u64 = 60_000;
/// `const timer = setTimeout(() => stop("stopped"), 900000)` (spec §5.4).
pub const PANEL_TIMER_MS: u64 = 900_000;
/// `maxRetries: 0` (spec §14.3) — a retry is a new user-visible request.
pub const MAX_RETRIES: u32 = 0;
/// `thinkingBudgets` of `pi-agent.mjs`; `MIN_ANSWER_TOKENS` keeps room for the
/// answer next to the reasoning.
pub const THINKING_BUDGETS: &[(&str, u64)] = &[
    ("minimal", 1024),
    ("low", 2048),
    ("medium", 8192),
    ("high", 16384),
];
pub const MIN_ANSWER_TOKENS: u64 = 1024;
pub const ANTHROPIC_VERSION: &str = "2023-06-01";

/// One outbound provider request: URL, headers and JSON body.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderRequest {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Value,
}

/// Token accounting as the panel renders it (spec §5.3 `usage`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub total: u64,
}

impl Usage {
    fn merge(&mut self, next: Usage) {
        // Google repeats the full usage on the last chunk; Anthropic splits it
        // across message_start and message_delta. Additive would double count.
        self.input = self.input.max(next.input);
        self.output = self.output.max(next.output);
        self.cache_read = self.cache_read.max(next.cache_read);
        self.total = self.total.max(next.total);
    }
}

/// One normalized streaming event, whatever the wire protocol was.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamDelta {
    Text(String),
    /// A tool call that is complete and ready to run.
    ToolCall(ToolCall),
    Usage(Usage),
    Stop(String),
    Error(String),
}

// ---------------------------------------------------------------------------
// Request builders
// ---------------------------------------------------------------------------

/// Build the outbound request for `provider`. `messages` carries the system
/// prompt as its leading `system` message, exactly like the JS runtime keeps
/// the baseline in the transcript.
pub fn build_request(
    config: &AgentConfig,
    messages: &[Message],
    tools: &[ToolDef],
) -> ProviderRequest {
    match config.provider {
        Provider::AnthropicMessages => anthropic_request(config, messages, tools),
        Provider::GoogleGemini => google_request(config, messages, tools),
        Provider::OpenAiCompat => openai_request(config, messages, tools),
    }
}

/// `POST {endpoint}/chat/completions`.
fn openai_request(
    config: &AgentConfig,
    messages: &[Message],
    tools: &[ToolDef],
) -> ProviderRequest {
    let has_tool_history = messages
        .iter()
        .any(|m| !m.tool_calls.is_empty() || m.role == Role::ToolResult);
    let mut body = json!({
        "model": config.model,
        "messages": openai_messages(messages),
        "stream": true,
        "stream_options": { "include_usage": true },
        "max_tokens": config.max_tokens,
    });
    if !tools.is_empty() {
        body["tools"] = json!(tools
            .iter()
            .map(|tool| json!({
                "type": "function",
                "function": {
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.schema,
                },
            }))
            .collect::<Vec<_>>());
    } else if has_tool_history {
        // An empty array tells a gateway that tool results may follow even
        // when this turn declares no tools (pi-ai sends `tools: []` there).
        body["tools"] = json!([]);
    }
    if config.reasoning != "off" && !config.reasoning.is_empty() {
        body["reasoning_effort"] = json!(config.reasoning);
    }
    ProviderRequest {
        url: format!("{}/chat/completions", config.endpoint),
        headers: provider_headers(
            config,
            vec![
                ("content-type".into(), "application/json".into()),
                (
                    "authorization".into(),
                    format!("Bearer {}", config.api_key.as_deref().unwrap_or("keyless")),
                ),
            ],
        ),
        body,
    }
}

fn openai_messages(messages: &[Message]) -> Vec<Value> {
    messages
        .iter()
        .map(|message| match message.role {
            Role::System => json!({ "role": "system", "content": message.text }),
            Role::User => json!({ "role": "user", "content": message.text }),
            Role::Assistant => {
                let content = if message.text.is_empty() {
                    Value::Null
                } else {
                    json!(message.text)
                };
                let mut out = json!({ "role": "assistant", "content": content });
                if !message.tool_calls.is_empty() {
                    out["tool_calls"] = json!(message
                        .tool_calls
                        .iter()
                        .map(|call| json!({
                            "id": call.id,
                            "type": "function",
                            "function": { "name": call.name, "arguments": call.arguments },
                        }))
                        .collect::<Vec<_>>());
                }
                out
            }
            Role::ToolResult => json!({
                "role": "tool",
                "tool_call_id": message.tool_call_id.clone().unwrap_or_default(),
                "content": message.text,
            }),
        })
        .collect()
}

/// `POST {endpoint}/v1/messages` (or `{endpoint}/messages` when the endpoint
/// already carries the `/v1` prefix, the way the JS hint suggests).
fn anthropic_request(
    config: &AgentConfig,
    messages: &[Message],
    tools: &[ToolDef],
) -> ProviderRequest {
    let system: Vec<&str> = messages
        .iter()
        .filter(|m| m.role == Role::System)
        .map(|m| m.text.as_str())
        .filter(|text| !text.is_empty())
        .collect();
    let mut body = json!({
        "model": config.model,
        "max_tokens": config.max_tokens,
        "messages": anthropic_messages(messages),
        "stream": true,
    });
    if !system.is_empty() {
        body["system"] = json!(system
            .iter()
            .map(|text| json!({ "type": "text", "text": text }))
            .collect::<Vec<_>>());
    }
    if !tools.is_empty() {
        body["tools"] = json!(tools
            .iter()
            .map(|tool| json!({
                "name": tool.name,
                "description": tool.description,
                "input_schema": tool.schema,
            }))
            .collect::<Vec<_>>());
    }
    if config.reasoning != "off" && !config.reasoning.is_empty() {
        if let Some(budget) = thinking_budget(&config.reasoning) {
            body["thinking"] = json!({ "type": "enabled", "budget_tokens": budget });
        }
    }
    ProviderRequest {
        url: anthropic_url(&config.endpoint),
        headers: provider_headers(
            config,
            vec![
                ("content-type".into(), "application/json".into()),
                ("accept".into(), "application/json".into()),
                (
                    "x-api-key".into(),
                    config.api_key.clone().unwrap_or_else(|| "keyless".into()),
                ),
                ("anthropic-version".into(), ANTHROPIC_VERSION.into()),
                (
                    "anthropic-dangerous-direct-browser-access".into(),
                    "true".into(),
                ),
            ],
        ),
        body,
    }
}

fn anthropic_url(endpoint: &str) -> String {
    let trimmed = endpoint.trim_end_matches('/');
    if trimmed.ends_with("/v1") {
        format!("{trimmed}/messages")
    } else {
        format!("{trimmed}/v1/messages")
    }
}

/// Anthropic tool ids must match `[A-Za-z0-9_-]{1,64}`; the model may return
/// ids with other characters, so both sides are normalized identically.
pub fn anthropic_tool_id(id: &str) -> String {
    let mut out: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    out.truncate(64);
    out
}

fn thinking_budget(level: &str) -> Option<u64> {
    THINKING_BUDGETS
        .iter()
        .find(|(name, _)| *name == level)
        .map(|(_, budget)| *budget)
        .filter(|budget| *budget >= MIN_ANSWER_TOKENS)
}

fn anthropic_messages(messages: &[Message]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    // Tool results are grouped into a single user message: Anthropic rejects a
    // user message whose content starts with a text block after tool_use.
    let mut pending_results: Vec<Value> = Vec::new();
    let flush = |out: &mut Vec<Value>, pending: &mut Vec<Value>| {
        if !pending.is_empty() {
            let blocks = std::mem::take(pending);
            out.push(json!({ "role": "user", "content": blocks }));
        }
    };
    for message in messages {
        match message.role {
            Role::System => continue,
            Role::ToolResult => {
                let mut block = json!({
                    "type": "tool_result",
                    "tool_use_id": anthropic_tool_id(message.tool_call_id.as_deref().unwrap_or("")),
                    "content": message.text,
                });
                if message.is_error {
                    block["is_error"] = json!(true);
                }
                pending_results.push(block);
            }
            Role::User => {
                flush(&mut out, &mut pending_results);
                out.push(json!({
                    "role": "user",
                    "content": [{ "type": "text", "text": message.text }],
                }));
            }
            Role::Assistant => {
                flush(&mut out, &mut pending_results);
                let mut blocks: Vec<Value> = Vec::new();
                if !message.text.is_empty() {
                    blocks.push(json!({ "type": "text", "text": message.text }));
                }
                for call in &message.tool_calls {
                    blocks.push(json!({
                        "type": "tool_use",
                        "id": anthropic_tool_id(&call.id),
                        "name": call.name,
                        "input": parse_arguments(&call.arguments),
                    }));
                }
                if blocks.is_empty() {
                    blocks.push(json!({ "type": "text", "text": "" }));
                }
                out.push(json!({ "role": "assistant", "content": blocks }));
            }
        }
    }
    flush(&mut out, &mut pending_results);
    out
}

/// `POST {endpoint}/models/{model}:streamGenerateContent?alt=sse`.
fn google_request(
    config: &AgentConfig,
    messages: &[Message],
    tools: &[ToolDef],
) -> ProviderRequest {
    let system: Vec<&str> = messages
        .iter()
        .filter(|m| m.role == Role::System)
        .map(|m| m.text.as_str())
        .filter(|text| !text.is_empty())
        .collect();
    let mut body = json!({
        "contents": google_contents(messages),
        "generationConfig": { "maxOutputTokens": config.max_tokens },
    });
    if !system.is_empty() {
        body["systemInstruction"] = json!({
            "parts": system.iter().map(|text| json!({ "text": text })).collect::<Vec<_>>(),
        });
    }
    if !tools.is_empty() {
        body["tools"] = json!([{
            "functionDeclarations": tools.iter().map(|tool| json!({
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.schema,
            })).collect::<Vec<_>>(),
        }]);
    }
    ProviderRequest {
        url: format!(
            "{}/models/{}:streamGenerateContent?alt=sse",
            config.endpoint.trim_end_matches('/'),
            config.model
        ),
        headers: provider_headers(
            config,
            vec![
                ("content-type".into(), "application/json".into()),
                (
                    "x-goog-api-key".into(),
                    config.api_key.clone().unwrap_or_else(|| "keyless".into()),
                ),
            ],
        ),
        body,
    }
}

fn google_contents(messages: &[Message]) -> Vec<Value> {
    let mut contents: Vec<Value> = Vec::new();
    for message in messages {
        if message.role == Role::System {
            continue;
        }
        let role = match message.role {
            Role::Assistant => "model",
            _ => "user",
        };
        let mut parts: Vec<Value> = Vec::new();
        // A tool result carries the `functionResponse` part only; the JS
        // adapter never repeats the raw text next to it.
        if message.role == Role::ToolResult {
            let name = message
                .tool_name
                .clone()
                .unwrap_or_else(|| message.tool_call_id.clone().unwrap_or_default());
            parts.push(json!({ "functionResponse": {
                "name": name,
                "response": google_response(&message.text, message.is_error),
            }}));
        } else {
            if !message.text.is_empty() {
                parts.push(json!({ "text": message.text }));
            }
            for call in &message.tool_calls {
                parts.push(json!({ "functionCall": {
                    "name": call.name,
                    "args": parse_arguments(&call.arguments),
                }}));
            }
        }
        if parts.is_empty() {
            continue;
        }
        // Google rejects alternating-free contents in some gateways: merge
        // consecutive same-role contents (tool results follow the model turn).
        match contents.last_mut() {
            Some(last) if last["role"] == json!(role) => {
                let existing = last["parts"].as_array().cloned().unwrap_or_default();
                last["parts"] = json!(existing.into_iter().chain(parts).collect::<Vec<_>>());
            }
            _ => contents.push(json!({ "role": role, "parts": parts })),
        }
    }
    contents
}

/// JS adapter: `response: isError ? {error: y} : {output: y}`, where `y` is the
/// tool result text verbatim. The Struct wraps the raw string; it is never
/// parsed back into JSON.
fn google_response(text: &str, is_error: bool) -> Value {
    if is_error {
        json!({ "error": text })
    } else {
        json!({ "output": text })
    }
}

fn parse_arguments(text: &str) -> Value {
    serde_json::from_str::<Value>(text).unwrap_or_else(|_| json!({}))
}

/// `content-type` / auth first, then the user's extra headers, which win on a
/// name collision (a gateway that needs its own `Authorization`).
fn provider_headers(
    config: &AgentConfig,
    mut headers: Vec<(String, String)>,
) -> Vec<(String, String)> {
    for (name, value) in &config.extra_headers {
        headers.retain(|(existing, _)| !existing.eq_ignore_ascii_case(name));
        headers.push((name.clone(), value.clone()));
    }
    headers
}

// ---------------------------------------------------------------------------
// SSE decoding
// ---------------------------------------------------------------------------

/// Incremental `text/event-stream` parser (WHATWG event stream semantics:
/// `event:`/`data:` fields, blank line dispatch, `:` comment lines).
#[derive(Default)]
pub struct SseDecoder {
    buf: Vec<u8>,
    event: Option<String>,
    data: Vec<String>,
}

impl SseDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one chunk of bytes; returns every event completed by it.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<(Option<String>, String)> {
        self.buf.extend_from_slice(chunk);
        let mut events = Vec::new();
        while let Some(pos) = self.buf.iter().position(|byte| *byte == b'\n') {
            let mut line: Vec<u8> = self.buf.drain(..=pos).collect();
            line.pop(); // '\n'
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if let Some(event) = self.feed_line(&String::from_utf8_lossy(&line)) {
                events.push(event);
            }
        }
        events
    }

    /// Flush a trailing event that was not terminated by a blank line.
    pub fn finish(&mut self) -> Option<(Option<String>, String)> {
        if !self.buf.is_empty() {
            let line = String::from_utf8_lossy(&self.buf).to_string();
            self.buf.clear();
            if let Some(event) = self.feed_line(&line) {
                return Some(event);
            }
        }
        self.take_event()
    }

    fn feed_line(&mut self, line: &str) -> Option<(Option<String>, String)> {
        if line.is_empty() {
            return self.take_event();
        }
        if line.starts_with(':') {
            return None;
        }
        let (field, value) = line.split_once(':')?;
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" => self.event = Some(value.to_string()),
            "data" => self.data.push(value.to_string()),
            _ => {}
        }
        None
    }

    fn take_event(&mut self) -> Option<(Option<String>, String)> {
        if self.data.is_empty() && self.event.is_none() {
            return None;
        }
        let event = self.event.take();
        let data = std::mem::take(&mut self.data).join("\n");
        Some((event, data))
    }
}

/// Per-provider streaming state: turns raw events into [`StreamDelta`]s and
/// assembles tool calls whose arguments arrive in fragments.
pub struct StreamAssembler {
    provider: Provider,
    /// OpenAI fragments tool calls by `index` across deltas.
    openai_calls: BTreeMap<u32, (String, String, String)>,
    /// The Anthropic `content_block` currently being streamed.
    anthropic_block: Option<(String, String, String)>,
    usage: Usage,
    google_seq: u64,
    stopped: bool,
}

impl StreamAssembler {
    pub fn new(provider: Provider) -> Self {
        StreamAssembler {
            provider,
            openai_calls: BTreeMap::new(),
            anthropic_block: None,
            usage: Usage::default(),
            google_seq: 0,
            stopped: false,
        }
    }

    /// Decode one `(event, data)` pair.
    pub fn feed(&mut self, _event: Option<&str>, data: &str) -> Vec<StreamDelta> {
        if data.trim() == "[DONE]" {
            return self.finish();
        }
        let Ok(value) = serde_json::from_str::<Value>(data) else {
            return Vec::new();
        };
        if let Some(message) = value.get("error").and_then(Value::as_object) {
            let text = message
                .get("message")
                .or_else(|| message.get("msg"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| Value::Object(message.clone()).to_string());
            return vec![StreamDelta::Error(text)];
        }
        match self.provider {
            Provider::OpenAiCompat => self.feed_openai(&value),
            Provider::AnthropicMessages => self.feed_anthropic(&value),
            Provider::GoogleGemini => self.feed_google(&value),
        }
    }

    /// Called at the end of the body: flush a tool call the provider never
    /// marked finished and publish the usage if the stream carried none.
    pub fn finish(&mut self) -> Vec<StreamDelta> {
        let mut out = Vec::new();
        out.extend(self.flush_openai_calls());
        if let Some(block) = self.anthropic_block.take() {
            out.push(StreamDelta::ToolCall(ToolCall {
                id: anthropic_tool_id(&block.0),
                name: block.1,
                arguments: block.2,
            }));
        }
        if self.usage.total > 0 {
            out.push(StreamDelta::Usage(self.usage));
            // Publish the merged usage once, even when `finish()` runs again
            // (a `[DONE]` event and the end of the body both reach it).
            self.usage = Usage::default();
        }
        self.stopped = true;
        out
    }

    fn usage(&mut self, next: Usage) -> Option<StreamDelta> {
        self.usage.merge(next);
        Some(StreamDelta::Usage(self.usage))
    }

    fn feed_openai(&mut self, value: &Value) -> Vec<StreamDelta> {
        let mut out = Vec::new();
        if let Some(usage) = value.get("usage").filter(|u| !u.is_null()) {
            let cached = usage
                .pointer("/prompt_tokens_details/cached_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            if let Some(delta) = self.usage(Usage {
                input: usage
                    .get("prompt_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                output: usage
                    .get("completion_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                cache_read: cached,
                total: usage
                    .get("total_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            }) {
                out.push(delta);
            }
        }
        let Some(choice) = value.pointer("/choices/0") else {
            return out;
        };
        if let Some(text) = choice.pointer("/delta/content").and_then(Value::as_str) {
            if !text.is_empty() {
                out.push(StreamDelta::Text(text.to_string()));
            }
        }
        if let Some(calls) = choice
            .pointer("/delta/tool_calls")
            .and_then(Value::as_array)
        {
            for call in calls {
                let index = call.get("index").and_then(Value::as_u64).unwrap_or(0) as u32;
                let entry = self.openai_calls.entry(index).or_default();
                if let Some(id) = call.get("id").and_then(Value::as_str) {
                    entry.0 = id.to_string();
                }
                if let Some(name) = call.pointer("/function/name").and_then(Value::as_str) {
                    entry.1.push_str(name);
                }
                if let Some(args) = call.pointer("/function/arguments").and_then(Value::as_str) {
                    entry.2.push_str(args);
                }
            }
        }
        if let Some(finish) = choice.get("finish_reason").and_then(Value::as_str) {
            out.extend(self.flush_openai_calls());
            out.push(StreamDelta::Stop(finish.to_string()));
        }
        out
    }

    fn flush_openai_calls(&mut self) -> Vec<StreamDelta> {
        std::mem::take(&mut self.openai_calls)
            .into_values()
            .filter(|(id, name, _)| !id.is_empty() || !name.is_empty())
            .map(|(id, name, arguments)| {
                StreamDelta::ToolCall(ToolCall {
                    id,
                    name,
                    arguments,
                })
            })
            .collect()
    }

    fn feed_anthropic(&mut self, value: &Value) -> Vec<StreamDelta> {
        let mut out = Vec::new();
        match value.get("type").and_then(Value::as_str) {
            Some("message_start") => {
                // The usage lives under `message.usage`, not at the top level.
                let usage = value.pointer("/message/usage").unwrap_or(&Value::Null);
                if let Some(delta) = self.usage(Usage {
                    input: usage
                        .get("input_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                    output: 0,
                    cache_read: usage
                        .get("cache_read_input_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                    total: usage
                        .get("input_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                }) {
                    out.push(delta);
                }
            }
            Some("content_block_start") => {
                if value.pointer("/content_block/type").and_then(Value::as_str) == Some("tool_use")
                {
                    let id = value
                        .pointer("/content_block/id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let name = value
                        .pointer("/content_block/name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    self.anthropic_block = Some((id, name, String::new()));
                }
            }
            Some("content_block_delta") => {
                match value.pointer("/delta/type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        if let Some(text) = value.pointer("/delta/text").and_then(Value::as_str) {
                            if !text.is_empty() {
                                out.push(StreamDelta::Text(text.to_string()));
                            }
                        }
                    }
                    Some("input_json_delta") => {
                        if let Some(partial) =
                            value.pointer("/delta/partial_json").and_then(Value::as_str)
                        {
                            if let Some(block) = self.anthropic_block.as_mut() {
                                block.2.push_str(partial);
                            }
                        }
                    }
                    _ => {}
                }
            }
            Some("content_block_stop") => {
                if let Some((id, name, arguments)) = self.anthropic_block.take() {
                    out.push(StreamDelta::ToolCall(ToolCall {
                        id: anthropic_tool_id(&id),
                        name,
                        arguments,
                    }));
                }
            }
            Some("message_delta") => {
                if let Some(reason) = value.pointer("/delta/stop_reason").and_then(Value::as_str) {
                    out.push(StreamDelta::Stop(reason.to_string()));
                }
                let usage = value.get("usage").unwrap_or(&Value::Null);
                let output = usage
                    .get("output_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                if output > 0 {
                    if let Some(delta) = self.usage(Usage {
                        input: 0,
                        output,
                        cache_read: 0,
                        total: output,
                    }) {
                        out.push(delta);
                    }
                }
            }
            Some("message_stop") => {
                if !self.stopped {
                    self.stopped = true;
                    out.push(StreamDelta::Stop("stop".into()));
                }
            }
            Some("error") => {
                let text = value
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("provider error")
                    .to_string();
                out.push(StreamDelta::Error(text));
            }
            _ => {}
        }
        out
    }

    fn feed_google(&mut self, value: &Value) -> Vec<StreamDelta> {
        let mut out = Vec::new();
        let mut usage_delta: Option<StreamDelta> = None;
        if let Some(meta) = value.get("usageMetadata") {
            let input = meta
                .get("promptTokenCount")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let output = meta
                .get("candidatesTokenCount")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            usage_delta = self.usage(Usage {
                input,
                output,
                cache_read: meta
                    .get("cachedContentTokenCount")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                total: meta
                    .get("totalTokenCount")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            });
        }
        // Parts first, then the usage, then the finish reason.
        if let Some(parts) = value
            .pointer("/candidates/0/content/parts")
            .and_then(Value::as_array)
        {
            for part in parts {
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    if !text.is_empty() {
                        out.push(StreamDelta::Text(text.to_string()));
                    }
                }
                if let Some(call) = part.get("functionCall") {
                    let name = call
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let args = call.get("args").cloned().unwrap_or_else(|| json!({}));
                    let ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis())
                        .unwrap_or(0);
                    self.google_seq += 1;
                    out.push(StreamDelta::ToolCall(ToolCall {
                        id: format!("{}_{}_{}", name, ms, self.google_seq),
                        name,
                        arguments: args.to_string(),
                    }));
                }
            }
        }
        if let Some(delta) = usage_delta {
            out.push(delta);
        }
        if let Some(reason) = value
            .pointer("/candidates/0/finishReason")
            .and_then(Value::as_str)
        {
            out.push(StreamDelta::Stop(reason.to_string()));
        }
        out
    }
}

/// Split a streamed body into events and deltas in one pass, for callers that
/// hold the raw bytes in one piece. The live path feeds the same two pieces
/// chunk by chunk as they arrive (`Runtime::stream`), so this is the tests'
/// way in — the HTTP reader itself never calls it.
#[cfg(test)]
pub fn decode_body(provider: Provider, body: &str) -> Vec<StreamDelta> {
    let mut decoder = SseDecoder::new();
    let mut assembler = StreamAssembler::new(provider);
    let mut out = Vec::new();
    for (event, data) in decoder.push(body.as_bytes()) {
        out.extend(assembler.feed(event.as_deref(), &data));
    }
    if let Some((event, data)) = decoder.finish() {
        out.extend(assembler.feed(event.as_deref(), &data));
    }
    out.extend(assembler.finish());
    out
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

/// One attempt: `maxRetries` is 0 (spec §14.3), so a failure is reported
/// instead of repeating an expensive request.
pub async fn send(request: &ProviderRequest) -> Result<reqwest::Response, String> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_millis(PROVIDER_TIMEOUT_MS))
        .build()
        .map_err(|error| error.to_string())?;
    let mut builder = client.post(&request.url);
    for (name, value) in &request.headers {
        builder = builder.header(name, value);
    }
    builder = builder.json(&request.body);
    match tokio::time::timeout(Duration::from_millis(PROVIDER_TIMEOUT_MS), builder.send()).await {
        Ok(Ok(response)) => {
            if response.status().is_success() {
                Ok(response)
            } else {
                let status = response.status().as_u16();
                let text = response.text().await.unwrap_or_default();
                Err(format!(
                    "Provider request failed ({status}): {}",
                    excerpt_error(&text)
                ))
            }
        }
        Ok(Err(error)) => Err(error.to_string()),
        Err(_) => Err(format!(
            "Provider request timed out after {PROVIDER_TIMEOUT_MS} ms."
        )),
    }
}

fn excerpt_error(text: &str) -> String {
    const LIMIT: usize = 400;
    let trimmed = text.trim();
    if trimmed.chars().count() <= LIMIT {
        return trimmed.to_string();
    }
    trimmed.chars().take(LIMIT).collect::<String>() + "…"
}

pub fn panel_timer() -> Duration {
    Duration::from_millis(PANEL_TIMER_MS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::config::{AGENT_DEFAULT_CONTEXT_WINDOW, AGENT_DEFAULT_MAX_TOKENS};

    fn config(provider: Provider) -> AgentConfig {
        AgentConfig {
            endpoint: "https://api.example.com/v1".into(),
            model: "test-model".into(),
            api_key: Some("secret".into()),
            provider,
            reasoning: "off".into(),
            extra_headers: Vec::new(),
            context_window: AGENT_DEFAULT_CONTEXT_WINDOW,
            max_tokens: AGENT_DEFAULT_MAX_TOKENS,
            price_input: 0.0,
            price_output: 0.0,
        }
    }

    fn history() -> Vec<Message> {
        vec![
            Message::system("SYSTEM PROMPT"),
            Message::user("hello"),
            Message::assistant(
                "thinking…",
                vec![ToolCall {
                    id: "call_1".into(),
                    name: "get_device_status".into(),
                    arguments: "{}".into(),
                }],
                "tool_calls",
            ),
            Message::tool_result("call_1", "get_device_status", r#"{"ok":true}"#, false),
        ]
    }

    #[test]
    fn openai_request_shape() {
        let request = openai_request(&config(Provider::OpenAiCompat), &history(), &base_tools());
        assert_eq!(request.url, "https://api.example.com/v1/chat/completions");
        assert!(request
            .headers
            .iter()
            .any(|(name, value)| name == "authorization" && value == "Bearer secret"));
        assert!(request
            .headers
            .iter()
            .any(|(name, value)| name == "content-type" && value == "application/json"));
        let body = &request.body;
        assert_eq!(body["model"], json!("test-model"));
        assert_eq!(body["stream"], json!(true));
        assert_eq!(body["stream_options"], json!({ "include_usage": true }));
        assert_eq!(body["max_tokens"], json!(AGENT_DEFAULT_MAX_TOKENS));
        assert_eq!(body["messages"][0]["role"], json!("system"));
        assert_eq!(body["messages"][0]["content"], json!("SYSTEM PROMPT"));
        assert_eq!(body["messages"][2]["content"], json!("thinking…"));
        assert_eq!(body["messages"][2]["tool_calls"][0]["id"], json!("call_1"));
        assert_eq!(
            body["messages"][2]["tool_calls"][0]["function"]["name"],
            json!("get_device_status")
        );
        assert_eq!(body["messages"][3]["role"], json!("tool"));
        assert_eq!(body["messages"][3]["tool_call_id"], json!("call_1"));
        assert_eq!(body["tools"][0]["type"], json!("function"));
        assert_eq!(
            body["tools"][0]["function"]["name"],
            json!("update_task_plan")
        );
        assert!(body["tools"][0]["function"]["parameters"]["properties"]
            .get("steps")
            .is_some());
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn openai_omits_tools_but_sends_empty_array_with_tool_history() {
        let config = config(Provider::OpenAiCompat);
        let messages = history();
        let no_tools = openai_request(&config, &messages, &[]);
        assert_eq!(no_tools.body["tools"], json!([]));
        assert_eq!(no_tools.body["tools"].as_array().unwrap().len(), 0);

        let plain = vec![Message::user("hi")];
        let fresh = openai_request(&config, &plain, &[]);
        assert!(fresh.body.get("tools").is_none());
    }

    #[test]
    fn openai_null_content_and_reasoning_effort() {
        let mut config = config(Provider::OpenAiCompat);
        config.reasoning = "high".into();
        let messages = vec![
            Message::system("S"),
            Message::user("q"),
            Message::assistant("", vec![], "stop"),
        ];
        let request = openai_request(&config, &messages, &[]);
        assert_eq!(request.body["messages"][2]["content"], Value::Null);
        assert_eq!(request.body["reasoning_effort"], json!("high"));
    }

    #[test]
    fn openai_extra_headers_override_defaults() {
        let mut config = config(Provider::OpenAiCompat);
        config
            .extra_headers
            .push(("Authorization".into(), "Bearer other".into()));
        let request = openai_request(&config, &history(), &[]);
        let auth: Vec<&str> = request
            .headers
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case("authorization"))
            .map(|(_, value)| value.as_str())
            .collect();
        assert_eq!(auth, vec!["Bearer other"]);
    }

    #[test]
    fn anthropic_request_shape() {
        let request = anthropic_request(
            &config(Provider::AnthropicMessages),
            &history(),
            &base_tools(),
        );
        assert_eq!(
            request.url,
            "https://api.example.com/v1/messages".replace("/v1/v1/", "/v1/")
        );
        assert_eq!(request.url, "https://api.example.com/v1/messages");
        let header = |name: &str| {
            request
                .headers
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        };
        assert_eq!(header("x-api-key").as_deref(), Some("secret"));
        assert_eq!(
            header("anthropic-version").as_deref(),
            Some(ANTHROPIC_VERSION)
        );
        assert_eq!(
            header("anthropic-dangerous-direct-browser-access").as_deref(),
            Some("true")
        );
        let body = &request.body;
        assert_eq!(body["model"], json!("test-model"));
        assert_eq!(body["stream"], json!(true));
        assert_eq!(body["system"][0]["type"], json!("text"));
        assert_eq!(body["system"][0]["text"], json!("SYSTEM PROMPT"));
        assert_eq!(body["messages"][0]["role"], json!("user"));
        assert_eq!(body["messages"][1]["role"], json!("assistant"));
        assert_eq!(body["messages"][1]["content"][1]["type"], json!("tool_use"));
        assert_eq!(body["messages"][1]["content"][1]["id"], json!("call_1"));
        // The tool result is grouped into its own user message.
        assert_eq!(body["messages"][2]["role"], json!("user"));
        assert_eq!(
            body["messages"][2]["content"][0]["type"],
            json!("tool_result")
        );
        assert_eq!(
            body["messages"][2]["content"][0]["tool_use_id"],
            json!("call_1")
        );
        assert_eq!(body["tools"][0]["name"], json!("update_task_plan"));
        assert!(body["tools"][0]["input_schema"]["properties"]
            .get("steps")
            .is_some());
        assert!(body.get("thinking").is_none());
    }

    #[test]
    fn anthropic_url_join_and_tool_id_normalization() {
        assert_eq!(
            anthropic_url("https://api.anthropic.com/v1"),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(
            anthropic_url("https://proxy.example/anthropic/"),
            "https://proxy.example/anthropic/v1/messages"
        );
        assert_eq!(anthropic_tool_id("a b/c"), "a_b_c");
        assert_eq!(anthropic_tool_id(&"x".repeat(80)).len(), 64);
        // `Jve()` of agent-runtime.js: `replace(/[^a-zA-Z0-9_-]/g, "_")` — a dot
        // is rewritten too, then the id is cut to 64 characters.
        assert_eq!(
            anthropic_tool_id("keep-odd_id.0123456789"),
            "keep-odd_id_0123456789"
        );
        assert_eq!(
            anthropic_tool_id("keep-odd_id_0123456789"),
            "keep-odd_id_0123456789"
        );
    }

    #[test]
    fn anthropic_thinking_budget_follows_level() {
        let mut config = config(Provider::AnthropicMessages);
        config.reasoning = "high".into();
        let request = anthropic_request(&config, &history(), &[]);
        assert_eq!(
            request.body["thinking"],
            json!({ "type": "enabled", "budget_tokens": 16384 })
        );
        config.reasoning = "medium".into();
        let request = anthropic_request(&config, &history(), &[]);
        assert_eq!(
            request.body["thinking"],
            json!({ "type": "enabled", "budget_tokens": 8192 })
        );
        config.reasoning = "off".into();
        let request = anthropic_request(&config, &history(), &[]);
        assert!(request.body.get("thinking").is_none());
    }

    #[test]
    fn google_request_shape() {
        let request = google_request(&config(Provider::GoogleGemini), &history(), &base_tools());
        assert_eq!(
            request.url,
            "https://api.example.com/v1/models/test-model:streamGenerateContent?alt=sse"
        );
        assert!(request
            .headers
            .iter()
            .any(|(name, value)| name == "x-goog-api-key" && value == "secret"));
        let body = &request.body;
        assert_eq!(
            body["generationConfig"]["maxOutputTokens"],
            json!(AGENT_DEFAULT_MAX_TOKENS)
        );
        assert_eq!(
            body["systemInstruction"]["parts"][0]["text"],
            json!("SYSTEM PROMPT")
        );
        assert_eq!(body["contents"][0]["role"], json!("user"));
        assert_eq!(body["contents"][1]["role"], json!("model"));
        assert_eq!(
            body["contents"][1]["parts"][1]["functionCall"]["name"],
            json!("get_device_status")
        );
        assert_eq!(body["contents"][2]["role"], json!("user"));
        assert_eq!(
            body["contents"][2]["parts"][0]["functionResponse"]["name"],
            json!("get_device_status")
        );
        // The response Struct wraps the raw tool result text, never a re-parse.
        assert_eq!(
            body["contents"][2]["parts"][0]["functionResponse"]["response"],
            json!({ "output": "{\"ok\":true}" })
        );
        assert_eq!(
            body["tools"][0]["functionDeclarations"][0]["name"],
            json!("update_task_plan")
        );
    }

    #[test]
    fn google_merges_consecutive_user_contents() {
        let messages = vec![
            Message::system("S"),
            Message::user("one"),
            Message::user("two"),
        ];
        let contents = google_contents(&messages);
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0]["parts"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn provider_headers_merge_extras_last() {
        let mut config = config(Provider::OpenAiCompat);
        config.extra_headers.push(("x-route".into(), "eu".into()));
        let headers = provider_headers(&config, vec![("content-type".into(), "a".into())]);
        assert_eq!(
            headers,
            vec![
                ("content-type".to_string(), "a".to_string()),
                ("x-route".to_string(), "eu".to_string())
            ]
        );
    }

    // --- SSE -------------------------------------------------------------

    #[test]
    fn sse_decoder_handles_chunks_events_and_comments() {
        let mut decoder = SseDecoder::new();
        let mut events = decoder.push(b": keep-alive\nevent: message_start\nda");
        assert!(events.is_empty());
        events.extend(decoder.push(b"ta: {\"a\":1}\n\n"));
        assert_eq!(
            events,
            vec![(Some("message_start".to_string()), "{\"a\":1}".to_string())]
        );
        events.extend(decoder.push(b"data: one\ndata: two\n\n"));
        assert_eq!(events[1].1, "one\ntwo");
        // A trailing event without the final blank line is flushed by finish().
        events.extend(decoder.push(b"data: tail"));
        let flushed = decoder.finish();
        assert_eq!(flushed, Some((None, "tail".to_string())));
    }

    #[test]
    fn openai_stream_decodes_text_tool_calls_and_usage() {
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",",
            "\"function\":{\"name\":\"read_serial_log\",\"arguments\":\"\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,",
            "\"function\":{\"arguments\":\"{\\\"limit\\\":10}\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: {\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":3,",
            "\"total_tokens\":15,\"prompt_tokens_details\":{\"cached_tokens\":4}}}\n\n",
            "data: [DONE]\n\n"
        );
        let deltas = decode_body(Provider::OpenAiCompat, body);
        assert_eq!(deltas[0], StreamDelta::Text("Hi".into()));
        assert_eq!(
            deltas[1],
            StreamDelta::ToolCall(ToolCall {
                id: "c1".into(),
                name: "read_serial_log".into(),
                arguments: "{\"limit\":10}".into(),
            })
        );
        assert_eq!(deltas[2], StreamDelta::Stop("tool_calls".into()));
        match &deltas[3] {
            StreamDelta::Usage(usage) => {
                assert_eq!(usage.input, 12);
                assert_eq!(usage.output, 3);
                assert_eq!(usage.cache_read, 4);
                assert_eq!(usage.total, 15);
            }
            other => panic!("expected usage, got {other:?}"),
        }
    }

    #[test]
    fn anthropic_stream_decodes_events() {
        let body = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":9,\"cache_read_input_tokens\":2}}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"content_block\":{\"type\":\"tool_use\",\"id\":\"tu_1\",\"name\":\"get_device_status\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"compact\\\"\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\":true}\"}}\n\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\"}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":7}}\n\n",
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
        );
        let deltas = decode_body(Provider::AnthropicMessages, body);
        assert_eq!(
            deltas[0],
            StreamDelta::Usage(Usage {
                input: 9,
                output: 0,
                cache_read: 2,
                total: 9
            })
        );
        assert_eq!(deltas[1], StreamDelta::Text("ok".into()));
        assert_eq!(
            deltas[2],
            StreamDelta::ToolCall(ToolCall {
                id: "tu_1".into(),
                name: "get_device_status".into(),
                arguments: "{\"compact\":true}".into(),
            })
        );
        assert_eq!(deltas[3], StreamDelta::Stop("tool_use".into()));
        match &deltas[4] {
            StreamDelta::Usage(usage) => {
                assert_eq!(usage.output, 7);
                assert_eq!(usage.cache_read, 2);
            }
            other => panic!("expected usage, got {other:?}"),
        }
        assert_eq!(deltas[5], StreamDelta::Stop("stop".into()));
    }

    #[test]
    fn google_stream_decodes_parts_usage_and_synthetic_ids() {
        let body = concat!(
            "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"hello\"}]},",
            "\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":5,",
            "\"candidatesTokenCount\":6,\"totalTokenCount\":11}}\n\n"
        );
        let deltas = decode_body(Provider::GoogleGemini, body);
        assert_eq!(deltas[0], StreamDelta::Text("hello".into()));
        assert!(matches!(&deltas[1], StreamDelta::Usage(usage)
            if usage.input == 5 && usage.output == 6 && usage.total == 11));
        assert_eq!(deltas[2], StreamDelta::Stop("STOP".into()));

        let call = "data: {\"candidates\":[{\"content\":{\"parts\":[{\"functionCall\":";
        let call = format!(
            "{}{{\"name\":\"probe_tools\",\"args\":{{}}}}}}]}}}}]}}\n\n",
            call
        );
        let deltas = decode_body(Provider::GoogleGemini, &call);
        match &deltas[0] {
            StreamDelta::ToolCall(tool) => {
                assert_eq!(tool.name, "probe_tools");
                assert!(tool.id.starts_with("probe_tools_"));
            }
            other => panic!("expected tool call, got {other:?}"),
        }
    }

    #[test]
    fn provider_errors_surface_as_error_deltas() {
        let openai = "data: {\"error\":{\"message\":\"bad key\"}}\n\n";
        assert_eq!(
            decode_body(Provider::OpenAiCompat, openai),
            vec![StreamDelta::Error("bad key".into())]
        );
        let anthropic =
            "event: error\ndata: {\"type\":\"error\",\"error\":{\"message\":\"overloaded\"}}\n\n";
        assert_eq!(
            decode_body(Provider::AnthropicMessages, anthropic),
            vec![StreamDelta::Error("overloaded".into())]
        );
    }

    #[test]
    fn budgets_follow_the_spec() {
        assert_eq!(PROVIDER_TIMEOUT_MS, 60_000);
        assert_eq!(PANEL_TIMER_MS, 900_000);
        assert_eq!(MAX_RETRIES, 0);
        assert_eq!(MIN_ANSWER_TOKENS, 1024);
        assert_eq!(
            THINKING_BUDGETS,
            &[
                ("minimal", 1024),
                ("low", 2048),
                ("medium", 8192),
                ("high", 16384)
            ]
        );
    }

    #[tokio::test]
    async fn http_round_trip_against_a_local_listener() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 8192];
            let mut request = Vec::new();
            loop {
                let read = socket.read(&mut buf).await.unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..read]);
                if let Some(pos) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&request[..pos]).to_string();
                    let content_length = head
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .unwrap_or(0);
                    while request.len() < pos + 4 + content_length {
                        let read = socket.read(&mut buf).await.unwrap();
                        if read == 0 {
                            break;
                        }
                        request.extend_from_slice(&buf[..read]);
                    }
                    break;
                }
            }
            let posted = String::from_utf8_lossy(&request[..]).to_string();
            let body = "data: {\"choices\":[{\"delta\":{\"content\":\"pong\"},\"finish_reason\":\"stop\"}]}\n\n";
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            socket.shutdown().await.ok();
            posted
        });

        let mut config = config(Provider::OpenAiCompat);
        config.endpoint = format!("http://{addr}/v1");
        let request = openai_request(&config, &[Message::user("ping")], &[]);
        let response = send(&request).await.expect("response");
        let mut decoder = SseDecoder::new();
        let mut assembler = StreamAssembler::new(Provider::OpenAiCompat);
        let mut out = Vec::new();
        let mut stream = response.bytes_stream();
        use futures::StreamExt;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.expect("chunk");
            for (event, data) in decoder.push(&chunk) {
                out.extend(assembler.feed(event.as_deref(), &data));
            }
        }
        out.extend(assembler.finish());
        assert_eq!(out[0], StreamDelta::Text("pong".into()));
        assert_eq!(out[1], StreamDelta::Stop("stop".into()));

        let posted = server.await.unwrap();
        assert!(posted.contains("POST /v1/chat/completions"));
        assert!(posted.contains("\"model\":\"test-model\""));
    }

    use crate::agent::tools::base_tools;
}
