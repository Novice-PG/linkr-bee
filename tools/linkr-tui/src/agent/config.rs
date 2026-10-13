//! Agent configuration (`web/agent_config.js`).
//!
//! Endpoint security classification, the HTTP-header codec, the provider list,
//! reasoning levels, and the strict config validator. Plain HTTP sends the API
//! key and serial logs in cleartext, so a non-loopback `http:` endpoint is
//! flagged so the UI can ask for consent before a credential is stored.

/// A shared key-value storage seam (`localStorage` upstream, any map here).
pub trait Storage {
    fn get(&self, key: &str) -> Option<String>;
    fn set(&mut self, key: &str, value: &str);
    fn remove(&mut self, key: &str);
}

pub const AGENT_CONFIG_KEY: &str = "linkr-agent-model";

/// What a plain `http:` endpoint exposes: the key only when it is not loopback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndpointSecurity {
    pub plaintext: bool,
    pub loopback: bool,
    pub exposes_key: bool,
}

/// Classify an endpoint URL. A local model server over loopback never leaves
/// the machine, so it stays a normal configuration; anything else over plain
/// HTTP exposes the credential.
pub fn endpoint_security(endpoint: &str) -> EndpointSecurity {
    // A URL parse that can fail returns the safe "nothing exposed" shape.
    let Some((protocol, hostname)) = parse_url_parts(endpoint) else {
        return EndpointSecurity {
            plaintext: false,
            loopback: false,
            exposes_key: false,
        };
    };
    let host = hostname
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase();
    let loopback = host == "localhost"
        || host == "::1"
        || host.ends_with(".localhost")
        || host.starts_with("127.");
    let plaintext = protocol == "http";
    EndpointSecurity {
        plaintext,
        loopback,
        exposes_key: plaintext && !loopback,
    }
}

/// Split a URL into `(protocol, hostname)` without pulling in a URL parser.
/// Returns `None` when the string is not a plausible `http(s)://host` URL.
fn parse_url_parts(endpoint: &str) -> Option<(String, String)> {
    let rest = endpoint
        .strip_prefix("http://")
        .or_else(|| endpoint.strip_prefix("https://"))?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let hostname = authority.split('@').next_back()?; // drop userinfo if present
    let hostname = strip_port(hostname);
    if hostname.is_empty() {
        return None;
    }
    Some((
        if endpoint.starts_with("https://") {
            "https"
        } else {
            "http"
        }
        .to_string(),
        hostname.to_string(),
    ))
}

pub const AGENT_HEADER_LIMIT: usize = 8;

/// Drop a trailing `:port`, but keep a bracketed IPv6 host intact.
fn strip_port(authority: &str) -> &str {
    if authority.starts_with('[') {
        match authority.find(']') {
            Some(close) => &authority[..=close],
            None => authority,
        }
    } else {
        match authority.find(':') {
            Some(i) => &authority[..i],
            None => authority,
        }
    }
}

/// Parse `Name: value` lines into a header map. Names and values are restricted
/// so a stored record cannot inject a new line into the HTTP request.
pub fn parse_agent_headers(text: &str) -> Result<Vec<(String, String)>, &'static str> {
    let mut headers: Vec<(String, String)> = Vec::new();
    let lines: Vec<&str> = text
        .split(['\r', '\n'])
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if lines.len() > AGENT_HEADER_LIMIT {
        return Err("bad header");
    }
    for line in lines {
        let separator = line.find(':').ok_or("bad header")?;
        if separator == 0 {
            return Err("bad header");
        }
        let name = line[..separator].trim();
        let value = line[separator + 1..].trim();
        if !valid_header_name(name)
            || value.is_empty()
            || value.len() > 256
            || value.chars().any(|c| c.is_control())
        {
            return Err("bad header");
        }
        // A duplicate name keeps the last value, matching the object assignment.
        headers.retain(|(n, _)| n != name);
        headers.push((name.to_string(), value.to_string()));
    }
    Ok(headers)
}

