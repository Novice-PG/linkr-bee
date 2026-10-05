//! AI configuration dialog (`section#agentSettings` of the web client).
//!
//! Storage: `dirs::config_dir()/linkr/agent.json`, camelCase exactly like the
//! `linkr-agent-model` localStorage record (WEB_UX_SPEC section 9.2). Every
//! validation and status string is the English literal of
//! `specs/WEB_UX_SPEC.md` section 7.4 / `web/agent_settings.js`, so the dialog
//! cannot drift from the browser, and its 中文 half sits next to it in the
//! same `strings!` table (WEB_UX_SPEC section 9: `linkr-lang`). The English
//! literals stay reachable as `ERR_*` / `SAVED` / `BUSY` … for parity callers.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use serde::{Deserialize, Serialize};

use super::i18n::{strings, t, Entry, Lang};
use super::state::{App, TextField};

/// Labels of the API protocol select (`AGENT_PROVIDERS`). These are protocol
/// / product names, so they stay verbatim in both languages.
pub const PROVIDERS: [(&str, &str); 3] = [
    ("openai-completions", "OpenAI-compatible API"),
    ("anthropic-messages", "Anthropic Messages API"),
    ("google-generative-ai", "Google AI"),
];

strings! {
    // Exact validation / status literals of WEB_UX_SPEC section 7.4.
    ASST_ERR_ENDPOINT => "Enter an HTTP(S) API base URL without credentials, query parameters or a fragment.",
        "请输入不含凭据、查询参数和片段的 HTTP(S) API 基础 URL。";
    ASST_ERR_MODEL => "Enter a model ID.", "请输入模型 ID。";
    ASST_ERR_PROVIDER => "Choose an API protocol.", "请选择 API 协议。";
    ASST_ERR_REASONING => "Choose a reasoning effort.", "请选择推理力度。";
    ASST_ERR_CONTEXT_WINDOW => "Context window must be 0, or an integer between 1000 and 2000000.",
        "上下文窗口必须为 0，或 1000 到 2000000 之间的整数。";
    ASST_ERR_MAX_TOKENS => "Max output tokens must be 0, or an integer between 1 and 100000.",
        "最大输出 token 必须为 0，或 1 到 100000 之间的整数。";
    ASST_ERR_PRICES => "Prices must be numbers between 0 and 100000.",
        "价格必须是 0 到 100000 之间的数字。";
    ASST_ERR_HEADERS => "Invalid headers: use one `Name: value` per line, with names limited to letters, digits and hyphens.",
        "请求头无效：每行一条 `Name: value`，名称仅限字母、数字和连字符。";
    ASST_SAVED => "Configuration saved on this device.", "配置已保存到本设备。";
    ASST_CLEARED => "AI configuration cleared from this device.", "已从本设备清除 AI 配置。";
    ASST_DIRTY => "Changes have not been saved.", "更改尚未保存。";
    ASST_SAVE_ERROR => "Save failed. Check that app / browser storage is allowed, then retry.",
        "保存失败。请确认应用 / 浏览器存储未被禁用，然后重试。";
    ASST_CLEAR_ERROR => "Clear failed. The saved configuration is still present; retry.",
        "清除失败。已保存的配置仍然存在，请重试。";
    ASST_BUSY => "Agent is running. Return to the conversation and stop it before editing configuration.",
        "助手正在运行。请返回对话并停止后再修改配置。";
    ASST_PLAINTEXT_WARNING => "Warning: this endpoint uses plain http and is not loopback, so the API key and serial logs travel the network in cleartext. Use https, or host the service on localhost.",
        "警告：该端点使用明文 http 且非本地回环，API 密钥与串口日志会以明文经网络传输。请改用 https，或把服务部署在 localhost。";
    ASST_PLAINTEXT_CONFIRM => "This endpoint uses plain http and is not loopback, so the API key will be sent in cleartext. Save anyway?",
        "该端点使用明文 http 且非本地回环，API 密钥将以明文发送。仍要保存吗？";
    ASST_PLAINTEXT_BLOCKED => "Save cancelled: a plaintext http endpoint needs confirmation before an API key is stored.",
        "已取消保存：明文 http 端点在存储 API 密钥前需要确认。";
    ASST_STORAGE_HINT => "Configuration and API key are saved in this device's app / browser storage across reloads and restarts. Clear the configuration or app / site data to remove them.",
        "配置与 API 密钥保存在本设备的应用 / 浏览器存储中，刷新与重启后仍在。清除配置或应用 / 站点数据即可删除。";
    ASST_NOTICE_HINT => "When you ask a question, the assistant sends the requested serial logs to your configured model service.",
        "当你提问时，助手会把所需的串口日志发送到你配置的模型服务。";

    // Reasoning select labels.
    ASST_REASONING_OFF => "Off", "关闭";
    ASST_REASONING_LOW => "Low", "低";
    ASST_REASONING_MEDIUM => "Medium", "中";
    ASST_REASONING_HIGH => "High", "高";

    // Field labels.
    ASST_LABEL_ENDPOINT => "API base URL", "API 基础 URL";
    ASST_LABEL_MODEL => "Model ID", "模型 ID";
    ASST_LABEL_API_KEY => "API key", "API 密钥";
    ASST_HEADERS_EXTRA => "  extra headers", "  额外请求头";
    ASST_HEADERS_ONE_PER_LINE => "  one `Name: value` per line", "  每行一条 `Name: value`";
    ASST_LABEL_PROVIDER => "API protocol", "API 协议";
    ASST_LABEL_REASONING => "Reasoning effort", "推理力度";
    ASST_LABEL_CONTEXT => "Context window", "上下文窗口";
    ASST_LABEL_MAX_TOKENS => "Max output tokens", "最大输出 token";
    ASST_LABEL_PRICE_IN => "Price input / 1M", "输入价格 / 1M";
    ASST_LABEL_PRICE_OUT => "Price output / 1M", "输出价格 / 1M";
    ASST_ACTION_SAVE => "Save configuration", "保存配置";
    ASST_ACTION_CLEAR => "Clear configuration", "清除配置";
    ASST_KEYS_HINT => "↑/↓ select · ←/→ cycle · Ctrl+S save · Esc close",
        "↑/↓ 选择 · ←/→ 切换 · Ctrl+S 保存 · Esc 关闭";
}

