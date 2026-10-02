//! AI configuration dialog (`section#agentSettings` of the web client).
//!
//! Storage: `dirs::config_dir()/linkr/agent.json`, camelCase exactly like the
//! `linkr-agent-model` localStorage record (WEB_UX_SPEC section 9.2). Every
//! validation and status string is the English literal of
//! `specs/WEB_UX_SPEC.md` section 7.4 / `web/agent_settings.js`, so the dialog
//! cannot drift from the browser.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use serde::{Deserialize, Serialize};

use super::state::{App, TextField};

/// Labels of the API protocol select (`AGENT_PROVIDERS`).
pub const PROVIDERS: [(&str, &str); 3] = [
    ("openai-completions", "OpenAI-compatible API"),
    ("anthropic-messages", "Anthropic Messages API"),
    ("google-generative-ai", "Google AI"),
];
/// Labels of the reasoning select (`AGENT_REASONING_LEVELS`).
pub const REASONING: [(&str, &str); 4] = [
    ("off", "Off"),
    ("low", "Low"),
    ("medium", "Medium"),
    ("high", "High"),
];

// Exact validation / status literals of WEB_UX_SPEC section 7.4.
pub const ERR_ENDPOINT: &str =
    "Enter an HTTP(S) API base URL without credentials, query parameters or a fragment.";
pub const ERR_MODEL: &str = "Enter a model ID.";
pub const ERR_PROVIDER: &str = "Choose an API protocol.";
pub const ERR_REASONING: &str = "Choose a reasoning effort.";
pub const ERR_CONTEXT_WINDOW: &str =
    "Context window must be 0, or an integer between 1000 and 2000000.";
pub const ERR_MAX_TOKENS: &str = "Max output tokens must be 0, or an integer between 1 and 100000.";
pub const ERR_PRICES: &str = "Prices must be numbers between 0 and 100000.";
pub const ERR_HEADERS: &str = "Invalid headers: use one `Name: value` per line, with names limited to letters, digits and hyphens.";
pub const SAVED: &str = "Configuration saved on this device.";
pub const CLEARED: &str = "AI configuration cleared from this device.";
pub const DIRTY: &str = "Changes have not been saved.";
pub const SAVE_ERROR: &str =
    "Save failed. Check that app / browser storage is allowed, then retry.";
pub const BUSY: &str =
    "Agent is running. Return to the conversation and stop it before editing configuration.";
pub const PLAINTEXT_WARNING: &str = "Warning: this endpoint uses plain http and is not loopback, so the API key and serial logs travel the network in cleartext. Use https, or host the service on localhost.";
pub const PLAINTEXT_CONFIRM: &str = "This endpoint uses plain http and is not loopback, so the API key will be sent in cleartext. Save anyway?";
pub const PLAINTEXT_BLOCKED: &str =
    "Save cancelled: a plaintext http endpoint needs confirmation before an API key is stored.";
pub const STORAGE_HINT: &str = "Configuration and API key are saved in this device's app / browser storage across reloads and restarts. Clear the configuration or app / site data to remove them.";
pub const NOTICE_HINT: &str = "When you ask a question, the assistant sends the requested serial logs to your configured model service.";
pub const HEADER_LIMIT: usize = 8;

/// The persisted record: camelCase like the JS storage shape.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StoredAgent {
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
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(rename = "priceInput")]
    pub price_input: f64,
    #[serde(rename = "priceOutput")]
    pub price_output: f64,
}

/// `dirs::config_dir()/linkr/agent.json`.
pub fn agent_settings_path() -> PathBuf {
    let mut path = dirs::config_dir().unwrap_or_default();
    path.push("linkr");
    path.push("agent.json");
    path
}

/// Read the stored record; a missing or broken file reads as "not configured".
pub fn load_stored() -> Option<StoredAgent> {
    let text = std::fs::read_to_string(agent_settings_path()).ok()?;
    serde_json::from_str::<StoredAgent>(&text).ok()
}

