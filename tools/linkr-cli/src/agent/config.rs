//! Model and provider configuration: port of `web/agent_config.js`
//! (validation, endpoint security, extra headers) plus the pricing helpers of
//! `web/agent_usage.js`, persisted next to the other agent state.
//!
//! Storage: `dirs::config_dir()/linkr/agent.json` holds the active config
//! under the shape of the `linkr-agent-model` record; the display-only prices
//! live in `agent_pricing.json` under `linkr-agent-pricing-v1`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{AgentConfig, Provider};

/// Storage key of the active config (spec §14.2).
pub const AGENT_CONFIG_KEY: &str = "linkr-agent-model";
/// Storage key of the display-only price table.
pub const PRICING_KEY: &str = "linkr-agent-pricing-v1";
/// At most 8 extra request headers.
pub const AGENT_HEADER_LIMIT: usize = 8;
/// "Not set" means the built-in default, which is large enough.
pub const AGENT_DEFAULT_CONTEXT_WINDOW: u32 = 32768;
pub const AGENT_DEFAULT_MAX_TOKENS: u32 = 4096;
/// The fixed part (prompt plus tool descriptions) is measured pessimistically
/// at 3 characters per token; a window below this cannot carry a request.
pub const AGENT_FIXED_CONTEXT_TOKENS: u32 = 8000;
/// Validation ranges, copied from `web/agent_config.js`, where
/// `CONTEXT_WINDOW_RANGE = [1000, 2000000]` and `MAX_TOKENS_RANGE = [1, 100000]`
/// bound what the settings form accepts (a stored `0` is always allowed and
/// means "use the built-in default"). AGENT_SPEC §12.1 still quotes the older
/// 2048/32768 clamps, which no longer match the web code; keeping a narrower
/// range here would reject a record the TUI's own dialog just wrote.
pub const CONTEXT_WINDOW_RANGE: (u32, u32) = (1_000, 2_000_000);
pub const MAX_TOKENS_RANGE: (u32, u32) = (1, 100_000);
pub const REASONING_LEVELS: [&str; 4] = ["off", "low", "medium", "high"];

impl Provider {
    pub fn as_str(self) -> &'static str {
        match self {
            Provider::OpenAiCompat => "openai-completions",
            Provider::AnthropicMessages => "anthropic-messages",
            Provider::GoogleGemini => "google-generative-ai",
        }
    }

    /// An unknown provider keeps the historical protocol, like the JS default.
    pub fn parse(value: &str) -> Provider {
        match value {
            "anthropic-messages" => Provider::AnthropicMessages,
            "google-generative-ai" => Provider::GoogleGemini,
            _ => Provider::OpenAiCompat,
        }
    }

    pub const ALL: [Provider; 3] = [
        Provider::OpenAiCompat,
        Provider::AnthropicMessages,
        Provider::GoogleGemini,
    ];
}

/// `dirs::config_dir()/linkr`, where `agent.json`, `agent_pricing.json` and
/// `command_policy.json` live.
pub fn config_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("linkr"))
}

fn control_chars(text: &str) -> bool {
    text.bytes().any(|b| b < 0x20 || b == 0x7f)
}

/// The persisted record: camelCase like the JS storage shape.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StoredConfig {
    pub provider: String,
    pub endpoint: String,
    #[serde(rename = "apiKey")]
    pub api_key: String,
    pub model: String,
    #[serde(rename = "contextWindow")]
    pub context_window: u32,
    #[serde(rename = "maxTokens")]
    pub max_tokens: u32,
    pub reasoning: String,
    pub headers: BTreeMap<String, String>,
    #[serde(rename = "priceInput")]
    pub price_input: f64,
    #[serde(rename = "priceOutput")]
    pub price_output: f64,
}

impl From<&AgentConfig> for StoredConfig {
    fn from(config: &AgentConfig) -> Self {
        StoredConfig {
            provider: config.provider.as_str().to_string(),
            endpoint: config.endpoint.clone(),
            api_key: config.api_key.clone().unwrap_or_default(),
            model: config.model.clone(),
            context_window: config.context_window,
            max_tokens: config.max_tokens,
            reasoning: config.reasoning.clone(),
            headers: config
                .extra_headers
                .iter()
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect(),
            price_input: config.price_input,
            price_output: config.price_output,
        }
    }
}

