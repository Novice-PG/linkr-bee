//! The LLM transport — the one part of the agent this crate did not carry.
//!
//! Until now a prompt was a thing the view recorded and nothing ever answered.
//! This is the answer: one OpenAI-compatible `/chat/completions` round trip
//! over `reqwest` with rustls, the protocol every provider in
//! [`crate::agent::config`] already lists — OpenAI, DeepSeek, Ollama, vLLM and
//! most gateways.
//!
//! # Why it looks like a transport
//!
//! The three links this crate already has — BLE, LAN, and the board's own
//! serial port — each build their own multi-thread Tokio runtime on a thread of
//! their own and hand bytes back to the interface over a channel, rather than
//! making the whole program async. That is not an accident of history; it is
//! why a frame is a pure function and the event loop can be read top to bottom.
//! This transport is the fourth of the same shape, and `chat()` is synchronous
//! from the caller's side because the caller is a synchronous TUI.
//!
//! The dependency is the one honest cost. `reqwest` with `rustls` pulls
//! `aws-lc-rs`, which builds a C library; the crate was zero-dependency for
//! good reasons and now is not. What the alternative buys is a transport that
//! can only speak plain `http://`, which for a real provider means the API key
//! crosses the network in the clear — so the dependency was the cheaper of the
//! two.

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use crate::agent::config::AgentConfig;
use crate::agent::usage::Usage;

/// One message in a conversation. The role is a fixed set because the API has
/// exactly three, and a `String` role would let a caller invent a fourth the
/// provider rejects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: String,
}

/// Who said it, in the API's own words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatRole {
    System,
    User,
    Assistant,
}

impl ChatRole {
    /// The spelling the wire uses.
    pub fn as_str(self) -> &'static str {
        match self {
            ChatRole::System => "system",
            ChatRole::User => "user",
            ChatRole::Assistant => "assistant",
        }
    }
}

/// What came back, when it came back at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    /// The reply text, with the provider's escapes resolved.
    pub text: String,
    /// Token accounting, when the provider reported it.
    pub usage: Usage,
}

/// Why there is no completion.
///
/// Two of these carry provider text, and both are redacted on the way out: a
/// provider that echoes the request back into its error body would otherwise
/// put the API key in the log, and a log is where credentials end up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The request was made and nothing answered: DNS, TCP, TLS, or a stall.
    Unreachable(String),
    /// The provider answered and refused, with its own words.
    Rejected(u16, String),
    /// The provider answered 200 but the body held no completion.
    Malformed,
}

impl Failure {
    /// One sentence for the interface, with the provider's own wording kept when
    /// it has any. Not `Display`: this is classified text and a `Display` impl
    /// would let a caller print one as though it were a log line.
    pub fn describe(&self) -> String {
        match self {
            Failure::Unreachable(why) => format!("the model did not answer: {why}"),
            Failure::Rejected(status, body) => {
                format!("the provider refused ({status}): {body}")
            }
            Failure::Malformed => {
                "the provider answered but sent no completion".to_string()
            }
        }
    }
}

/// A conversation sent to the model, and where its answer goes.
struct Job {
    config: AgentConfig,
    messages: Vec<ChatMessage>,
    reply: Option<Sender<(String, Result<Completion, Failure>)>>,
    /// The task this answers, so a reply can be matched to the goal it belongs
    /// to when it arrives out of order.
    task_id: String,
}

/// The transport. One of these lives in [`crate::app::App`] and is the only
/// thing that talks to a provider.
pub struct AgentClient {
    jobs: Sender<Job>,
}

/// `App` derives `Debug`, so this is hand-written: a transport's job shows a
/// channel and nothing else anyone would want to read, and a derive would then
/// tie the field to a `Sender`'s own `Debug`, which is not stable.
impl std::fmt::Debug for AgentClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AgentClient(1 job channel)")
    }
}

impl Default for AgentClient {
    fn default() -> Self {
        AgentClient::spawn()
    }
}