fn write_stored(stored: &StoredAgent) -> Result<(), String> {
    let path = agent_settings_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|_| SAVE_ERROR.to_string())?;
    }
    let json = serde_json::to_string_pretty(stored).map_err(|_| SAVE_ERROR.to_string())?;
    std::fs::write(&path, json).map_err(|_| SAVE_ERROR.to_string())
}

/// Endpoint security mirror of `endpointSecurity()` in `agent_config.js`.
pub fn endpoint_security(endpoint: &str) -> (bool, bool) {
    let lowered = endpoint.trim().to_ascii_lowercase();
    let (scheme, rest) = match lowered.split_once("://") {
        Some(pair) => pair,
        None => return (false, false),
    };
    let host = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .rsplit('@')
        .next()
        .unwrap_or("")
        .trim_start_matches('[')
        .trim_end_matches(']');
    let host = host.split(':').next().unwrap_or("");
    let loopback = host == "localhost"
        || host == "::1"
        || host.ends_with(".localhost")
        || host.starts_with("127.");
    let plaintext = scheme == "http";
    (plaintext, plaintext && !loopback)
}

/// Port of `parseAgentHeaders` with the shared `headers` error literal.
pub fn parse_headers(text: &str) -> Result<Vec<(String, String)>, String> {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    if lines.len() > HEADER_LIMIT {
        return Err(ERR_HEADERS.to_string());
    }
    let mut out = Vec::with_capacity(lines.len());
    for line in lines {
        let Some(index) = line.find(':') else {
            return Err(ERR_HEADERS.to_string());
        };
        if index == 0 {
            return Err(ERR_HEADERS.to_string());
        }
        let name = line[..index].trim();
        let value = line[index + 1..].trim();
        if name.is_empty()
            || name.len() > 64
            || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            || value.is_empty()
            || value.len() > 256
            || value.chars().any(|c| c.is_control())
        {
            return Err(ERR_HEADERS.to_string());
        }
        out.push((name.to_string(), value.to_string()));
    }
    Ok(out)
}

/// Port of `formatAgentHeaders`.
pub fn format_headers(headers: &[(String, String)]) -> String {
    headers
        .iter()
        .map(|(name, value)| format!("{name}: {value}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_number(text: &str, range: (u32, u32), message: &str) -> Result<u32, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed == "0" {
        return Ok(0);
    }
    match trimmed.parse::<u32>() {
        Ok(value) if value >= range.0 && value <= range.1 => Ok(value),
        _ => Err(message.to_string()),
    }
}

fn parse_price(text: &str) -> Result<f64, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(0.0);
    }
    match trimmed.parse::<f64>() {
        Ok(value) if (0.0..=100_000.0).contains(&value) => Ok(value),
        _ => Err(ERR_PRICES.to_string()),
    }
}

/// Selectable rows of the dialog, in render order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Endpoint,
    Model,
    ApiKey,
    Headers,
    Provider,
    Reasoning,
    ContextWindow,
    MaxTokens,
    PriceInput,
    PriceOutput,
    Save,
    Clear,
}

pub const FIELDS: [Field; 12] = [
    Field::Endpoint,
    Field::Model,
    Field::ApiKey,
    Field::Headers,
    Field::Provider,
    Field::Reasoning,
    Field::ContextWindow,
    Field::MaxTokens,
    Field::PriceInput,
    Field::PriceOutput,
    Field::Save,
    Field::Clear,
];

/// Editable state of one dialog instance.
#[derive(Debug, Clone, Default)]
pub struct AgentSettingsState {
    pub endpoint: TextField,
    pub model: TextField,
    pub api_key: TextField,
    pub headers: TextField,
    pub provider: usize,
    pub reasoning: usize,
    pub context_window: TextField,
    pub max_tokens: TextField,
    pub price_input: TextField,
    pub price_output: TextField,
    pub selection: usize,
    pub status: Option<String>,
    pub error: bool,
    pub dirty: bool,
    /// Plaintext consent is a two-step save, exactly like the web dialog.
    pub confirm_pending: bool,
}