/// English half of the messages above: the web suite pins these byte for
/// byte, so tests and parity callers keep reading them from here.
pub const ERR_ENDPOINT: &str = ASST_ERR_ENDPOINT[0];
pub const ERR_MODEL: &str = ASST_ERR_MODEL[0];
pub const ERR_PROVIDER: &str = ASST_ERR_PROVIDER[0];
pub const ERR_REASONING: &str = ASST_ERR_REASONING[0];
pub const ERR_CONTEXT_WINDOW: &str = ASST_ERR_CONTEXT_WINDOW[0];
pub const ERR_MAX_TOKENS: &str = ASST_ERR_MAX_TOKENS[0];
pub const ERR_PRICES: &str = ASST_ERR_PRICES[0];
pub const ERR_HEADERS: &str = ASST_ERR_HEADERS[0];
pub const SAVED: &str = ASST_SAVED[0];
pub const CLEARED: &str = ASST_CLEARED[0];
pub const DIRTY: &str = ASST_DIRTY[0];
pub const SAVE_ERROR: &str = ASST_SAVE_ERROR[0];
pub const BUSY: &str = ASST_BUSY[0];
pub const PLAINTEXT_WARNING: &str = ASST_PLAINTEXT_WARNING[0];
pub const PLAINTEXT_CONFIRM: &str = ASST_PLAINTEXT_CONFIRM[0];
pub const PLAINTEXT_BLOCKED: &str = ASST_PLAINTEXT_BLOCKED[0];
pub const STORAGE_HINT: &str = ASST_STORAGE_HINT[0];
pub const NOTICE_HINT: &str = ASST_NOTICE_HINT[0];