impl AgentClient {
    /// Start the transport's own thread, with its own runtime.
    ///
    /// A runtime per transport rather than one shared with the BLE link, because
    /// two reactors on one process is what the existing transports do and a
    /// shared one would couple an LLM stall to a serial link dropping.
    pub fn spawn() -> AgentClient {
        let (tx, rx) = mpsc::channel::<Job>();
        thread::spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                // No runtime means no transport, and the thread has nowhere to
                // report it: every pending job's reply channel is simply never
                // answered, which the caller's timeout turns into an
                // `Unreachable` rather than a hang.
                Err(_) => {
                    for job in rx.iter() {
                        drop(job);
                    }
                    return;
                }
            };
            while let Ok(mut job) = rx.recv() {
                let outcome = runtime.block_on(send_once(&job));
                if let Some(reply) = job.reply.take() {
                    let _ = reply.send((job.task_id, outcome));
                }
            }
        });
        AgentClient { jobs: tx }
    }

    /// Ask the model a question. Blocks until the answer arrives or the timeout
    /// fires — the caller is a synchronous interface, and a frame that waited
    /// for a channel would be a frame waiting on a network.
    ///
    /// `task_id` is carried through so the view can file the reply against the
    /// goal it belongs to, which matters because two prompts in flight at once
    /// can answer out of order.
    pub fn chat(
        &self,
        task_id: &str,
        config: AgentConfig,
        messages: Vec<ChatMessage>,
        timeout: Duration,
    ) -> Result<Completion, Failure> {
        let (tx, rx) = mpsc::channel();
        let job = Job {
            config,
            messages,
            reply: Some(tx),
            task_id: task_id.to_string(),
        };
        if self.jobs.send(job).is_err() {
            return Err(Failure::Unreachable("the transport stopped".to_string()));
        }
        // A dropped job would leave this blocking for ever, so the timeout is
        // the real bound and the worker's own request timeout is a second one
        // inside it. The task id comes back with the answer and is dropped
        // here, because the caller named the task it asked about.
        rx.recv_timeout(timeout)
            .map(|(_, outcome)| outcome)
            .map_err(|_| Failure::Unreachable("the model took too long".to_string()))?
    }

    /// Ask without waiting: the worker answers into a channel the interface
    /// drains on its own beat, which is what keeps a slow model from freezing
    /// the terminal.
    ///
    /// Returns the reply channel, or `None` when the transport is not running.
    pub fn submit(
        &self,
        task_id: &str,
        config: AgentConfig,
        messages: Vec<ChatMessage>,
    ) -> Option<Receiver<(String, Result<Completion, Failure>)>> {
        let (tx, rx) = mpsc::channel();
        let job = Job {
            config,
            messages,
            reply: Some(tx),
            task_id: task_id.to_string(),
        };
        self.jobs.send(job).ok()?;
        Some(rx)
    }
}

/// One round trip, on the transport's runtime.
async fn send_once(job: &Job) -> Result<Completion, Failure> {
    let key = job.config.api_key.clone();
    let url = chat_url(&job.config.endpoint);
    let body = request_body(&job.config, &job.messages);

    // A client per call rather than one shared: the timeout is part of the
    // config's own `max_tokens`, and a client built once would have to guess at
    // the first call's budget. The connection pool is the cost, and an
    // interactive assistant makes one call per question.
    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|why| Failure::Unreachable(redact(&why.to_string(), &key)))?;

    let mut request = client
        .post(&url)
        .header("content-type", "application/json")
        .body(body);
    if !key.is_empty() {
        request = request.header("authorization", format!("Bearer {key}"));
    }
    for (name, value) in &job.config.headers {
        // The validator in `agent::config` already rejects a header name that
        // is not `[A-Za-z0-9-]{1,64}`, so this cannot fail on a name a provider
        // would refuse — and a value containing a control byte was refused
        // there too.
        request = request.header(name.as_str(), value.as_str());
    }

    let response = request
        .send()
        .await
        .map_err(|why| Failure::Unreachable(redact(&why.to_string(), &key)))?;
    let status = response.status().as_u16();
    let text = response
        .text()
        .await
        .map_err(|why| Failure::Unreachable(redact(&why.to_string(), &key)))?;

    if status != 200 {
        return Err(Failure::Rejected(status, redact(&text, &key)));
    }
    parse_completion(&text).ok_or(Failure::Malformed)
}

/// The chat-completions URL. A trailing slash is stripped rather than
/// appended to, because `…/v1//chat/completions` is a 404 on some gateways and
/// `…/v1/chat/completions` is right on all of them.
fn chat_url(endpoint: &str) -> String {
    format!(
        "{}/chat/completions",
        endpoint.trim_end_matches('/').trim()
    )
}

