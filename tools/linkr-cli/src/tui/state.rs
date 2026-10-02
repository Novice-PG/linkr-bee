//! TUI state: views, focus, text fields, toasts and the root `App`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::agent::{AgentEvent, AgentHandle, ExecMode};
use crate::event::{ConnectionState, NoticeLevel};
use crate::session::{CoreBus, SessionHandle};
use crate::watch::SerialWatch;

use super::assistant_view::AssistantState;
use super::diagnostics_view::DiagnosticsState;
use super::dialogs::{Dialog, TuiBroker};
use super::network_view::NetworkState;
use super::palette::PaletteState;
use super::settings::{TransportChoice, TuiSettings};
use super::sidebar::SidebarState;
use super::terminal_view::TerminalPane;
use tokio::sync::{broadcast, oneshot};

/// The four switchable surfaces (F2..F5, palette `view.*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum View {
    #[default]
    Terminal,
    Diagnostics,
    Network,
    Assistant,
}

impl View {
    pub fn label(self) -> &'static str {
        match self {
            View::Terminal => "Serial Terminal",
            View::Diagnostics => "Diagnostics",
            View::Network => "Network",
            View::Assistant => "Assistant",
        }
    }

    pub fn next(self) -> Self {
        match self {
            View::Terminal => View::Diagnostics,
            View::Diagnostics => View::Network,
            View::Network => View::Assistant,
            View::Assistant => View::Terminal,
        }
    }
}

/// Which pane receives unbound keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Focus {
    Sidebar,
    #[default]
    Center,
    Assistant,
}

impl Focus {
    pub fn label(self) -> &'static str {
        match self {
            Focus::Sidebar => "Sidebar",
            Focus::Center => "Terminal",
            Focus::Assistant => "Assistant",
        }
    }
}

/// Single-line (and simple multi-line) editor used by every form.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextField {
    pub text: String,
    /// Byte index of the caret; always on a `char` boundary.
    pub cursor: usize,
}

impl TextField {
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let cursor = text.len();
        Self { text, cursor }
    }

    pub fn set(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cursor = self.text.len();
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn insert_char(&mut self, c: char) {
        let mut text = std::mem::take(&mut self.text);
        text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
        self.text = text;
    }

    pub fn insert_str(&mut self, s: &str) {
        let mut text = std::mem::take(&mut self.text);
        text.insert_str(self.cursor, s);
        self.cursor += s.len();
        self.text = text;
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let mut text = std::mem::take(&mut self.text);
        let start = self.text_char_boundary_before(&text);
        text.remove(start);
        self.cursor = start;
        self.text = text;
    }

    pub fn delete(&mut self) {
        if self.cursor >= self.text.len() {
            return;
        }
        let mut text = std::mem::take(&mut self.text);
        let next = text[self.cursor..]
            .chars()
            .next()
            .map(|c| self.cursor + c.len_utf8())
            .unwrap_or(self.cursor);
        text.replace_range(self.cursor..next, "");
        self.text = text;
    }

    fn text_char_boundary_before(&self, text: &str) -> usize {
        let mut start = self.cursor - 1;
        while start > 0 && !text.is_char_boundary(start) {
            start -= 1;
        }
        start
    }

    pub fn left(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let mut cursor = self.cursor - 1;
        while cursor > 0 && !self.text.is_char_boundary(cursor) {
            cursor -= 1;
        }
        self.cursor = cursor;
    }

    pub fn right(&mut self) {
        if self.cursor >= self.text.len() {
            return;
        }
        let mut cursor = self.cursor + 1;
        while cursor < self.text.len() && !self.text.is_char_boundary(cursor) {
            cursor += 1;
        }
        self.cursor = cursor;
    }

    pub fn home(&mut self) {
        self.cursor = self.text[..self.cursor]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
    }

    pub fn end(&mut self) {
        self.cursor = self.text[self.cursor..]
            .find('\n')
            .map(|i| self.cursor + i)
            .unwrap_or(self.text.len());
    }

    /// Rendered text plus caret column (maskable for passwords).
    pub fn display(&self, mask: Option<char>) -> (String, usize) {
        match mask {
            None => (self.text.clone(), self.text[..self.cursor].chars().count()),
            Some(m) => {
                let masked: String = self.text.chars().map(|_| m).collect();
                let col = self.text[..self.cursor].chars().count();
                (masked, col)
            }
        }
    }
}

/// A transient notice in the top-right corner (web `toast()`: max 3 visible,
/// 2200 ms lifetime).
#[derive(Debug, Clone)]
pub struct Toast {
    pub text: String,
    pub level: NoticeLevel,
    pub at: Instant,
}

pub const TOAST_LIMIT: usize = 3;
pub const TOAST_LIFETIME: Duration = Duration::from_millis(2200);

impl Toast {
    pub fn expired(&self, now: Instant) -> bool {
        now.duration_since(self.at) > TOAST_LIFETIME
    }
}

/// Toast + notice-log helpers (shared by the loop and the dialogs).
#[derive(Default)]
pub struct Notices {
    pub toasts: Vec<Toast>,
    pub log: Vec<(NoticeLevel, String)>,
    pub dropped: usize,
}

