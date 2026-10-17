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

use futures::StreamExt;

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

/// One step of an answer arriving.
///
/// A model streams its reply as a series of deltas and then a final message with
/// the token counts, so the transport reports progress rather than one value.
/// The distinction matters because a reply being typed out on screen is a
/// partial one and must never be mistaken for a finished one: `Done` is what
/// moves the task from "asking" to "answered", and nothing else may.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    /// More of the reply, in order.
    Delta(String),
    /// The reply is complete, with whatever accounting the provider sent.
    Done(Usage),
    /// The reply is over and there is no answer.
    Failed(Failure),
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
    ///
    /// The sentence is the interface's own language rather than this module's
    /// English: an assistant that answers nothing is the hardest thing in the
    /// program to diagnose, and the reason has to arrive in words its reader
    /// uses.
    pub fn describe(&self, lang: crate::i18n::Lang) -> String {
        let s = lang.strings();
        match self {
            Failure::Unreachable(why) => {
                crate::text::fill(s.asst_unreachable, std::slice::from_ref(why))
            }
            Failure::Rejected(status, body) => {
                crate::text::fill(s.asst_rejected, &[status.to_string(), body.clone()])
            }
            Failure::Malformed => s.asst_malformed.to_string(),
        }
    }
}

/// A conversation sent to the model, and where its answer goes.
struct Job {
    config: AgentConfig,
    messages: Vec<ChatMessage>,
    reply: Option<Sender<(String, Progress)>>,
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
                // The reply channel is moved out first, because the stream sends
                // through it as it goes rather than once at the end.
                let reply = job.reply.take();
                runtime.block_on(send_stream(&job, reply));
            }
        });
        AgentClient { jobs: tx }
    }

    /// Ask the model and wait for the whole answer.
    ///
    /// The streaming path, collected. There is no second non-streaming request:
    /// a provider that answers in one message is a stream of one, and two code
    /// paths to the same wire would be two places for the parsing to drift.
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
        let mut text = String::new();
        loop {
            match rx.recv_timeout(timeout) {
                Ok((_, Progress::Delta(part))) => text.push_str(&part),
                Ok((_, Progress::Done(reported))) => {
                    return Ok(Completion {
                        text,
                        usage: reported,
                    })
                }
                Ok((_, Progress::Failed(failure))) => return Err(failure),
                Err(_) => return Err(Failure::Unreachable("the model took too long".to_string())),
            }
        }
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
    ) -> Option<Receiver<(String, Progress)>> {
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

/// One round trip, streamed, reporting as it goes.
///
/// The body asks for a stream with the usage reported at the end, which is the
/// one option every OpenAI-compatible provider understands. What comes back is
/// a series of `data:` lines separated by blank ones — server-sent events — and
/// the reply arrives as deltas of `choices[0].delta.content` until a final event
/// carries `finish_reason`, after which `usage` may follow in the same event or
/// in a later one.
///
/// A provider that does not stream still answers with one event and a
/// `message` rather than a `delta`, which is read the same way: the reply is in
/// `choices[0].message.content` and the token counts are in `usage`.
async fn send_stream(job: &Job, reply: Option<Sender<(String, Progress)>>) {
    let key = job.config.api_key.clone();
    let url = chat_url(&job.config.endpoint);

    let mut request = client_for()
        .post(&url)
        .header("content-type", "application/json")
        // Accept SSE, because a provider that supports both will otherwise
        // answer with JSON and the deltas would never arrive.
        .header("accept", "text/event-stream")
        .body(request_body(&job.config, &job.messages, true));
    if !key.is_empty() {
        request = request.header("authorization", format!("Bearer {key}"));
    }
    for (name, value) in &job.config.headers {
        request = request.header(name.as_str(), value.as_str());
    }

    let response = match request.send().await {
        Ok(response) => response,
        Err(why) => {
            send(
                &reply,
                &job.task_id,
                Progress::Failed(Failure::Unreachable(redact(&why.to_string(), &key))),
            );
            return;
        }
    };
    let status = response.status().as_u16();
    if status != 200 {
        // Read the body whatever the status: a provider's reason for refusing is
        // the one thing worth showing, and it is redacted on the way out.
        let body = response.text().await.unwrap_or_default();
        send(
            &reply,
            &job.task_id,
            Progress::Failed(Failure::Rejected(status, redact(&body, &key))),
        );
        return;
    }

    let mut stream = response.bytes_stream();
    // A partial line survives across chunks: an SSE event is not obliged to
    // arrive in one TCP segment, and cutting the buffer at the chunk boundary
    // would split `data: {...}` in half and lose the event.
    let mut buffer = String::new();
    // Bytes of a character that straddles two chunks: a delta is UTF-8 on the
    // wire, and the unfinished tail waits here for the rest of itself.
    let mut carry: Vec<u8> = Vec::new();
    // Whether any text was ever reported. A 200 whose body carried no
    // completion is not a reply, and the difference between it and an empty one
    // is worth keeping.
    let mut saw_text = false;
    let mut usage = Usage::default();
    while let Some(chunk) = stream.next().await {
        let bytes = match chunk {
            Ok(bytes) => bytes,
            Err(why) => {
                send(
                    &reply,
                    &job.task_id,
                    Progress::Failed(Failure::Unreachable(redact(&why.to_string(), &key))),
                );
                return;
            }
        };
        carry.extend_from_slice(&bytes);
        let decoded = decode_utf8(&mut carry);
        if decoded.is_empty() {
            continue;
        }
        buffer.push_str(&decoded);
        // Every event in the chunk is read, and any text it holds is sent as
        // one delta: a chunk is a unit of what arrived, and one delta per chunk
        // is what the interface renders on.
        let text = drain_events(&mut buffer, &mut usage);
        if !text.is_empty() {
            saw_text = true;
            send(&reply, &job.task_id, Progress::Delta(text));
        }
    }
    if !saw_text {
        // The provider answered 200 and the body held no completion at all — a
        // proxy's error page, or a provider answering in a shape this reader
        // does not know. Calling that a finished turn would file an empty reply
        // as a success and record it.
        send(&reply, &job.task_id, Progress::Failed(Failure::Malformed));
        return;
    }
    send(&reply, &job.task_id, Progress::Done(usage));
}

/// Push one step into the reply channel, if there is one.
///
/// A job with no channel is a job nobody asked to watch — `chat()` takes the
/// channel for itself — and a send that fails means the interface stopped
/// listening, which is not a transport error.
fn send(reply: &Option<Sender<(String, Progress)>>, task_id: &str, step: Progress) {
    if let Some(reply) = reply {
        let _ = reply.send((task_id.to_string(), step));
    }
}

/// The client, with the timeout the whole request has to fit in, and any proxy
/// the environment names.
///
/// A machine that can only reach the model through a proxy has no other way
/// out, and reqwest's own system-proxy support is not built into this binary —
/// so `HTTPS_PROXY`, `HTTP_PROXY` and `ALL_PROXY` (either case) are read here,
/// with `NO_PROXY` handed to reqwest as the bypass list. A proxy reqwest
/// cannot parse is skipped rather than made fatal: a typo in a variable is not
/// a reason for the assistant to stop working, and the request will say what
/// happened if it was the only way out.
fn client_for() -> reqwest::Client {
    let mut builder = reqwest::Client::builder().timeout(REQUEST_TIMEOUT);
    if let Some(proxy) = proxy_from_env() {
        builder = builder.proxy(proxy);
    }
    // A client per answer, because the timeout is part of the request and a
    // shared one would have to be built before the first caller knew its
    // budget. The connection pool is the cost, and an interactive assistant
    // asks one question at a time in practice.
    builder.build().unwrap_or_else(|_| reqwest::Client::new())
}

/// The proxy the environment names, if any.
///
/// `HTTPS_PROXY` is looked at first because that is the variable every provider
/// in the list is reached through, then `ALL_PROXY`, then `HTTP_PROXY` — the
/// order curl documents. The `NO_PROXY` list is parsed by reqwest rather than
/// here, because bypassing is its own rules about domains, ports and CIDRs.
fn proxy_from_env() -> Option<reqwest::Proxy> {
    proxy_from(|name| std::env::var(name).ok())
}

/// The variables that name a proxy, in the order they are read.
const PROXY_VARS: [&str; 6] = [
    "HTTPS_PROXY",
    "https_proxy",
    "ALL_PROXY",
    "all_proxy",
    "HTTP_PROXY",
    "http_proxy",
];

/// The same choice, taken from a lookup rather than the process environment, so
/// the order and the skipping are testable without touching globals.
///
/// A variable set to nothing, or to something reqwest cannot parse, is passed
/// over rather than made fatal: a typo in one of six names is not a reason for
/// the assistant to stop working, and the request will say what happened if
/// that was the only way out.
fn proxy_from(mut lookup: impl FnMut(&str) -> Option<String>) -> Option<reqwest::Proxy> {
    let mut proxy = PROXY_VARS
        .iter()
        .filter_map(|name| lookup(name))
        .find_map(|url| reqwest::Proxy::all(url.trim()).ok())?;
    if let Some(no_proxy) = reqwest::NoProxy::from_env() {
        proxy = proxy.no_proxy(Some(no_proxy));
    }
    Some(proxy)
}

/// The next complete SSE event in `buffer`, and what is left after it.
///
/// `\n\n` is the separator, and a lone `\r\n\r\n` is what some gateways send —
/// so the search is for the longest run of blank-line separators and the event
/// is everything before it.
/// Read every complete SSE event in `buffer`, leaving the unfinished tail
/// behind, and return the text they carried.
///
/// A chunk is not an event: one chunk can hold several events, and one event
/// can span several chunks. Reading the batch one event at a time without
/// joining them is how two events in one chunk came out as a single line of
/// mangled JSON — which parsed as nothing, and the reply went with it.
fn drain_events(buffer: &mut String, usage: &mut Usage) -> String {
    let mut text = String::new();
    while let Some((event, rest)) = next_event(buffer) {
        *buffer = rest;
        let Some(payload) = event.strip_prefix("data:") else {
            continue;
        };
        let payload = payload.trim();
        if payload == "[DONE]" {
            // The stream's own end. Whatever follows in the buffer is not part
            // of the reply.
            break;
        }
        // Text and usage are read from the same event, not one *or* the
        // other: several providers — every gateway that reports the bill in
        // its last delta rather than in an event of its own — put the usage
        // object beside the final slice of the reply, and an event that was
        // read for its tokens alone would drop the words it came with.
        if let Some(delta) = text_of(payload) {
            text.push_str(&delta);
        }
        if let Some(reported) = usage_of(payload) {
            *usage = reported;
        }
    }
    text
}

/// Decode the valid UTF-8 prefix of `bytes`, holding an unfinished character
/// back for the next call.
///
/// A delta may split a multi-byte character across two chunks, and the half
/// that arrives is not the character. Decoded lossily, one `你` becomes two
/// replacement characters — visible in the reply, and wrong in whatever the
/// interface stores from it. Decoded here, it arrives whole one chunk later.
fn decode_utf8(bytes: &mut Vec<u8>) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => {
            let text = text.to_string();
            bytes.clear();
            text
        }
        Err(error) if error.error_len().is_none() => {
            // Only the last character is incomplete: decode what is whole and
            // keep the rest for the next chunk.
            let whole = std::str::from_utf8(&bytes[..error.valid_up_to()])
                .expect("valid_up_to is a character boundary")
                .to_string();
            bytes.drain(..error.valid_up_to());
            whole
        }
        Err(_) => {
            // Genuinely invalid bytes: replacement characters are the honest
            // reading, and the character the provider meant to send never
            // existed.
            let text = String::from_utf8_lossy(bytes).into_owned();
            bytes.clear();
            text
        }
    }
}