impl AgentSettingsState {
    /// Open the dialog with the stored record (or empty fields).
    pub fn load() -> Self {
        let mut state = Self::default();
        if let Some(stored) = load_stored() {
            state.endpoint.set(stored.endpoint);
            state.model.set(stored.model);
            state.api_key.set(stored.api_key);
            state.headers.set(format_headers(
                &stored
                    .headers
                    .into_iter()
                    .collect::<Vec<(String, String)>>(),
            ));
            state.provider = PROVIDERS
                .iter()
                .position(|(id, _)| *id == stored.provider)
                .unwrap_or(0);
            state.reasoning = REASONING
                .iter()
                .position(|(id, _)| *id == stored.reasoning)
                .unwrap_or(0);
            if stored.context_window > 0 {
                state.context_window.set(stored.context_window.to_string());
            }
            if stored.max_tokens > 0 {
                state.max_tokens.set(stored.max_tokens.to_string());
            }
            if stored.price_input > 0.0 {
                state.price_input.set(format!("{}", stored.price_input));
            }
            if stored.price_output > 0.0 {
                state.price_output.set(format!("{}", stored.price_output));
            }
        }
        state
    }

    /// Build the record the form currently describes.
    pub fn to_stored(&self) -> Result<StoredAgent, String> {
        let endpoint = self.endpoint.text.trim().to_string();
        if endpoint.is_empty() {
            return Err(ERR_ENDPOINT.to_string());
        }
        let (scheme, rest) = endpoint.split_once("://").unwrap_or(("", ""));
        let allowed = matches!(scheme, "http" | "https");
        let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
        if !allowed || authority.contains('@') {
            return Err(ERR_ENDPOINT.to_string());
        }
        let model = self.model.text.trim().to_string();
        if model.is_empty() {
            return Err(ERR_MODEL.to_string());
        }
        let provider = PROVIDERS
            .get(self.provider)
            .map(|(id, _)| id.to_string())
            .ok_or_else(|| ERR_PROVIDER.to_string())?;
        let reasoning = REASONING
            .get(self.reasoning)
            .map(|(id, _)| id.to_string())
            .ok_or_else(|| ERR_REASONING.to_string())?;
        let context_window = parse_number(
            &self.context_window.text,
            (1000, 2_000_000),
            ERR_CONTEXT_WINDOW,
        )?;
        let max_tokens = parse_number(&self.max_tokens.text, (1, 100_000), ERR_MAX_TOKENS)?;
        let price_input = parse_price(&self.price_input.text)?;
        let price_output = parse_price(&self.price_output.text)?;
        let headers = parse_headers(&self.headers.text)?;
        Ok(StoredAgent {
            provider,
            endpoint,
            api_key: self.api_key.text.clone(),
            model,
            context_window,
            max_tokens,
            reasoning,
            headers: headers.into_iter().collect(),
            price_input,
            price_output,
        })
    }

    /// `endpointSecurity(...).exposesKey` for the current form value.
    pub fn exposes_key(&self) -> bool {
        endpoint_security(&self.endpoint.text).1 && !self.api_key.text.is_empty()
    }

    fn select(&mut self, next: usize) {
        self.selection = next.min(FIELDS.len() - 1);
    }
}

// --- keys --------------------------------------------------------------------

enum Act {
    Nothing,
    Close,
    Save,
    Clear,
}

fn field_of(state: &AgentSettingsState) -> Field {
    FIELDS[state.selection]
}