impl Notices {
    pub fn push(&mut self, level: NoticeLevel, text: impl Into<String>) {
        let text = text.into();
        self.log.push((level, text.clone()));
        if self.log.len() > 500 {
            let drop = self.log.len() - 500;
            self.log.drain(..drop);
        }
        let now = Instant::now();
        self.toasts.retain(|t| !t.expired(now));
        if self.toasts.len() >= TOAST_LIMIT {
            self.toasts.remove(0);
            self.dropped += 1;
        }
        self.toasts.push(Toast {
            text,
            level,
            at: now,
        });
    }

    pub fn tick(&mut self) {
        let now = Instant::now();
        self.toasts.retain(|t| !t.expired(now));
    }
}

/// One live assistant runtime (spawned lazily on the first question).
pub struct AgentRuntime {
    pub handle: AgentHandle,
    pub events: broadcast::Receiver<AgentEvent>,
    pub last_error: Option<String>,
}

/// Root application state. Everything the event loop mutates lives here.
pub struct App {
    pub session: SessionHandle,
    pub bus: CoreBus,
    pub rt: Arc<tokio::runtime::Runtime>,
    pub settings: TuiSettings,

    // Connection snapshot (rebuilt from `SessionInfo` + `CoreEvent`s).
    pub state: ConnectionState,
    pub detail: String,
    pub info: crate::session::SessionInfo,

    // Surfaces.
    pub view: View,
    pub focus: Focus,
    pub sidebar: SidebarState,
    pub terminal: TerminalPane,
    pub diagnostics: DiagnosticsState,
    pub network: NetworkState,
    pub assistant: AssistantState,

    // Overlays.
    pub palette: Option<PaletteState>,
    pub dialog: Option<Dialog>,
    pub notices: Notices,

    // Assistant runtime.
    pub exec_mode: ExecMode,
    pub broker: TuiBroker,
    pub agent: Option<AgentRuntime>,

    // Serial watch (findings shown in the sidebar).
    pub watch: SerialWatch,
    pub watch_ok: bool,

    pub quit: bool,
    pub started: Instant,
    pub force_redraw: bool,

    /// Visible height of the center pane in rows; the layout feeds it back so
    /// the VT grid and the scroll helpers know their geometry.
    pub center_height: u16,
    /// Scroll offset of the non-terminal center views (Diagnostics/Network).
    pub center_scroll: u16,

    /// In-flight (re)connect started by [`super::connect::connect`]; polled by
    /// the event loop so the UI never blocks on the transport handshake.
    pub pending_connect: Option<oneshot::Receiver<Result<SessionHandle, String>>>,
}

impl App {
    /// Refresh the cached `SessionInfo`; cheap (mutex clone).
    pub fn refresh_info(&mut self) {
        self.info = self.session.info();
        if self.info.connected {
            if self.state == ConnectionState::Disconnected {
                self.state = ConnectionState::Connected;
            }
        } else if self.state == ConnectionState::Connected {
            self.state = ConnectionState::Disconnected;
        }
    }

    pub fn connected(&self) -> bool {
        self.state == ConnectionState::Connected
    }

    /// Management commands require BLE (WEB_UX_SPEC section 3.6 matrix).
    pub fn ble_connected(&self) -> bool {
        self.connected() && self.info.kind == Some(crate::transport::TransportKind::Ble)
    }

    pub fn capabilities(&self) -> u32 {
        if self.ble_connected() {
            self.info.capabilities
        } else {
            0
        }
    }

    pub fn transport_choice(&self) -> TransportChoice {
        match self.info.kind {
            Some(crate::transport::TransportKind::Ble) => TransportChoice::Ble,
            Some(crate::transport::TransportKind::Lan) => TransportChoice::Lan,
            None => self.settings.transport,
        }
    }

    pub fn toast(&mut self, level: NoticeLevel, text: impl Into<String>) {
        self.notices.push(level, text);
    }

    /// Current view switch helper used by the palette and F-keys.
    pub fn set_view(&mut self, view: View) {
        if self.settings.active_view != view {
            self.settings.active_view = view;
            if let Err(err) = super::settings::save(&self.settings) {
                self.notices
                    .push(NoticeLevel::Warn, format!("Could not save settings: {err}"));
            }
        }
        self.view = view;
        self.center_scroll = 0;
        match view {
            View::Terminal => self.focus = Focus::Center,
            View::Assistant => self.focus = Focus::Assistant,
            _ => self.focus = Focus::Center,
        }
    }

    /// Pending approval requests queued by [`TuiBroker`] (agent thread).
    pub fn take_approval(&mut self) -> Option<super::dialogs::PendingApproval> {
        self.broker.take_pending()
    }

    /// Send a line of text to the target, applying the Enter mode and the
    /// local-echo setting (web `sendText`).
    pub fn send_text(&mut self, text: &str) {
        let payload = super::terminal_view::prepare_line(
            text,
            self.settings.enter_mode,
            self.settings.local_echo,
        );
        if self.settings.local_echo {
            self.terminal.feed(&payload);
        }
        if let Err(err) = self.session.send_uart(payload) {
            self.notices.push(NoticeLevel::Error, err.to_string());
        }
    }

    /// Send raw key bytes produced by [`super::keys::encode_key`].
    pub fn send_bytes(&mut self, bytes: Vec<u8>) {
        if let Err(err) = self.session.send_uart(bytes) {
            self.notices.push(NoticeLevel::Error, err.to_string());
        }
    }
}