/// Validate a stored record into the runtime config.
///
/// Error strings are the exact ones of spec §12.1; the provider falls back to
/// the OpenAI-compatible protocol instead of failing, like the JS normalizer.
///
/// A stored `0` for `contextWindow` / `maxTokens` means "use the built-in
/// default" and is resolved here, the way the web reads it at every use site
/// (`e.maxTokens || 4096`, `e.contextWindow || 32768`). Handing the `0` to the
/// provider instead yields `400 Invalid max_tokens value`, which is how the
/// blank "Max output tokens" field used to make every assistant turn fail.
pub fn validate_agent_config(stored: &StoredConfig) -> Result<AgentConfig, String> {
    let endpoint = normalize_endpoint(&stored.endpoint)?;
    let model = stored.model.trim().to_string();
    if model.is_empty() {
        return Err("Enter a model ID.".to_string());
    }
    let context_window = stored.context_window;
    if context_window != 0
        && (context_window < CONTEXT_WINDOW_RANGE.0 || context_window > CONTEXT_WINDOW_RANGE.1)
    {
        return Err(format!(
            "Context window must be 0, or an integer between {} and {}.",
            CONTEXT_WINDOW_RANGE.0, CONTEXT_WINDOW_RANGE.1
        ));
    }
    let max_tokens = stored.max_tokens;
    if max_tokens != 0 && (max_tokens < MAX_TOKENS_RANGE.0 || max_tokens > MAX_TOKENS_RANGE.1) {
        return Err(format!(
            "Max output tokens must be 0, or an integer between {} and {}.",
            MAX_TOKENS_RANGE.0, MAX_TOKENS_RANGE.1
        ));
    }
    let context_window = if context_window == 0 {
        AGENT_DEFAULT_CONTEXT_WINDOW
    } else {
        context_window
    };
    let max_tokens = if max_tokens == 0 {
        AGENT_DEFAULT_MAX_TOKENS
    } else {
        max_tokens
    };
    let reasoning = stored.reasoning.clone();
    if reasoning.is_empty() {
        // "off" is the built-in default of a record without the field.
    } else if !REASONING_LEVELS.contains(&reasoning.as_str()) {
        return Err("Choose a reasoning effort.".to_string());
    }
    let mut extra_headers = Vec::new();
    if stored.headers.len() > AGENT_HEADER_LIMIT {
        return Err("headers".to_string());
    }
    for (name, value) in &stored.headers {
        if !valid_header_name(name)
            || value.is_empty()
            || value.chars().count() > 256
            || control_chars(value)
        {
            return Err("headers".to_string());
        }
        extra_headers.push((name.clone(), value.clone()));
    }
    Ok(AgentConfig {
        endpoint,
        model,
        api_key: if stored.api_key.is_empty() {
            None
        } else {
            Some(stored.api_key.clone())
        },
        provider: Provider::parse(&stored.provider),
        reasoning: if reasoning.is_empty() {
            "off".to_string()
        } else {
            reasoning
        },
        extra_headers,
        context_window,
        max_tokens,
        price_input: stored.price_input,
        price_output: stored.price_output,
    })
}

fn valid_header_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'-')
}

/// A plain base URL: http(s), no credentials, no query, no fragment. One
/// trailing slash is normalized away so the provider adapters can append a
/// path deterministically.
fn normalize_endpoint(endpoint: &str) -> Result<String, String> {
    const ERR: &str =
        "Enter an HTTP(S) API base URL without credentials, query parameters or a fragment.";
    let url = reqwest::Url::parse(endpoint.trim()).map_err(|_| ERR.to_string())?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err(ERR.to_string());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(ERR.to_string());
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(ERR.to_string());
    }
    let mut href = url.to_string();
    if href.ends_with('/') {
        href.pop();
    }
    Ok(href)
}

/// Plain http sends the API key in cleartext; loopback never leaves the
/// machine, so only `exposes_key` deserves a consent prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EndpointSecurity {
    pub plaintext: bool,
    pub loopback: bool,
    pub exposes_key: bool,
}