fn valid_header_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Serialise a header map back to `Name: value` lines, in insertion order.
pub fn format_agent_headers(headers: &[(String, String)]) -> String {
    headers
        .iter()
        .map(|(name, value)| format!("{name}: {value}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A provider's identity and its bilingual display strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentProvider {
    pub id: &'static str,
    pub label_zh: &'static str,
    pub label_en: &'static str,
    pub hint_zh: &'static str,
    pub hint_en: &'static str,
}

pub const AGENT_PROVIDERS: [AgentProvider; 3] = [
    AgentProvider {
        id: "openai-completions",
        label_zh: "OpenAI 兼容接口",
        label_en: "OpenAI-compatible API",
        hint_zh: "适用于提供 /chat/completions 的服务，例如 OpenAI、DeepSeek、Ollama、vLLM",
        hint_en: "For services exposing /chat/completions, such as OpenAI, DeepSeek, Ollama or vLLM",
    },
    AgentProvider {
        id: "anthropic-messages",
        label_zh: "Anthropic Messages 接口",
        label_en: "Anthropic Messages API",
        hint_zh: "适用于提供 /v1/messages 的服务，例如 https://api.anthropic.com/v1",
        hint_en: "For services exposing /v1/messages, such as https://api.anthropic.com/v1",
    },
    AgentProvider {
        id: "google-generative-ai",
        label_zh: "Google Gemini 接口",
        label_en: "Google Generative AI",
        hint_zh: "适用于提供 generateContent 的服务，例如 https://generativelanguage.googleapis.com/v1beta",
        hint_en: "For services exposing generateContent, such as https://generativelanguage.googleapis.com/v1beta",
    },
];

/// Ascending thinking budget. `off` leaves reasoning options out of the request.
pub const AGENT_REASONING_LEVELS: [&str; 4] = ["off", "low", "medium", "high"];

pub const AGENT_DEFAULT_CONTEXT_WINDOW: i64 = 32768;
pub const AGENT_DEFAULT_MAX_TOKENS: i64 = 4096;

/// What the assistant costs before any conversation is sent: the system prompt
/// plus every tool description. A window below this cannot hold a request.
pub const AGENT_FIXED_CONTEXT_TOKENS: i64 = 8000;

/// The smallest context window that can carry the fixed part plus the output
/// plus a little history and framing.
pub fn minimum_useful_context_window(max_tokens: i64) -> i64 {
    let output = if max_tokens > 0 {
        max_tokens
    } else {
        AGENT_DEFAULT_MAX_TOKENS
    };
    AGENT_FIXED_CONTEXT_TOKENS + output + 512
}

/// `0` means "use the built-in default", so it is never tight. Only an explicit
/// window is judged.
pub fn is_context_window_tight(context_window: i64, max_tokens: i64) -> bool {
    if context_window <= 0 {
        return false;
    }
    context_window < minimum_useful_context_window(max_tokens)
}

/// A fully validated, normalised agent configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentConfig {
    pub endpoint: String,
    pub model: String,
    pub api_key: String,
    pub headers: Vec<(String, String)>,
    pub provider: &'static str,
    pub reasoning: &'static str,
    pub context_window: i64,
    pub max_tokens: i64,
}

/// Validate a config and normalise it. Field-specific errors are reported by
/// name, matching the upstream `throw new Error(field)`.
pub fn validate_agent_config(input: &AgentConfigInput) -> Result<AgentConfig, String> {
    // endpoint: http(s), no userinfo, no query, no fragment.
    let endpoint = input.endpoint.trim();
    let (protocol, hostname) = parse_url_parts(endpoint).ok_or_else(|| "endpoint".to_string())?;
    let _ = protocol;
    // Reject userinfo / query / fragment by checking the raw string shape.
    let rest = endpoint
        .strip_prefix("http://")
        .or_else(|| endpoint.strip_prefix("https://"))
        .ok_or_else(|| "endpoint".to_string())?;
    if rest.contains('@') || rest.contains('?') || rest.contains('#') {
        return Err("endpoint".to_string());
    }
    if hostname.is_empty() {
        return Err("endpoint".to_string());
    }

    if input.model.trim().is_empty() {
        return Err("model".to_string());
    }

    // headers
    let mut normalized_headers: Vec<(String, String)> = Vec::new();
    if input.headers.len() > AGENT_HEADER_LIMIT {
        return Err("headers".to_string());
    }
    for (name, value) in &input.headers {
        if !valid_header_name(name)
            || value.is_empty()
            || value.len() > 256
            || value.chars().any(|c| c.is_control())
        {
            return Err("headers".to_string());
        }
        normalized_headers.retain(|(n, _)| n != name);
        normalized_headers.push((name.clone(), value.clone()));
    }

    if !AGENT_PROVIDERS.iter().any(|p| p.id == input.provider) {
        return Err("provider".to_string());
    }
    if !AGENT_REASONING_LEVELS.contains(&input.reasoning) {
        return Err("reasoning".to_string());
    }

    Ok(AgentConfig {
        endpoint: endpoint.trim_end_matches('/').to_string(),
        model: input.model.trim().to_string(),
        api_key: input.api_key.clone(),
        headers: normalized_headers,
        provider: input.provider,
        reasoning: input.reasoning,
        context_window: validate_range(input.context_window, "contextWindow", (1000, 2_000_000))?,
        max_tokens: validate_range(input.max_tokens, "maxTokens", (1, 100_000))?,
    })
}

fn validate_range(value: i64, field: &str, (min, max): (i64, i64)) -> Result<i64, String> {
    if value != 0 && !(min..=max).contains(&value) {
        return Err(field.to_string());
    }
    Ok(value)
}

/// The unvalidated input shape, mirroring the upstream `{...}` argument.
#[derive(Debug, Clone, Default)]
pub struct AgentConfigInput {
    pub endpoint: String,
    pub model: String,
    pub api_key: String,
    pub headers: Vec<(String, String)>,
    pub provider: &'static str,
    pub reasoning: &'static str,
    pub context_window: i64,
    pub max_tokens: i64,
}

impl AgentConfigInput {
    /// A blank input, with the provider and reasoning already at their defaults.
    pub fn new() -> Self {
        AgentConfigInput {
            provider: AGENT_PROVIDERS[0].id,
            reasoning: AGENT_REASONING_LEVELS[0],
            ..AgentConfigInput::default()
        }
    }
}

/// A request that never reached the model arrives as an opaque transport
/// error. Both the SDK's `APIConnectionError` name and the common message
/// phrasings are recognised, because the fix is browser-side, not model-side.
pub fn is_endpoint_unreachable(error_name: &str, message: &str) -> bool {
    if error_name == "APIConnectionError" {
        return true;
    }
    let lower = message.to_ascii_lowercase();
    [
        "failed to fetch",
        "fetch failed",
        "networkerror",
        "load failed",
        "network request failed",
        "connection error",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plaintext_non_loopback_exposes_the_key() {
        let s = endpoint_security("http://10.0.0.5:8080/v1");
        assert!(s.plaintext);
        assert!(!s.loopback);
        assert!(s.exposes_key);
    }

    #[test]
    fn loopback_plaintext_does_not_expose_the_key() {
        for host in [
            "http://localhost:8080",
            "http://127.0.0.1:11434",
            "http://[::1]:8080",
        ] {
            let s = endpoint_security(host);
            assert!(s.plaintext, "{host}");
            assert!(s.loopback, "{host}");
            assert!(!s.exposes_key, "{host}");
        }
    }

    #[test]
    fn https_is_never_plaintext() {
        let s = endpoint_security("https://api.example.com/v1");
        assert!(!s.plaintext);
        assert!(!s.exposes_key);
    }

    #[test]
    fn an_unparseable_endpoint_is_safe_by_default() {
        let s = endpoint_security("not a url");
        assert!(!s.exposes_key);
    }

    #[test]
    fn headers_round_trip() {
        let headers = parse_agent_headers("Authorization: Bearer x\nX-Target: rk3588").unwrap();
        assert_eq!(headers.len(), 2);
        assert_eq!(
            headers[0],
            ("Authorization".to_string(), "Bearer x".to_string())
        );
        assert_eq!(
            format_agent_headers(&headers),
            "Authorization: Bearer x\nX-Target: rk3588"
        );
    }

    #[test]
    fn headers_reject_control_characters_and_blank_names() {
        assert!(parse_agent_headers("Bad: has\u{0}control").is_err());
        assert!(parse_agent_headers(": no name").is_err());
        assert!(parse_agent_headers("no-colon-here").is_err());
    }

    #[test]
    fn headers_reject_more_than_the_limit() {
        let many = (0..9)
            .map(|i| format!("X-H{i}: v"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(parse_agent_headers(&many).is_err());
    }

    #[test]
    fn a_valid_config_normalises_the_endpoint_trailing_slash() {
        let cfg = validate_agent_config(&AgentConfigInput {
            endpoint: "https://api.example.com/v1/".to_string(),
            model: " model-x ".to_string(),
            provider: "anthropic-messages",
            reasoning: "off",
            ..AgentConfigInput::default()
        })
        .unwrap();
        assert_eq!(cfg.endpoint, "https://api.example.com/v1");
        assert_eq!(cfg.model, "model-x");
    }

    #[test]
    fn a_config_with_userinfo_query_or_fragment_is_refused() {
        for bad in ["https://u:p@h/v1", "https://h/v1?q=1", "https://h/v1#frag"] {
            assert_eq!(
                validate_agent_config(&AgentConfigInput {
                    endpoint: bad.to_string(),
                    model: "m".to_string(),
                    ..AgentConfigInput::default()
                }),
                Err("endpoint".to_string())
            );
        }
    }

    #[test]
    fn an_unknown_provider_or_reasoning_is_refused() {
        let base = AgentConfigInput::new();
        let base = AgentConfigInput {
            endpoint: "https://h/v1".to_string(),
            model: "m".to_string(),
            ..base
        };
        assert_eq!(
            validate_agent_config(&AgentConfigInput {
                provider: "nope",
                ..base.clone()
            }),
            Err("provider".to_string())
        );
        assert_eq!(
            validate_agent_config(&AgentConfigInput {
                reasoning: "nope",
                ..base.clone()
            }),
            Err("reasoning".to_string())
        );
    }

    #[test]
    fn context_window_tightness_tracks_the_floor() {
        assert!(!is_context_window_tight(0, 0));
        assert!(!is_context_window_tight(
            minimum_useful_context_window(0),
            0
        ));
        assert!(is_context_window_tight(1000, 0));
    }
}