fn next_event(buffer: &str) -> Option<(String, String)> {
    for separator in ["\r\n\r\n", "\n\n"] {
        if let Some(at) = buffer.find(separator) {
            let (event, rest) = buffer.split_at(at);
            let event = event.trim_end_matches('\r').to_string();
            let rest = rest[separator.len()..].to_string();
            return Some((event, rest));
        }
    }
    None
}

/// The text one stream event carries, if it carries any.
///
/// Two shapes reach here and both are real: a delta during the answer
/// (`choices[0].delta.content`), and a provider that answers in one message
/// (`choices[0].message.content`). Reading the delta first is not a preference —
/// a provider that sends both sends the message only once, at the end, and
/// reading the message on every event would repeat the whole reply.
fn text_of(event: &str) -> Option<String> {
    let choices = field(event, "choices")?;
    let first = first_element(choices)?;
    if let Some(delta) = field(first, "delta") {
        if let Some(content) = field(delta, "content") {
            return unquote(content);
        }
    }
    let message = field(first, "message")?;
    unquote(field(message, "content")?)
}

/// The token accounting a stream event carries, if it carries one.
fn usage_of(event: &str) -> Option<Usage> {
    let usage = field(event, "usage")?;
    // A provider that was not asked for its accounting still spells the key —
    // OpenAI sends `"usage":null` on every chunk — and reading that as a
    // report would overwrite the real one with zeros.
    let usage = usage.trim();
    if usage.is_empty() || usage == "null" {
        return None;
    }
    Some(parse_usage(usage))
}