/// The request body, written by hand.
///
/// No `serde_json`: the request has three fields, and a dependency that
/// serialises a struct with `#[derive]` to save eight lines of `format!` is a
/// dependency that then has to be taught about the response too. The endpoint,
/// the model and the messages are the whole of what a provider needs.
fn request_body(config: &AgentConfig, messages: &[ChatMessage]) -> String {
    let mut out = String::from("{\"model\":");
    out.push_str(&crate::agent::tasks::json_string(&config.model));
    out.push_str(",\"messages\":[");
    for (i, message) in messages.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str("{\"role\":");
        out.push_str(&crate::agent::tasks::json_string(message.role.as_str()));
        out.push_str(",\"content\":");
        out.push_str(&crate::agent::tasks::json_string(&message.content));
        out.push('}');
    }
    out.push_str("],\"max_tokens\":");
    out.push_str(&config.max_tokens.to_string());
    out.push('}');
    out
}

// ---------------------------------------------------------------------------
// Reading the reply
// ---------------------------------------------------------------------------

/// Pull the completion out of a 200 response.
///
/// The reader walks the object rather than deserialising one, because that is
/// what the crate already does for its own stored JSON and a second parser for
/// the same format in the same binary is a second place for the bugs to live.
/// It is deliberately tolerant of what the response adds and strict about what
/// the completion needs: an unknown field is skipped, a missing one is the end
/// of the read.
pub fn parse_completion(text: &str) -> Option<Completion> {
    let choices = field(text, "choices")?;
    let first = first_element(choices)?;
    let message = field(first, "message")?;
    let content = field(message, "content")?;
    let text_out = unquote(content)?;

    let usage = field(text, "usage").map(parse_usage).unwrap_or_default();
    Some(Completion {
        text: text_out,
        usage,
    })
}

/// The token accounting, when the provider reported it. A missing or
/// non-numeric field reads as zero rather than as an error: a provider that
/// reports usage for some models and not others must not make the reply
/// unusable.
fn parse_usage(object: &str) -> Usage {
    let number = |key: &str| field(object, key).and_then(|raw| raw.trim().parse().ok()).unwrap_or(0);
    Usage {
        input: number("prompt_tokens"),
        output: number("completion_tokens"),
        total_tokens: number("total_tokens"),
        ..Usage::default()
    }
}

/// The value of `key` in a flat JSON object, as a raw span.
///
/// Walks the object rather than searching for `"key":`, because a value that
/// *contains* `"key":` — a system prompt quoting the wire format, a file whose
/// contents are JSON — would otherwise match itself.
fn field<'a>(object: &'a str, key: &str) -> Option<&'a str> {
    let inner = object.trim().strip_prefix('{')?.strip_suffix('}')?;
    let bytes = inner.as_bytes();
    let mut i = skip_ws(bytes, 0);
    while i < bytes.len() {
        if bytes[i] != b'"' {
            return None;
        }
        let (key_span, after_key) = value_span(inner, i)?;
        if unquote(key_span)?.as_str() != key {
            i = skip_ws(bytes, after_key);
            if bytes.get(i) != Some(&b':') {
                return None;
            }
            // Skip this value and move to the next pair.
            let (_, after_value) = value_span(inner, skip_ws(bytes, i + 1))?;
            i = skip_ws(bytes, after_value);
            if bytes.get(i) == Some(&b',') {
                i = skip_ws(bytes, i + 1);
                continue;
            }
            return None;
        }
        i = skip_ws(bytes, after_key);
        if bytes.get(i) != Some(&b':') {
            return None;
        }
        let (value, _) = value_span(inner, skip_ws(bytes, i + 1))?;
        return Some(value);
    }
    None
}

/// The first element of a JSON array, as a raw span.
fn first_element(array: &str) -> Option<&str> {
    let inner = array.trim().strip_prefix('[')?.strip_suffix(']')?;
    let bytes = inner.as_bytes();
    let at = skip_ws(bytes, 0);
    if at >= bytes.len() {
        return None;
    }
    value_span(inner, at).map(|(span, _)| span)
}