/// Apply one key to the dialog body. Returns the follow-up action for the
/// caller (which owns the whole [`App`]).
fn edit(state: &mut AgentSettingsState, key: KeyEvent) -> Act {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Esc => return Act::Close,
        KeyCode::Up | KeyCode::BackTab => {
            state.selection = state.selection.saturating_sub(1);
            return Act::Nothing;
        }
        KeyCode::Down | KeyCode::Tab => {
            state.select(state.selection + 1);
            return Act::Nothing;
        }
        KeyCode::Home => {
            state.selection = 0;
            return Act::Nothing;
        }
        KeyCode::End => {
            state.selection = FIELDS.len() - 1;
            return Act::Nothing;
        }
        _ => {}
    }

    let field = field_of(state);
    // Cycle the selects with Left/Right (and Enter), like a native combo box.
    match (field, key.code) {
        (Field::Provider, KeyCode::Left) => {
            state.provider = (state.provider + PROVIDERS.len() - 1) % PROVIDERS.len();
            state.dirty = true;
            return Act::Nothing;
        }
        (Field::Provider, KeyCode::Right | KeyCode::Enter) => {
            state.provider = (state.provider + 1) % PROVIDERS.len();
            state.dirty = true;
            return Act::Nothing;
        }
        (Field::Reasoning, KeyCode::Left) => {
            state.reasoning = (state.reasoning + REASONING.len() - 1) % REASONING.len();
            state.dirty = true;
            return Act::Nothing;
        }
        (Field::Reasoning, KeyCode::Right | KeyCode::Enter) => {
            state.reasoning = (state.reasoning + 1) % REASONING.len();
            state.dirty = true;
            return Act::Nothing;
        }
        (Field::Save, KeyCode::Enter) => return Act::Save,
        (Field::Clear, KeyCode::Enter) => return Act::Clear,
        _ => {}
    }

    macro_rules! edit_field {
        ($target:expr) => {{
            match key.code {
                KeyCode::Char(c) if !ctrl => $target.insert_char(c),
                KeyCode::Backspace => $target.backspace(),
                KeyCode::Delete => $target.delete(),
                KeyCode::Left => $target.left(),
                KeyCode::Right => $target.right(),
                KeyCode::Home => $target.home(),
                KeyCode::End => $target.end(),
                _ => {}
            }
            state.dirty = true;
        }};
    }
    match field {
        Field::Endpoint => edit_field!(state.endpoint),
        Field::Model => edit_field!(state.model),
        Field::ApiKey => edit_field!(state.api_key),
        Field::Headers => edit_field!(state.headers),
        Field::ContextWindow => edit_field!(state.context_window),
        Field::MaxTokens => edit_field!(state.max_tokens),
        Field::PriceInput => edit_field!(state.price_input),
        Field::PriceOutput => edit_field!(state.price_output),
        Field::Provider | Field::Reasoning | Field::Save | Field::Clear => {}
    }
    Act::Nothing
}

/// Key routing for the open AI configuration dialog.
pub fn handle_key(app: &mut App, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    if ctrl && key.code == KeyCode::Char('s') {
        save(app);
        return;
    }
    let action = match &mut app.dialog {
        Some(super::dialogs::Dialog::Settings(state)) => edit(state, key),
        _ => return,
    };
    match action {
        Act::Nothing => {}
        Act::Close => app.dialog = None,
        Act::Save => save(app),
        Act::Clear => clear(app),
    }
}

/// Validate the form and persist it (plaintext consent is a second save).
pub fn save(app: &mut App) {
    if app.assistant.busy {
        set_status(app, BUSY, true);
        return;
    }
    let stored = {
        let Some(super::dialogs::Dialog::Settings(state)) = &app.dialog else {
            return;
        };
        match state.to_stored() {
            Ok(stored) => stored,
            Err(message) => {
                set_status(app, &message, true);
                return;
            }
        }
    };
    // Plaintext consent: first save warns, second save stores.
    let (_, exposes) = endpoint_security(&stored.endpoint);
    if exposes && !stored.api_key.is_empty() {
        let confirmed = matches!(
            &app.dialog,
            Some(super::dialogs::Dialog::Settings(state)) if state.confirm_pending
        );
        if !confirmed {
            set_status(app, PLAINTEXT_CONFIRM, true);
            if let Some(super::dialogs::Dialog::Settings(state)) = &mut app.dialog {
                state.confirm_pending = true;
            }
            return;
        }
    }
    match write_stored(&stored) {
        Ok(()) => {
            if let Some(super::dialogs::Dialog::Settings(state)) = &mut app.dialog {
                state.dirty = false;
                state.confirm_pending = false;
            }
            set_status(app, SAVED, false);
            app.toast(crate::event::NoticeLevel::Info, SAVED);
        }
        Err(message) => set_status(app, &message, true),
    }
}