/// The chat-completions URL. A trailing slash is stripped rather than
/// appended to, because `…/v1//chat/completions` is a 404 on some gateways and
/// `…/v1/chat/completions` is right on all of them.
fn chat_url(endpoint: &str) -> String {
    format!("{}/chat/completions", endpoint.trim_end_matches('/').trim())
}

/// The request body, written by hand.
///
/// No `serde_json`: the request has three fields, and a dependency that
/// serialises a struct with `#[derive]` to save eight lines of `format!` is a
/// dependency that then has to be taught about the response too. The endpoint,
/// the model and the messages are the whole of what a provider needs.
fn request_body(config: &AgentConfig, messages: &[ChatMessage], stream: bool) -> String {
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
    if stream {
        // `stream` on its own is enough for the deltas; `stream_options` is what
        // asks for the token counts in a final event, because the default is
        // that a streaming reply reports no usage at all — which would leave the
        // accounting permanently at zero for every question asked this way.
        out.push_str(",\"stream\":true");
        out.push_str(",\"stream_options\":{\"include_usage\":true}");
    }
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
    let number = |key: &str| {
        field(object, key)
            .and_then(|raw| raw.trim().parse().ok())
            .unwrap_or(0)
    };
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
                    // The escape after a high surrogate has to be a low one.
                    // Anything else is a lone surrogate, and subtracting the
                    // low range's start from a value below it would wrap — a
                    // provider typo is a replacement character, not a panic.
                    if !(0xdc00..0xe000).contains(&low) {
                        out.push('\u{fffd}');
                        continue;
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
    use crate::i18n::Lang;

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
            false,
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

    /// A chunk can hold several events, and every one of them is read in
    /// order. Joined into a single line without separators, the mangled JSON
    /// parsed as nothing and the whole reply was lost.
    #[test]
    fn every_event_in_a_batch_is_read() {
        let mut buffer = String::from(
            "data: {\"choices\":[{\"delta\":{\"content\":\"one \"}}]}\n\
             \ndata: {\"choices\":[{\"delta\":{\"content\":\"two\"}}]}\n\n\
             data: [DONE]\n\n",
        );
        let mut usage = Usage::default();
        let text = drain_events(&mut buffer, &mut usage);
        assert_eq!(text, "one two");
        assert!(buffer.is_empty(), "the whole batch was consumed");

        // And an event split across two chunks waits for its other half.
        let mut buffer = String::from("data: {\"choices\":[{\"delta\":{\"content\":\"hal");
        assert_eq!(drain_events(&mut buffer, &mut usage), "");
        buffer.push_str("f\"}}]}\n\n");
        assert_eq!(drain_events(&mut buffer, &mut usage), "half");
    }

    /// A character split across two chunks stays one character: decoded
    /// lossily it would arrive as two replacement characters, in the reply and
    /// in whatever the interface stores from it.
    #[test]
    fn a_character_split_across_chunks_is_joined() {
        let text = "温度";
        let bytes = text.as_bytes();
        let mut carry = bytes[..4].to_vec();
        assert_eq!(
            decode_utf8(&mut carry),
            "温",
            "the whole character came out"
        );
        carry.extend_from_slice(&bytes[4..]);
        assert_eq!(decode_utf8(&mut carry), "度");
        assert!(carry.is_empty(), "nothing was left over");
    }

    /// The environment is the only way out on a machine that can only reach the
    /// model through a proxy: the first variable naming a usable one is taken,
    /// a blank or unparsable one is passed over rather than made fatal, and no
    /// variable at all is no proxy rather than an error.
    #[test]
    fn the_proxy_comes_from_the_environment_in_order() {
        assert!(
            proxy_from(|_| None).is_none(),
            "no variable, no proxy — and no complaint"
        );

        let lowercase =
            |name: &str| (name == "all_proxy").then(|| "http://127.0.0.1:7890".to_string());
        assert!(proxy_from(lowercase).is_some(), "a lowercase name counts");

        let blank_then_good = |name: &str| match name {
            "HTTPS_PROXY" => Some("   ".to_string()),
            "http_proxy" => Some("http://127.0.0.1:7890".to_string()),
            _ => None,
        };
        assert!(
            proxy_from(blank_then_good).is_some(),
            "a variable set to nothing is not a proxy, and the next one is"
        );

        let garbage_then_good = |name: &str| match name {
            "HTTPS_PROXY" => Some("not a url".to_string()),
            "HTTP_PROXY" => Some("http://127.0.0.1:7890".to_string()),
            _ => None,
        };
        assert!(
            proxy_from(garbage_then_good).is_some(),
            "an unparsable variable is skipped rather than made fatal"
        );

        assert!(
            proxy_from(|_| Some("not a url".to_string())).is_none(),
            "nothing usable anywhere is no proxy"
        );
    }

    /// A failed turn is said in the interface's own words: the reason an
    /// assistant answered nothing is the hardest thing in the program to
    /// diagnose, and an English sentence under a Chinese interface is half of
    /// the reason.
    #[test]
    fn a_failure_is_said_in_the_interfaces_language() {
        let unreachable = Failure::Unreachable("connection refused".to_string());
        assert_eq!(
            unreachable.describe(Lang::En),
            "the model did not answer: connection refused"
        );
        assert_eq!(
            unreachable.describe(Lang::Zh),
            "模型没有应答：connection refused",
            "the provider's own wording is kept as it came"
        );

        let rejected = Failure::Rejected(503, "upstream is down".to_string());
        assert_eq!(
            rejected.describe(Lang::Zh),
            "服务商拒绝（HTTP 503）：upstream is down"
        );
        assert!(rejected
            .describe(Lang::En)
            .starts_with("the provider refused"));

        let malformed = Failure::Malformed;
        assert_eq!(
            malformed.describe(Lang::Zh),
            "服务商返回了正文，但里面没有回答"
        );
        assert_eq!(
            malformed.describe(Lang::En),
            "the provider answered, but its body held no reply"
        );
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
    /// A stream event is only complete when its blank line has arrived, and a
    /// chunk can split one — cutting the buffer at the chunk boundary would
    /// lose the event.
    #[test]
    fn a_partial_event_is_kept_until_its_blank_line() {
        let mut buffer = "data: {\"choices\":[".to_string();
        assert!(
            next_event(&buffer).is_none(),
            "an incomplete event was read"
        );
        buffer.push_str("{\"finish_reason\":\"stop\"}]}\n\n");
        let (event, rest) = next_event(&buffer).expect("now complete");
        assert_eq!(event, "data: {\"choices\":[{\"finish_reason\":\"stop\"}]}");
        assert!(rest.is_empty());
    }

    /// Both blank-line spellings, because gateways send either.
    #[test]
    fn both_blank_line_spellings_separate_events() {
        for separator in ["\n\n", "\r\n\r\n"] {
            let text = format!("data: one{separator}data: two{separator}");
            let (first, rest) = next_event(&text).expect("split");
            assert_eq!(first, "data: one", "{separator:?}");
            assert_eq!(rest, format!("data: two{separator}"));
        }
    }

    /// A delta is what arrives while the model writes.
    #[test]
    fn a_delta_is_what_arrives_mid_stream() {
        let event = r#"{"choices":[{"index":0,"delta":{"role":"assistant","content":"hel"}}]}"#;
        assert_eq!(text_of(event).as_deref(), Some("hel"));
    }

    /// The event that carries the bill carries words too, on every gateway that
    /// reports usage beside its last delta rather than in an event of its own:
    /// read for its tokens alone, the reply loses its ending — live, this was
    /// `hi there` arriving as `hi`.
    #[test]
    fn a_delta_and_its_usage_are_read_from_one_event() {
        let mut buffer = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\" there\"}}],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2}}\n\n",
            "data: [DONE]\n\n"
        )
        .to_string();
        let mut usage = Usage::default();
        let text = drain_events(&mut buffer, &mut usage);
        assert_eq!(text, "hi there", "the last delta went with the usage");
        assert_eq!(usage.input, 3);
        assert_eq!(usage.output, 2);
    }

    /// `"usage":null` is a key with no report in it — OpenAI spells it that way
    /// when the accounting was not asked for — and reading it as a report would
    /// overwrite a real one with zeros.
    #[test]
    fn a_null_usage_is_not_a_report() {
        assert!(usage_of(r#"{"choices":[],"usage":null}"#).is_none());
        assert!(usage_of(r#"{"choices":[]}"#).is_none());
        let reported = usage_of(r#"{"usage":{"prompt_tokens":5}}"#).expect("a report");
        assert_eq!(reported.input, 5);
    }

    /// A provider that answers in one message puts it in `message`, not `delta`.
    #[test]
    fn a_single_message_is_read_from_message() {
        let event = r#"{"choices":[{"index":0,"message":{"role":"assistant","content":"done"},"finish_reason":"stop"}],"usage":{"prompt_tokens":2,"completion_tokens":3}}"#;
        assert_eq!(text_of(event).as_deref(), Some("done"));
        let usage = usage_of(event).expect("usage");
        assert_eq!(usage.input, 2);
        assert_eq!(usage.output, 3);
    }

    /// `[DONE]` is a sentinel with no JSON, and must not be read as a delta.
    #[test]
    fn the_done_sentinel_is_not_a_delta() {
        assert!(text_of("data: [DONE]").is_none());
    }

    /// A transport that is started and answered by nothing is `Unreachable`,
    /// not a hang — the only way to test the whole path without a server.
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