/// The span of the JSON value starting at `at`, and the index after it.
///
/// The one place in this file that understands JSON's grammar, so the readers
/// above can be about the fields they want.
fn value_span(s: &str, at: usize) -> Option<(&str, usize)> {
    let bytes = s.as_bytes();
    if at >= bytes.len() {
        return None;
    }
    match bytes[at] {
        b'"' => {
            let mut i = at + 1;
            while i < bytes.len() {
                match bytes[i] {
                    b'\\' => i += 2,
                    b'"' => return Some((&s[at..i + 1], i + 1)),
                    _ => i += 1,
                }
            }
            None
        }
        b'{' | b'[' => {
            let mut depth = 0usize;
            let mut in_string = false;
            let mut i = at;
            while i < bytes.len() {
                let byte = bytes[i];
                if in_string {
                    match byte {
                        b'\\' => i += 2,
                        b'"' => {
                            in_string = false;
                            i += 1;
                        }
                        _ => i += 1,
                    }
                    continue;
                }
                match byte {
                    b'"' => {
                        in_string = true;
                        i += 1;
                    }
                    b'{' | b'[' => {
                        depth += 1;
                        i += 1;
                    }
                    b'}' | b']' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some((&s[at..i + 1], i + 1));
                        }
                        i += 1;
                    }
                    _ => i += 1,
                }
            }
            None
        }
        _ => {
            // A number, `true`, `false` or `null`: everything up to what ends it.
            let mut i = at;
            while i < bytes.len()
                && !matches!(bytes[i], b',' | b'}' | b']')
                && !bytes[i].is_ascii_whitespace()
            {
                i += 1;
            }
            (i > at).then(|| (&s[at..i], i))
        }
    }
}

fn skip_ws(bytes: &[u8], mut at: usize) -> usize {
    while at < bytes.len() && bytes[at].is_ascii_whitespace() {
        at += 1;
    }
    at
}

/// Resolve a JSON string literal into text, `\uXXXX` and all.
///
/// `agent::tasks::parse_json_string_literal` gets close but leaves an unknown
/// escape as `\` plus its letter, which turns a provider's `\u003c` into six
/// characters of soup in the middle of a reply. The escape set is small and
/// closed, so it is written out rather than extended by trial.
fn unquote(literal: &str) -> Option<String> {
    let literal = literal.trim();
    let body = literal.strip_prefix('"')?.strip_suffix('"')?;
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next()? {
            '"' => out.push('"'),
            '\\' => out.push('\\'),
            '/' => out.push('/'),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            'b' => out.push('\u{8}'),
            'f' => out.push('\u{c}'),
            'u' => {
                let mut code = 0u32;
                for _ in 0..4 {
                    let digit = chars.next()?.to_digit(16)?;
                    code = code * 16 + digit;
                }
                // A surrogate pair arrives as two escapes and is one character;
                // an unpaired one is not a character at all.
                if (0xd800..0xdc00).contains(&code) {
                    if chars.next() != Some('\\') || chars.next() != Some('u') {
                        out.push('\u{fffd}');
                        continue;
                    }
                    let mut low = 0u32;
                    for _ in 0..4 {
                        low = low * 16 + chars.next()?.to_digit(16)?;
                    }
                    match char::from_u32(0x10000 + ((code - 0xd800) << 10) + (low - 0xdc00)) {
                        Some(paired) => out.push(paired),
                        None => out.push('\u{fffd}'),
                    }
                } else {
                    out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                }
            }
            other => {
                out.push('\\');
                out.push(other);
            }
        }
    }
    Some(out)
}

/// Remove a secret from anything that is about to be printed.
///
/// Provider error bodies quote the request, and the request carries the key in
/// an `authorization` header. A transport that logged its own errors verbatim
/// would be a transport that puts a credential in the interface's log.
fn redact(text: &str, key: &str) -> String {
    if key.is_empty() {
        return text.to_string();
    }
    text.replace(key, "[redacted]")
}

/// The per-request ceiling.
///
/// Generous enough for a model thinking over a long system prompt, bounded
/// enough that the interface cannot be left waiting. The caller's own timeout
/// is the one the user feels; this one is what actually closes the socket.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

#[cfg(test)]
mod tests {
    use super::*;

    fn config(endpoint: &str) -> AgentConfig {
        AgentConfig {
            endpoint: endpoint.to_string(),
            model: "m".to_string(),
            api_key: String::new(),
            headers: Vec::new(),
            provider: "openai-completions",
            reasoning: "none",
            context_window: 8192,
            max_tokens: 1024,
        }
    }

    /// A trailing slash is the difference between a working endpoint and a 404,
    /// and users type it without thinking.
    #[test]
    fn the_url_survives_a_trailing_slash() {
        assert_eq!(chat_url("http://x/v1/"), "http://x/v1/chat/completions");
        assert_eq!(chat_url("http://x/v1"), "http://x/v1/chat/completions");
        assert_eq!(chat_url(" http://x/v1 "), "http://x/v1/chat/completions");
    }