/// Forget the stored record (web `clearAgentConfig`).
pub fn clear(app: &mut App) {
    if app.assistant.busy {
        set_status(app, BUSY, true);
        return;
    }
    let path = agent_settings_path();
    let outcome = if path.exists() {
        std::fs::remove_file(&path).map_err(|_| CLEAR_ERROR.to_string())
    } else {
        Ok(())
    };
    match outcome {
        Ok(()) => {
            if let Some(super::dialogs::Dialog::Settings(state)) = &mut app.dialog {
                *state = AgentSettingsState::default();
                state.dirty = false;
            }
            set_status(app, CLEARED, false);
            app.toast(crate::event::NoticeLevel::Info, CLEARED);
        }
        Err(message) => set_status(app, &message, true),
    }
}

const CLEAR_ERROR: &str = "Clear failed. The saved configuration is still present; retry.";

fn set_status(app: &mut App, status: &str, error: bool) {
    if let Some(super::dialogs::Dialog::Settings(state)) = &mut app.dialog {
        state.status = Some(status.to_string());
        state.error = error;
    }
}

// --- rendering ---------------------------------------------------------------

fn label(text: &str, selected: bool) -> Span<'static> {
    Span::styled(
        format!("{text:<18}"),
        Style::default().fg(if selected {
            Color::Yellow
        } else {
            Color::DarkGray
        }),
    )
}

fn value(text: String, selected: bool, masked: bool) -> Span<'static> {
    let shown = if masked {
        "*".repeat(text.chars().count().min(32))
    } else if text.is_empty() {
        "–".to_string()
    } else {
        text
    };
    Span::styled(
        shown,
        Style::default()
            .fg(if selected { Color::White } else { Color::Gray })
            .add_modifier(if selected {
                Modifier::BOLD
            } else {
                Modifier::empty()
            }),
    )
}

/// Body of the AI configuration dialog.
pub fn render_lines(state: &AgentSettingsState, width: u16) -> Vec<Line<'static>> {
    let width = width.max(20) as usize;
    let sel = |field: Field| FIELDS[state.selection] == field;
    let mut lines: Vec<Line<'static>> = Vec::new();

    let row = |field: Field, name: &str, text: String, masked: bool, lines: &mut Vec<Line>| {
        let marker = if sel(field) { "▸ " } else { "  " };
        let mut spans = vec![
            Span::styled(marker, Style::default().fg(Color::Yellow)),
            label(name, sel(field)),
        ];
        let tail = width.saturating_sub(21);
        if text.chars().count() > tail {
            let clipped: String = text.chars().take(tail.saturating_sub(1)).collect();
            spans.push(value(format!("{clipped}…"), sel(field), masked));
        } else {
            spans.push(value(text, sel(field), masked));
        }
        lines.push(Line::from(spans));
    };

    row(
        Field::Endpoint,
        "API base URL",
        state.endpoint.text.clone(),
        false,
        &mut lines,
    );
    row(
        Field::Model,
        "Model ID",
        state.model.text.clone(),
        false,
        &mut lines,
    );
    row(
        Field::ApiKey,
        "API key",
        state.api_key.text.clone(),
        true,
        &mut lines,
    );
    let headers_hint = if sel(Field::Headers) {
        "  one `Name: value` per line"
    } else {
        "  extra headers"
    };
    row(
        Field::Headers,
        headers_hint,
        state.headers.text.clone().replace('\n', " ⏎ "),
        false,
        &mut lines,
    );
    row(
        Field::Provider,
        "API protocol",
        PROVIDERS[state.provider].1.to_string(),
        false,
        &mut lines,
    );
    row(
        Field::Reasoning,
        "Reasoning effort",
        REASONING[state.reasoning].1.to_string(),
        false,
        &mut lines,
    );
    row(
        Field::ContextWindow,
        "Context window",
        state.context_window.text.clone(),
        false,
        &mut lines,
    );
    row(
        Field::MaxTokens,
        "Max output tokens",
        state.max_tokens.text.clone(),
        false,
        &mut lines,
    );
    row(
        Field::PriceInput,
        "Price input / 1M",
        state.price_input.text.clone(),
        false,
        &mut lines,
    );
    row(
        Field::PriceOutput,
        "Price output / 1M",
        state.price_output.text.clone(),
        false,
        &mut lines,
    );

    lines.push(Line::from(""));
    for (field, name) in [
        (Field::Save, "Save configuration"),
        (Field::Clear, "Clear configuration"),
    ] {
        let selected = sel(field);
        lines.push(Line::from(vec![
            Span::styled(
                if selected { "▸ " } else { "  " },
                Style::default().fg(Color::Yellow),
            ),
            Span::styled(
                name.to_string(),
                Style::default()
                    .fg(if field == Field::Save {
                        Color::Green
                    } else {
                        Color::Red
                    })
                    .add_modifier(if selected {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            ),
        ]));
    }

    if let Some(status) = &state.status {
        let color = if state.error {
            Color::LightRed
        } else {
            Color::LightGreen
        };
        for line in super::dialogs::wrap_line(status, width as u16) {
            lines.push(Line::from(
                line.spans
                    .into_iter()
                    .map(|span| span.style(Style::default().fg(color)))
                    .collect::<Vec<_>>(),
            ));
        }
    }
    if state.dirty && state.status.is_none() {
        lines.push(Line::from(Span::styled(
            DIRTY.to_string(),
            Style::default().fg(Color::DarkGray),
        )));
    }
    if state.exposes_key() {
        for line in super::dialogs::wrap_line(PLAINTEXT_WARNING, width as u16) {
            lines.push(Line::from(
                line.spans
                    .into_iter()
                    .map(|span| span.style(Style::default().fg(Color::LightYellow)))
                    .collect::<Vec<_>>(),
            ));
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        NOTICE_HINT.to_string(),
        Style::default().fg(Color::DarkGray),
    )));
    lines.push(Line::from(Span::styled(
        "↑/↓ select · ←/→ cycle · Ctrl+S save · Esc close",
        Style::default().fg(Color::DarkGray),
    )));
    lines
}