pub fn endpoint_security(endpoint: &str) -> EndpointSecurity {
    let Ok(url) = reqwest::Url::parse(endpoint) else {
        return EndpointSecurity::default();
    };
    let host = url
        .host_str()
        .unwrap_or("")
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_lowercase();
    let loopback = host == "localhost"
        || host == "::1"
        || host.ends_with(".localhost")
        || host.starts_with("127.");
    let plaintext = url.scheme() == "http";
    EndpointSecurity {
        plaintext,
        loopback,
        exposes_key: plaintext && !loopback,
    }
}

/// `"Name: value"` lines. Several endpoints only answer a request when a
/// specific header is present; names and values are restricted so a stored
/// record cannot inject a new line into the HTTP request.
pub fn parse_agent_headers(text: &str) -> Result<Vec<(String, String)>, String> {
    let mut headers = Vec::new();
    // `/\r?\n/`: a lone CR inside a value stays part of the value and is then
    // rejected by the control-character rule, exactly like the JS splitter.
    let lines: Vec<&str> = text
        .split('\n')
        .map(|line| line.trim_end_matches('\r').trim())
        .filter(|line| !line.is_empty())
        .collect();
    if lines.len() > AGENT_HEADER_LIMIT {
        return Err("headers".to_string());
    }
    for line in lines {
        let separator = match line.find(':') {
            Some(index) if index > 0 => index,
            _ => return Err("headers".to_string()),
        };
        let name = line[..separator].trim().to_string();
        let value = line[separator + 1..].trim().to_string();
        if !valid_header_name(&name)
            || value.is_empty()
            || value.chars().count() > 256
            || control_chars(&value)
        {
            return Err("headers".to_string());
        }
        headers.push((name, value));
    }
    Ok(headers)
}

pub fn format_agent_headers(headers: &[(String, String)]) -> String {
    headers
        .iter()
        .map(|(name, value)| format!("{}: {}", name, value))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A window below this cannot hold the fixed part plus the requested output.
pub fn minimum_useful_context_window(max_tokens: u32) -> u32 {
    let output = if max_tokens > 0 {
        max_tokens
    } else {
        AGENT_DEFAULT_MAX_TOKENS
    };
    AGENT_FIXED_CONTEXT_TOKENS + output + 512
}

/// 0 means "use the built-in default", so it is never judged.
pub fn is_context_window_tight(context_window: u32, max_tokens: u32) -> bool {
    if context_window == 0 {
        return false;
    }
    context_window < minimum_useful_context_window(max_tokens)
}

// --- storage ---------------------------------------------------------------

fn read_stored(path: &Path) -> Option<StoredConfig> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str::<StoredConfig>(&raw).ok()
}

/// Load the active config; an unreadable or invalid record is `None`, exactly
/// like `loadAgentConfig`.
pub fn load_config_from(dir: &Path) -> Option<AgentConfig> {
    let stored = read_stored(&dir.join("agent.json"))?;
    validate_agent_config(&stored).ok()
}

pub fn load_config() -> Option<AgentConfig> {
    let dir = config_dir()?;
    load_config_from(&dir)
}

/// Validate and persist. A successful return is the normalized config whose
/// prices travel with it so the settings form and the runtime cannot disagree.
pub fn save_config_to(dir: &Path, config: &AgentConfig) -> Result<AgentConfig, String> {
    let normalized = validate_agent_config(&StoredConfig::from(config))?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let text = serde_json::to_string_pretty(&StoredConfig::from(&normalized))
        .map_err(|e| e.to_string())?;
    std::fs::write(dir.join("agent.json"), text).map_err(|e| e.to_string())?;
    Ok(normalized)
}

pub fn save_config(config: &AgentConfig) -> Result<AgentConfig, String> {
    let dir =
        config_dir().ok_or_else(|| "Unable to locate the configuration directory.".to_string())?;
    save_config_to(&dir, config)
}

pub fn clear_config_from(dir: &Path) -> Result<(), String> {
    match std::fs::remove_file(dir.join("agent.json")) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.to_string()),
    }
}

pub fn clear_config() -> Result<(), String> {
    let dir =
        config_dir().ok_or_else(|| "Unable to locate the configuration directory.".to_string())?;
    clear_config_from(&dir)
}

// --- pricing ---------------------------------------------------------------