    /// The body is valid JSON by construction and carries what a provider needs.
    #[test]
    fn the_body_carries_the_conversation() {
        let body = request_body(
            &config("http://x/v1"),
            &[
                ChatMessage {
                    role: ChatRole::System,
                    content: "you are brief".to_string(),
                },
                ChatMessage {
                    role: ChatRole::User,
                    content: "hi\nthere".to_string(),
                },
            ],
        );
        // A request is not a reply: reading one back yields nothing, which is
        // the failure a caller would otherwise take for an empty answer.
        assert!(parse_completion(&body).is_none());
        assert!(body.contains("\"model\":\"m\""));
        assert!(body.contains("\"role\":\"system\""));
        assert!(body.contains("\"role\":\"user\""));
        // A newline in the content is escaped, not raw: raw is invalid JSON.
        assert!(body.contains("hi\\nthere"));
        assert!(!body.contains("hi\n\nthere"), "the newline was written raw");
    }

    /// A reply whose text contains the shape of another reply must not be read
    /// as that other reply. This is the bug a `contains("\"content\":")` search
    /// would have.
    #[test]
    fn a_reply_quoting_the_wire_format_is_read_itself() {
        let text = r#"{"id":"x","choices":[{"index":0,"message":{"role":"assistant","content":"the reply says {\"role\":\"user\"} inside it"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":5}}"#;
        let parsed = parse_completion(text).expect("parses");
        assert_eq!(parsed.text, "the reply says {\"role\":\"user\"} inside it");
        assert_eq!(parsed.usage.input, 3);
        assert_eq!(parsed.usage.output, 5);
        assert_eq!(parsed.usage.total_tokens, 0);
    }

    /// An empty choices array is a malformed reply, not a panic.
    #[test]
    fn an_empty_choices_array_is_not_a_completion() {
        let text = r#"{"choices":[],"usage":{}}"#;
        assert!(parse_completion(text).is_none());
    }

    /// A missing usage is zeroed rather than refused.
    #[test]
    fn a_missing_usage_is_zero() {
        let text = r#"{"choices":[{"message":{"content":"ok"}}]}"#;
        let parsed = parse_completion(text).expect("parses");
        assert_eq!(parsed.text, "ok");
        assert_eq!(parsed.usage.input, 0);
    }

    /// Escapes a provider actually emits, including a surrogate pair and an
    /// emoji.
    #[test]
    fn unquote_resolves_what_providers_emit() {
        assert_eq!(unquote("\"a\\nb\"").as_deref(), Some("a\nb"));
        assert_eq!(unquote("\"a\\tb\"").as_deref(), Some("a\tb"));
        assert_eq!(unquote("\"\\u003cdiv\\u003e\"").as_deref(), Some("<div>"));
        assert_eq!(unquote("\"\\ud83d\\ude00\"").as_deref(), Some("\u{1f600}"));
        assert_eq!(unquote("\"no escapes\"").as_deref(), Some("no escapes"));
        assert_eq!(unquote("\"unterminated"), None);
    }

    /// A secret never survives into something the interface would print.
    #[test]
    fn a_key_is_redacted_from_provider_text() {
        const KEY: &str = "sk-super-secret-key";
        let body = format!("{{\"error\":\"bad key {KEY}\"}}");
        let shown = redact(&body, KEY);
        assert!(!shown.contains(KEY), "the key was not redacted");
        assert!(shown.contains("[redacted]"));
        // And an empty key redacts nothing rather than mangling the text.
        assert_eq!(redact("plain", ""), "plain");
    }

    /// The timeout is what the user feels, and it is a real bound: a worker
    /// that never answers must not wedge the interface for ever.
    #[test]
    fn a_dropped_job_times_out_rather_than_hanging() {
        let (tx, rx) = mpsc::channel::<Result<Completion, Failure>>();
        drop(tx);
        let outcome = rx.recv_timeout(Duration::from_millis(20));
        assert!(outcome.is_err(), "a closed channel blocked for ever");
    }

    /// Constructing an `AgentClient` costs a thread and a runtime, which is
    /// exactly what a `Default` for a field in `App` must not do inside a test
    /// suite that builds hundreds of frames — so `App` holds `None` and the
    /// transport is started explicitly.
    #[test]
    fn a_client_is_constructed_explicitly() {
        let client = AgentClient::spawn();
        let outcome = client.chat(
            "t1",
            config("http://127.0.0.1:1/v1"),
            vec![ChatMessage {
                role: ChatRole::User,
                content: "hi".to_string(),
            }],
            Duration::from_millis(200),
        );
        // Nothing is listening, so this is `Unreachable` and not a hang.
        assert!(matches!(outcome, Err(Failure::Unreachable(_))));
    }
}