/// Runtime config for `agent::spawn`, or `None` when nothing is stored yet.
pub fn runtime_config() -> Option<crate::agent::AgentConfig> {
    let stored = load_stored()?;
    if stored.endpoint.trim().is_empty() || stored.model.trim().is_empty() {
        return None;
    }
    let provider = match stored.provider.as_str() {
        "anthropic-messages" => crate::agent::Provider::AnthropicMessages,
        "google-generative-ai" => crate::agent::Provider::GoogleGemini,
        _ => crate::agent::Provider::OpenAiCompat,
    };
    Some(crate::agent::AgentConfig {
        endpoint: stored.endpoint,
        model: stored.model,
        api_key: if stored.api_key.is_empty() {
            None
        } else {
            Some(stored.api_key)
        },
        provider,
        reasoning: stored.reasoning,
        extra_headers: stored.headers.into_iter().collect(),
        context_window: stored.context_window,
        max_tokens: stored.max_tokens,
        price_input: stored.price_input,
        price_output: stored.price_output,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with(endpoint: &str, model: &str) -> AgentSettingsState {
        let mut state = AgentSettingsState::default();
        state.endpoint.set(endpoint);
        state.model.set(model);
        state
    }

    #[test]
    fn validation_messages_match_the_web_literals() {
        let mut state = AgentSettingsState::default();
        assert_eq!(state.to_stored().unwrap_err(), ERR_ENDPOINT);

        state = state_with("", "gpt-4o");
        assert_eq!(state.to_stored().unwrap_err(), ERR_ENDPOINT);

        state = state_with("ftp://api.example.com", "gpt-4o");
        assert_eq!(state.to_stored().unwrap_err(), ERR_ENDPOINT);

        state = state_with("https://user:pass@example.com/v1", "gpt-4o");
        assert_eq!(state.to_stored().unwrap_err(), ERR_ENDPOINT);

        state = state_with("https://api.example.com/v1", "  ");
        assert_eq!(state.to_stored().unwrap_err(), ERR_MODEL);

        state.context_window.set("999".to_string());
        state = state_with("https://api.example.com/v1", "m");
        state.context_window.set("999".to_string());
        assert_eq!(state.to_stored().unwrap_err(), ERR_CONTEXT_WINDOW);

        state.max_tokens.set("0".to_string());
        state.context_window.set("2000001".to_string());
        assert_eq!(state.to_stored().unwrap_err(), ERR_CONTEXT_WINDOW);

        state.context_window.clear();
        state.max_tokens.set("100001".to_string());
        assert_eq!(state.to_stored().unwrap_err(), ERR_MAX_TOKENS);

        state.max_tokens.clear();
        state.price_input.set("100001".to_string());
        assert_eq!(state.to_stored().unwrap_err(), ERR_PRICES);

        state.price_input.clear();
        state.headers.set("bad header".to_string());
        assert_eq!(state.to_stored().unwrap_err(), ERR_HEADERS);
    }

    #[test]
    fn valid_record_round_trips_with_camel_case_keys() {
        let mut state = state_with("https://api.example.com/v1", "gpt-4o");
        state.api_key.set("sk-test".to_string());
        state.provider = 1;
        state.reasoning = 2;
        state.context_window.set("8192".to_string());
        state.max_tokens.set("1024".to_string());
        state.price_input.set("1.25".to_string());
        state
            .headers
            .set("anthropic-dangerous-direct-browser-access: true".to_string());

        let stored = state.to_stored().unwrap();
        assert_eq!(stored.provider, "anthropic-messages");
        assert_eq!(stored.reasoning, "medium");
        assert_eq!(stored.context_window, 8192);
        assert_eq!(stored.headers.len(), 1);

        let json = serde_json::to_string(&stored).unwrap();
        for key in [
            "\"apiKey\"",
            "\"contextWindow\"",
            "\"maxTokens\"",
            "\"priceInput\"",
            "\"priceOutput\"",
        ] {
            assert!(json.contains(key), "missing camelCase key {key} in {json}");
        }
        let back: StoredAgent = serde_json::from_str(&json).unwrap();
        assert_eq!(back, stored);
    }

    #[test]
    fn header_rules_match_the_browser() {
        assert_eq!(
            parse_headers("a: 1\nb: 2").unwrap(),
            vec![
                ("a".to_string(), "1".to_string()),
                ("b".to_string(), "2".to_string())
            ]
        );
        assert_eq!(parse_headers("no colon").unwrap_err(), ERR_HEADERS);
        assert_eq!(parse_headers("bad name: v").unwrap_err(), ERR_HEADERS);
        assert_eq!(parse_headers(": novalue").unwrap_err(), ERR_HEADERS);
        let many = (0..9)
            .map(|i| format!("h{i}: v"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(parse_headers(&many).unwrap_err(), ERR_HEADERS);
        assert_eq!(parse_headers("h: ok").unwrap().len(), 1);
    }

    #[test]
    fn plaintext_detection_matches_endpoint_security() {
        assert_eq!(endpoint_security("http://127.0.0.1:8080"), (true, false));
        assert_eq!(endpoint_security("http://localhost/v1"), (true, false));
        assert_eq!(endpoint_security("http://api.example.com"), (true, true));
        assert_eq!(endpoint_security("https://api.example.com"), (false, false));
        assert_eq!(endpoint_security("not a url"), (false, false));
    }

    #[test]
    fn number_ranges_follow_the_spec() {
        assert_eq!(parse_number("", (1000, 2_000_000), "x").unwrap(), 0);
        assert_eq!(parse_number("0", (1000, 2_000_000), "x").unwrap(), 0);
        assert_eq!(
            parse_number("32768", (1000, 2_000_000), "x").unwrap(),
            32768
        );
        assert!(parse_number("999", (1000, 2_000_000), ERR_CONTEXT_WINDOW).is_err());
        assert!(parse_number("-5", (1, 100_000), ERR_MAX_TOKENS).is_err());
        assert!(parse_number("abc", (1, 100_000), ERR_MAX_TOKENS).is_err());
        assert_eq!(parse_price("").unwrap(), 0.0);
        assert_eq!(parse_price("0.01").unwrap(), 0.01);
        assert!(parse_price("100001").is_err());
    }
}