/// Labels of the reasoning select (`AGENT_REASONING_LEVELS`).
pub const REASONING: [(&str, Entry); 4] = [
    ("off", ASST_REASONING_OFF),
    ("low", ASST_REASONING_LOW),
    ("medium", ASST_REASONING_MEDIUM),
    ("high", ASST_REASONING_HIGH),
];
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

fn write_stored(stored: &StoredAgent, lang: Lang) -> Result<(), String> {
    let failed = || t(ASST_SAVE_ERROR, lang).to_string();
    let path = agent_settings_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|_| failed())?;
    }
    let json = serde_json::to_string_pretty(stored).map_err(|_| failed())?;
    std::fs::write(&path, json).map_err(|_| failed())
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
pub fn parse_headers(text: &str, lang: Lang) -> Result<Vec<(String, String)>, String> {
    let invalid = || t(ASST_ERR_HEADERS, lang).to_string();
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    if lines.len() > HEADER_LIMIT {
        return Err(invalid());
    }
    let mut out = Vec::with_capacity(lines.len());
    for line in lines {
        let Some(index) = line.find(':') else {
            return Err(invalid());
        };
        if index == 0 {
            return Err(invalid());
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
            return Err(invalid());
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

fn parse_price(text: &str, lang: Lang) -> Result<f64, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(0.0);
    }
    match trimmed.parse::<f64>() {
        Ok(value) if (0.0..=100_000.0).contains(&value) => Ok(value),
        _ => Err(t(ASST_ERR_PRICES, lang).to_string()),
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
    pub fn to_stored(&self, lang: Lang) -> Result<StoredAgent, String> {
        let endpoint = self.endpoint.text.trim().to_string();
        if endpoint.is_empty() {
            return Err(t(ASST_ERR_ENDPOINT, lang).to_string());
        }
        let (scheme, rest) = endpoint.split_once("://").unwrap_or(("", ""));
        let allowed = matches!(scheme, "http" | "https");
        let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
        if !allowed || authority.contains('@') {
            return Err(t(ASST_ERR_ENDPOINT, lang).to_string());
        }
        let model = self.model.text.trim().to_string();
        if model.is_empty() {
            return Err(t(ASST_ERR_MODEL, lang).to_string());
        }
        let provider = PROVIDERS
            .get(self.provider)
            .map(|(id, _)| id.to_string())
            .ok_or_else(|| t(ASST_ERR_PROVIDER, lang).to_string())?;
        let reasoning = REASONING
            .get(self.reasoning)
            .map(|(id, _)| id.to_string())
            .ok_or_else(|| t(ASST_ERR_REASONING, lang).to_string())?;
        let context_window = parse_number(
            &self.context_window.text,
            (1000, 2_000_000),
            t(ASST_ERR_CONTEXT_WINDOW, lang),
        )?;
        let max_tokens = parse_number(
            &self.max_tokens.text,
            (1, 100_000),
            t(ASST_ERR_MAX_TOKENS, lang),
        )?;
        let price_input = parse_price(&self.price_input.text, lang)?;
        let price_output = parse_price(&self.price_output.text, lang)?;
        let headers = parse_headers(&self.headers.text, lang)?;
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
    let lang = app.lang();
    if app.assistant.busy {
        set_status(app, t(ASST_BUSY, lang), true);
        return;
    }
    let stored = {
        let Some(super::dialogs::Dialog::Settings(state)) = &app.dialog else {
            return;
        };
        match state.to_stored(lang) {
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
            set_status(app, t(ASST_PLAINTEXT_CONFIRM, lang), true);
            if let Some(super::dialogs::Dialog::Settings(state)) = &mut app.dialog {
                state.confirm_pending = true;
            }
            return;
        }
    }
    match write_stored(&stored, lang) {
        Ok(()) => {
            if let Some(super::dialogs::Dialog::Settings(state)) = &mut app.dialog {
                state.dirty = false;
                state.confirm_pending = false;
            }
            set_status(app, t(ASST_SAVED, lang), false);
            app.toast(crate::event::NoticeLevel::Info, t(ASST_SAVED, lang));
        }
        Err(message) => set_status(app, &message, true),
    }
}

/// Forget the stored record (web `clearAgentConfig`).
pub fn clear(app: &mut App) {
    let lang = app.lang();
    if app.assistant.busy {
        set_status(app, t(ASST_BUSY, lang), true);
        return;
    }
    let path = agent_settings_path();
    let outcome = if path.exists() {
        std::fs::remove_file(&path).map_err(|_| t(ASST_CLEAR_ERROR, lang).to_string())
    } else {
        Ok(())
    };
    match outcome {
        Ok(()) => {
            if let Some(super::dialogs::Dialog::Settings(state)) = &mut app.dialog {
                *state = AgentSettingsState::default();
                state.dirty = false;
            }
            set_status(app, t(ASST_CLEARED, lang), false);
            app.toast(crate::event::NoticeLevel::Info, t(ASST_CLEARED, lang));
        }
        Err(message) => set_status(app, &message, true),
    }
}

fn set_status(app: &mut App, status: &str, error: bool) {
    if let Some(super::dialogs::Dialog::Settings(state)) = &mut app.dialog {
        state.status = Some(status.to_string());
        state.error = error;
    }
}

// --- rendering ---------------------------------------------------------------

fn label(text: &str, selected: bool) -> Span<'static> {
    // Padded by display width: a CJK glyph is two columns, and without this
    // the translated labels would push the value column out of line.
    let pad = 18usize.saturating_sub(unicode_width::UnicodeWidthStr::width(text));
    Span::styled(
        format!("{text}{}", " ".repeat(pad)),
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
pub fn render_lines(state: &AgentSettingsState, width: u16, lang: Lang) -> Vec<Line<'static>> {
    let width = width.max(20) as usize;
    let sel = |field: Field| FIELDS[state.selection] == field;
    let mut lines: Vec<Line<'static>> = Vec::new();

    let row = |field: Field, name: &str, text: String, masked: bool, lines: &mut Vec<Line>| {
        let marker = if sel(field) { "▸ " } else { "  " };
        let mut spans = vec![
            Span::styled(marker, Style::default().fg(Color::Yellow)),
            label(name, sel(field)),
        ];
        // marker (2 columns) + label (padded to 18) + one column of air, so
        // the value gets the rest. Measured in columns, never in characters:
        // a CJK value is two columns wide, and the character count ran every
        // such row one column over the dialog, wrapping it.
        let tail = width.saturating_sub(21);
        spans.push(value(
            super::dialogs::clip_columns(&text, tail),
            sel(field),
            masked,
        ));
        lines.push(Line::from(spans));
    };

    row(
        Field::Endpoint,
        t(ASST_LABEL_ENDPOINT, lang),
        state.endpoint.text.clone(),
        false,
        &mut lines,
    );
    row(
        Field::Model,
        t(ASST_LABEL_MODEL, lang),
        state.model.text.clone(),
        false,
        &mut lines,
    );
    row(
        Field::ApiKey,
        t(ASST_LABEL_API_KEY, lang),
        state.api_key.text.clone(),
        true,
        &mut lines,
    );
    let headers_hint = if sel(Field::Headers) {
        t(ASST_HEADERS_ONE_PER_LINE, lang)
    } else {
        t(ASST_HEADERS_EXTRA, lang)
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
        t(ASST_LABEL_PROVIDER, lang),
        PROVIDERS[state.provider].1.to_string(),
        false,
        &mut lines,
    );
    row(
        Field::Reasoning,
        t(ASST_LABEL_REASONING, lang),
        t(REASONING[state.reasoning].1, lang).to_string(),
        false,
        &mut lines,
    );
    row(
        Field::ContextWindow,
        t(ASST_LABEL_CONTEXT, lang),
        state.context_window.text.clone(),
        false,
        &mut lines,
    );
    row(
        Field::MaxTokens,
        t(ASST_LABEL_MAX_TOKENS, lang),
        state.max_tokens.text.clone(),
        false,
        &mut lines,
    );
    row(
        Field::PriceInput,
        t(ASST_LABEL_PRICE_IN, lang),
        state.price_input.text.clone(),
        false,
        &mut lines,
    );
    row(
        Field::PriceOutput,
        t(ASST_LABEL_PRICE_OUT, lang),
        state.price_output.text.clone(),
        false,
        &mut lines,
    );

    lines.push(Line::from(""));
    for (field, name) in [
        (Field::Save, t(ASST_ACTION_SAVE, lang)),
        (Field::Clear, t(ASST_ACTION_CLEAR, lang)),
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
            t(ASST_DIRTY, lang),
            Style::default().fg(Color::DarkGray),
        )));
    }
    if state.exposes_key() {
        for line in super::dialogs::wrap_line(t(ASST_PLAINTEXT_WARNING, lang), width as u16) {
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
        t(ASST_NOTICE_HINT, lang),
        Style::default().fg(Color::DarkGray),
    )));
    lines.push(Line::from(Span::styled(
        t(ASST_KEYS_HINT, lang),
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
        assert_eq!(state.to_stored(Lang::En).unwrap_err(), ERR_ENDPOINT);

        state = state_with("", "gpt-4o");
        assert_eq!(state.to_stored(Lang::En).unwrap_err(), ERR_ENDPOINT);

        state = state_with("ftp://api.example.com", "gpt-4o");
        assert_eq!(state.to_stored(Lang::En).unwrap_err(), ERR_ENDPOINT);

        state = state_with("https://user:pass@example.com/v1", "gpt-4o");
        assert_eq!(state.to_stored(Lang::En).unwrap_err(), ERR_ENDPOINT);

        state = state_with("https://api.example.com/v1", "  ");
        assert_eq!(state.to_stored(Lang::En).unwrap_err(), ERR_MODEL);

        state.context_window.set("999".to_string());
        state = state_with("https://api.example.com/v1", "m");
        state.context_window.set("999".to_string());
        assert_eq!(state.to_stored(Lang::En).unwrap_err(), ERR_CONTEXT_WINDOW);

        state.max_tokens.set("0".to_string());
        state.context_window.set("2000001".to_string());
        assert_eq!(state.to_stored(Lang::En).unwrap_err(), ERR_CONTEXT_WINDOW);

        state.context_window.clear();
        state.max_tokens.set("100001".to_string());
        assert_eq!(state.to_stored(Lang::En).unwrap_err(), ERR_MAX_TOKENS);

        state.max_tokens.clear();
        state.price_input.set("100001".to_string());
        assert_eq!(state.to_stored(Lang::En).unwrap_err(), ERR_PRICES);

        state.price_input.clear();
        state.headers.set("bad header".to_string());
        assert_eq!(state.to_stored(Lang::En).unwrap_err(), ERR_HEADERS);
    }

    /// The same form reads back in Chinese, and the English half still
    /// matches the web literal it was ported from.
    #[test]
    fn validation_messages_follow_the_language() {
        let mut state = AgentSettingsState::default();
        assert_eq!(
            state.to_stored(Lang::Zh).unwrap_err(),
            t(ASST_ERR_ENDPOINT, Lang::Zh)
        );
        state = state_with("https://api.example.com/v1", "  ");
        assert_eq!(
            state.to_stored(Lang::Zh).unwrap_err(),
            t(ASST_ERR_MODEL, Lang::Zh)
        );
        state.headers.set("bad header".to_string());
        assert_eq!(
            parse_headers(&state.headers.text, Lang::Zh).unwrap_err(),
            t(ASST_ERR_HEADERS, Lang::Zh)
        );
        state = state_with("https://api.example.com/v1", "gpt-4o");
        state.price_input.set("100001".to_string());
        assert_eq!(
            state.to_stored(Lang::Zh).unwrap_err(),
            t(ASST_ERR_PRICES, Lang::Zh)
        );
        assert_eq!(t(ASST_SAVED, Lang::Zh), "配置已保存到本设备。");
        // The English halves stay byte for byte the web literals.
        assert_eq!(ERR_HEADERS, ASST_ERR_HEADERS[0]);
        assert_eq!(BUSY, ASST_BUSY[0]);
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

        let stored = state.to_stored(Lang::En).unwrap();
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
            parse_headers("a: 1\nb: 2", Lang::En).unwrap(),
            vec![
                ("a".to_string(), "1".to_string()),
                ("b".to_string(), "2".to_string())
            ]
        );
        assert_eq!(
            parse_headers("no colon", Lang::En).unwrap_err(),
            ERR_HEADERS
        );
        assert_eq!(
            parse_headers("bad name: v", Lang::En).unwrap_err(),
            ERR_HEADERS
        );
        assert_eq!(
            parse_headers(": novalue", Lang::En).unwrap_err(),
            ERR_HEADERS
        );
        let many = (0..9)
            .map(|i| format!("h{i}: v"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(parse_headers(&many, Lang::En).unwrap_err(), ERR_HEADERS);
        assert_eq!(parse_headers("h: ok", Lang::En).unwrap().len(), 1);
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
        assert_eq!(parse_price("", Lang::En).unwrap(), 0.0);
        assert_eq!(parse_price("0.01", Lang::En).unwrap(), 0.01);
        assert!(parse_price("100001", Lang::En).is_err());
    }

    /// The English half is what the web suite pins, and `ERR_*` now reads
    /// from the table — so spell the literals out here, where a typo would
    /// otherwise move both sides of the assertion at once.
    #[test]
    fn the_english_halves_are_the_web_literals() {
        let pinned: [(Entry, &str); 37] = [
            (ASST_ERR_ENDPOINT, "Enter an HTTP(S) API base URL without credentials, query parameters or a fragment."),
            (ASST_ERR_MODEL, "Enter a model ID."),
            (ASST_ERR_PROVIDER, "Choose an API protocol."),
            (ASST_ERR_REASONING, "Choose a reasoning effort."),
            (ASST_ERR_CONTEXT_WINDOW, "Context window must be 0, or an integer between 1000 and 2000000."),
            (ASST_ERR_MAX_TOKENS, "Max output tokens must be 0, or an integer between 1 and 100000."),
            (ASST_ERR_PRICES, "Prices must be numbers between 0 and 100000."),
            (ASST_ERR_HEADERS, "Invalid headers: use one `Name: value` per line, with names limited to letters, digits and hyphens."),
            (ASST_SAVED, "Configuration saved on this device."),
            (ASST_CLEARED, "AI configuration cleared from this device."),
            (ASST_DIRTY, "Changes have not been saved."),
            (ASST_SAVE_ERROR, "Save failed. Check that app / browser storage is allowed, then retry."),
            (ASST_CLEAR_ERROR, "Clear failed. The saved configuration is still present; retry."),
            (ASST_BUSY, "Agent is running. Return to the conversation and stop it before editing configuration."),
            (ASST_PLAINTEXT_WARNING, "Warning: this endpoint uses plain http and is not loopback, so the API key and serial logs travel the network in cleartext. Use https, or host the service on localhost."),
            (ASST_PLAINTEXT_CONFIRM, "This endpoint uses plain http and is not loopback, so the API key will be sent in cleartext. Save anyway?"),
            (ASST_PLAINTEXT_BLOCKED, "Save cancelled: a plaintext http endpoint needs confirmation before an API key is stored."),
            (ASST_STORAGE_HINT, "Configuration and API key are saved in this device's app / browser storage across reloads and restarts. Clear the configuration or app / site data to remove them."),
            (ASST_NOTICE_HINT, "When you ask a question, the assistant sends the requested serial logs to your configured model service."),
            (ASST_REASONING_OFF, "Off"),
            (ASST_REASONING_LOW, "Low"),
            (ASST_REASONING_MEDIUM, "Medium"),
            (ASST_REASONING_HIGH, "High"),
            (ASST_LABEL_ENDPOINT, "API base URL"),
            (ASST_LABEL_MODEL, "Model ID"),
            (ASST_LABEL_API_KEY, "API key"),
            (ASST_HEADERS_EXTRA, "  extra headers"),
            (ASST_HEADERS_ONE_PER_LINE, "  one `Name: value` per line"),
            (ASST_LABEL_PROVIDER, "API protocol"),
            (ASST_LABEL_REASONING, "Reasoning effort"),
            (ASST_LABEL_CONTEXT, "Context window"),
            (ASST_LABEL_MAX_TOKENS, "Max output tokens"),
            (ASST_LABEL_PRICE_IN, "Price input / 1M"),
            (ASST_LABEL_PRICE_OUT, "Price output / 1M"),
            (ASST_ACTION_SAVE, "Save configuration"),
            (ASST_ACTION_CLEAR, "Clear configuration"),
            (ASST_KEYS_HINT, "↑/↓ select · ←/→ cycle · Ctrl+S save · Esc close"),
        ];
        for (entry, expected) in pinned {
            assert_eq!(t(entry, Lang::En), expected, "english drifted: {entry:?}");
        }
        assert_eq!(PROVIDERS[0].1, "OpenAI-compatible API");
        assert_eq!(REASONING[3].0, "high", "the stored id stays english");
    }

    /// Both languages of every message carry text and differ.
    #[test]
    fn every_asst_message_is_translated() {
        super::super::i18n::assert_bilingual(ALL);
        assert!(ALL.len() >= 37, "the AI dialog alone carries 37 messages");
    }

    /// Chinese must reach the rendered dialog, not just the table.
    #[test]
    fn the_dialog_body_follows_the_language() {
        let state = state_with("https://api.example.com/v1", "gpt-4o");
        let body = |lang| {
            render_lines(&state, 74, lang)
                .iter()
                .map(|line| {
                    line.spans
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        let en = body(Lang::En);
        assert!(en.contains("API base URL"), "{en}");
        assert!(en.contains("Save configuration"), "{en}");
        assert!(en.contains("Ctrl+S save"), "{en}");

        let zh = body(Lang::Zh);
        assert!(zh.contains("API 基础 URL"), "{zh}");
        assert!(zh.contains("保存配置"), "{zh}");
        assert!(zh.contains("Ctrl+S 保存"), "{zh}");
        assert!(!zh.contains("Save configuration"), "{zh}");

        // Reasoning options follow the language, their ids do not.
        assert_eq!(REASONING[0].0, "off", "the stored id is a protocol value");
        assert_eq!(t(REASONING[0].1, Lang::En), "Off");
        assert_eq!(t(REASONING[0].1, Lang::Zh), "关闭");
        assert_eq!(t(REASONING[3].1, Lang::Zh), "高");
    }

    /// A field row has to end inside the dialog whatever the value holds: a
    /// CJK value is two columns wide, and the character count used for the
    /// cut put such a row one column over per glyph, wrapping the dialog and
    /// shifting every row below it.
    #[test]
    fn a_cjk_value_stays_inside_the_dialog() {
        let mut state = state_with("https://api.example.com/v1", "gpt-4o");
        state
            .endpoint
            .set("https://".to_string() + &"中".repeat(60));
        state.model.set("模型".repeat(30));

        for width in [24u16, 40, 74] {
            for lang in [Lang::En, Lang::Zh] {
                let mut rows = 0usize;
                for line in render_lines(&state, width, lang) {
                    let spans: Vec<&str> = line.spans.iter().map(|s| s.content.as_ref()).collect();
                    if !matches!(spans.first().copied(), Some("▸ ") | Some("  ")) {
                        continue;
                    }
                    rows += 1;
                    let cols: usize = spans
                        .iter()
                        .map(|s| unicode_width::UnicodeWidthStr::width(*s))
                        .sum();
                    assert!(
                        cols <= width as usize,
                        "width {width} {lang:?}: row is {cols} columns: {}",
                        spans.concat()
                    );
                }
                assert!(rows >= 12, "{lang:?}: only {rows} rows were checked");
            }
        }

        // And the value really was cut, not merely accepted.
        let zh = render_lines(&state, 74, Lang::Zh)
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(zh.contains('…'), "the long value must be cut: {zh}");
        assert!(
            !zh.contains(&"中".repeat(30)),
            "the whole value must not be drawn: {zh}"
        );
    }
}