/// Prices are quoted per million tokens. They are display-only: they never
/// take part in a model request, so changing them cannot invalidate a run.
pub fn parse_pricing(input: f64, output: f64) -> Result<(f64, f64), String> {
    let rate = |value: f64| -> Result<f64, String> {
        if !(0.0..=100_000.0).contains(&value) || value.is_nan() {
            return Err("pricing".to_string());
        }
        Ok(value)
    };
    Ok((rate(input)?, rate(output)?))
}

pub fn load_pricing_from(dir: &Path) -> (f64, f64) {
    read_stored(&dir.join("agent_pricing.json"))
        .map(|stored| (stored.price_input, stored.price_output))
        .unwrap_or((0.0, 0.0))
}

pub fn save_pricing_to(dir: &Path, input: f64, output: f64) -> Result<(f64, f64), String> {
    let (input, output) = parse_pricing(input, output)?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let stored = StoredConfig {
        price_input: input,
        price_output: output,
        ..StoredConfig::default()
    };
    let text = serde_json::to_string_pretty(&stored).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("agent_pricing.json"), text).map_err(|e| e.to_string())?;
    Ok((input, output))
}

/// `tokens / 1e6 * rate`, summed; `None` when no price is configured at all.
pub fn estimate_cost(
    input_tokens: u64,
    output_tokens: u64,
    input: f64,
    output: f64,
) -> Option<f64> {
    if input == 0.0 && output == 0.0 {
        return None;
    }
    let cost = (input_tokens as f64 / 1e6) * input + (output_tokens as f64 / 1e6) * output;
    Some(cost)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG_JS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../web/agent_config.js");
    const USAGE_JS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../web/agent_usage.js");

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("linkr-agent-config-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn valid() -> StoredConfig {
        StoredConfig {
            provider: "openai-completions".into(),
            endpoint: "http://127.0.0.1:8080/v1/".into(),
            api_key: "secret".into(),
            model: "local-model".into(),
            context_window: 32768,
            max_tokens: 4096,
            reasoning: "off".into(),
            headers: BTreeMap::new(),
            price_input: 0.0,
            price_output: 0.0,
        }
    }

    /// The exact validation messages: web's `agent_settings.js` labels for the
    /// same throws, quoted verbatim by AGENT_SPEC §12.1 and `WEB_UX_SPEC.md:497`.
    /// The four that also exist as `tui::agent_settings` constants are compared
    /// against them, so this crate's two validation paths cannot drift apart.
    #[test]
    fn validation_messages_are_exact() {
        let mut stored = valid();
        stored.endpoint = "not a url".into();
        assert_eq!(
            validate_agent_config(&stored).unwrap_err(),
            "Enter an HTTP(S) API base URL without credentials, query parameters or a fragment."
        );

        let mut stored = valid();
        stored.endpoint = "ftp://host".into();
        assert_eq!(
            validate_agent_config(&stored).unwrap_err(),
            "Enter an HTTP(S) API base URL without credentials, query parameters or a fragment."
        );

        let mut stored = valid();
        stored.endpoint = "http://user:pass@host/v1".into();
        assert_eq!(
            validate_agent_config(&stored).unwrap_err(),
            "Enter an HTTP(S) API base URL without credentials, query parameters or a fragment."
        );

        let mut stored = valid();
        stored.endpoint = "http://host/v1?x=1".into();
        assert_eq!(
            validate_agent_config(&stored).unwrap_err(),
            "Enter an HTTP(S) API base URL without credentials, query parameters or a fragment."
        );

        let mut stored = valid();
        stored.model = "   ".into();
        assert_eq!(
            validate_agent_config(&stored).unwrap_err(),
            "Enter a model ID."
        );

        let mut stored = valid();
        stored.context_window = 999;
        assert_eq!(
            validate_agent_config(&stored).unwrap_err(),
            "Context window must be 0, or an integer between 1000 and 2000000."
        );
        stored.context_window = 2_000_001;
        assert_eq!(
            validate_agent_config(&stored).unwrap_err(),
            "Context window must be 0, or an integer between 1000 and 2000000."
        );

        let mut stored = valid();
        stored.max_tokens = 0; // 0 keeps the built-in default
        assert!(validate_agent_config(&stored).is_ok());
        stored.max_tokens = 100_001;
        assert_eq!(
            validate_agent_config(&stored).unwrap_err(),
            "Max output tokens must be 0, or an integer between 1 and 100000."
        );

        let mut stored = valid();
        stored.reasoning = "turbo".into();
        assert_eq!(
            validate_agent_config(&stored).unwrap_err(),
            "Choose a reasoning effort."
        );
    }

    /// The stored-record path and the TUI dialog are two validations of one
    /// record: a value the dialog accepts must never be rejected on load.
    #[test]
    fn stored_record_errors_match_the_settings_dialog() {
        use crate::tui::agent_settings::{
            ERR_CONTEXT_WINDOW, ERR_ENDPOINT, ERR_MAX_TOKENS, ERR_MODEL,
        };

        let mut stored = valid();
        stored.endpoint = "not a url".into();
        assert_eq!(validate_agent_config(&stored).unwrap_err(), ERR_ENDPOINT);

        let mut stored = valid();
        stored.model = String::new();
        assert_eq!(validate_agent_config(&stored).unwrap_err(), ERR_MODEL);

        let mut stored = valid();
        stored.context_window = 999;
        assert_eq!(
            validate_agent_config(&stored).unwrap_err(),
            ERR_CONTEXT_WINDOW
        );

        let mut stored = valid();
        stored.max_tokens = 100_001;
        assert_eq!(validate_agent_config(&stored).unwrap_err(), ERR_MAX_TOKENS);
    }

    #[test]
    fn normalization_follows_the_js_default() {
        let mut stored = valid();
        stored.provider = "unknown-protocol".into();
        stored.endpoint = "http://host/v1/".into();
        let config = validate_agent_config(&stored).unwrap();
        assert_eq!(config.provider, Provider::OpenAiCompat);
        assert_eq!(config.endpoint, "http://host/v1", "trailing slash dropped");
        assert_eq!(config.api_key.as_deref(), Some("secret"));

        let mut stored = valid();
        stored.provider = "anthropic-messages".into();
        stored.reasoning = String::new();
        stored.api_key = String::new();
        let config = validate_agent_config(&stored).unwrap();
        assert_eq!(config.provider, Provider::AnthropicMessages);
        assert_eq!(config.reasoning, "off");
        assert_eq!(config.api_key, None);
    }

    /// A blank "Max output tokens" (or "Context window") field is stored as `0`
    /// — "use the built-in default", exactly what the web form does with an
    /// empty input. Handing that `0` to the provider instead of the default
    /// made every assistant turn fail with
    /// `400 Invalid max_tokens value, the valid range of max_tokens is [1, 393216]`.
    #[test]
    fn a_stored_zero_becomes_the_builtin_default() {
        let mut stored = valid();
        stored.context_window = 0;
        stored.max_tokens = 0;
        let config = validate_agent_config(&stored).unwrap();
        assert_eq!(config.context_window, AGENT_DEFAULT_CONTEXT_WINDOW);
        assert_eq!(config.max_tokens, AGENT_DEFAULT_MAX_TOKENS);
        assert_eq!(AGENT_DEFAULT_MAX_TOKENS, 4096);

        // The other end of the range: anything the settings dialog accepts
        // (1000..100000) has to load, or the record cannot be used at all.
        let mut stored = valid();
        stored.context_window = 1000;
        stored.max_tokens = 100_000;
        let config = validate_agent_config(&stored).unwrap();
        assert_eq!(config.context_window, 1000);
        assert_eq!(config.max_tokens, 100_000);
    }

    #[test]
    fn endpoint_security_matrix() {
        assert_eq!(
            endpoint_security("http://127.0.0.1:8080/v1"),
            EndpointSecurity {
                plaintext: true,
                loopback: true,
                exposes_key: false
            }
        );
        assert_eq!(
            endpoint_security("http://localhost:11434"),
            EndpointSecurity {
                plaintext: true,
                loopback: true,
                exposes_key: false
            }
        );
        assert_eq!(
            endpoint_security("http://api.example.com"),
            EndpointSecurity {
                plaintext: true,
                loopback: false,
                exposes_key: true
            }
        );
        assert_eq!(
            endpoint_security("https://api.example.com"),
            EndpointSecurity {
                plaintext: false,
                loopback: false,
                exposes_key: false
            }
        );
        assert_eq!(endpoint_security("not a url"), EndpointSecurity::default());
    }

    #[test]
    fn header_lines_round_trip_and_reject_injection() {
        let text = "anthropic-dangerous-direct-browser-access: true\nX-Trace: abc";
        let headers = parse_agent_headers(text).unwrap();
        assert_eq!(headers.len(), 2);
        assert_eq!(format_agent_headers(&headers), text);
        assert_eq!(headers[0].0, "anthropic-dangerous-direct-browser-access");

        assert_eq!(parse_agent_headers("no colon").unwrap_err(), "headers");
        assert_eq!(
            parse_agent_headers(": value").unwrap_err(),
            "headers",
            "empty name"
        );
        assert_eq!(
            parse_agent_headers("X-A: a\u{7}b").unwrap_err(),
            "headers",
            "control character in a value"
        );
        let many: Vec<String> = (0..9).map(|i| format!("X-{i}: v")).collect();
        assert_eq!(
            parse_agent_headers(&many.join("\n")).unwrap_err(),
            "headers"
        );
    }

    #[test]
    fn config_round_trip_and_clear() {
        let dir = temp_dir("roundtrip");
        let stored = valid();
        let config = validate_agent_config(&stored).unwrap();
        let saved = save_config_to(&dir, &config).unwrap();
        assert_eq!(saved.model, "local-model");

        let raw = std::fs::read_to_string(dir.join("agent.json")).unwrap();
        assert!(raw.contains("\"apiKey\""), "storage shape: {raw}");
        assert!(raw.contains("\"contextWindow\""));

        let loaded = load_config_from(&dir).expect("round trip");
        assert_eq!(loaded.endpoint, "http://127.0.0.1:8080/v1");
        assert_eq!(loaded.provider, Provider::OpenAiCompat);

        clear_config_from(&dir).unwrap();
        assert!(load_config_from(&dir).is_none());
        clear_config_from(&dir).unwrap(); // idempotent
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_config_is_not_loaded() {
        let dir = temp_dir("invalid");
        std::fs::write(dir.join("agent.json"), "{ not json").unwrap();
        assert!(load_config_from(&dir).is_none());
        std::fs::write(dir.join("agent.json"), "{\"endpoint\":\"nope\"}").unwrap();
        assert!(load_config_from(&dir).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pricing_helpers_follow_agent_usage_js() {
        let source = std::fs::read_to_string(USAGE_JS).expect("agent_usage.js");
        assert!(source.contains("linkr-agent-pricing-v1"));
        assert_eq!(PRICING_KEY, "linkr-agent-pricing-v1");
        assert_eq!(parse_pricing(1.5, 2.0).unwrap(), (1.5, 2.0));
        assert_eq!(parse_pricing(0.0, 0.0).unwrap(), (0.0, 0.0));
        assert_eq!(parse_pricing(-1.0, 0.0).unwrap_err(), "pricing");
        assert_eq!(parse_pricing(100_001.0, 0.0).unwrap_err(), "pricing");

        assert_eq!(estimate_cost(1_000_000, 500_000, 2.0, 4.0), Some(4.0));
        assert_eq!(estimate_cost(10, 10, 0.0, 0.0), None);

        let dir = temp_dir("pricing");
        let saved = save_pricing_to(&dir, 3.0, 7.0).unwrap();
        assert_eq!(saved, (3.0, 7.0));
        assert_eq!(load_pricing_from(&dir), (3.0, 7.0));
        assert!(save_pricing_to(&dir, -1.0, 0.0).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tight_window_guard_matches_agent_config_js() {
        let source = std::fs::read_to_string(CONFIG_JS).expect("agent_config.js");
        assert!(source.contains("AGENT_FIXED_CONTEXT_TOKENS = 8000"));
        assert_eq!(minimum_useful_context_window(4096), 8000 + 4096 + 512);
        assert_eq!(minimum_useful_context_window(0), 8000 + 4096 + 512);
        assert!(is_context_window_tight(8192, 4096));
        assert!(!is_context_window_tight(0, 4096));
        assert!(!is_context_window_tight(32768, 4096));
        assert_eq!(AGENT_DEFAULT_CONTEXT_WINDOW, 32768);
        assert_eq!(AGENT_DEFAULT_MAX_TOKENS, 4096);
        assert!(source.contains("linkr-agent-model"));
        assert_eq!(AGENT_CONFIG_KEY, "linkr-agent-model");
    }
}
