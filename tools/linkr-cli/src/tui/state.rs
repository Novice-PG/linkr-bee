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
use super::i18n::{strings, t, tr, Lang, MSG_SAVE_SETTINGS};
use super::network_view::NetworkState;
use super::palette::PaletteState;
use super::settings::{TransportChoice, TuiSettings};
use super::sidebar::SidebarState;
use super::terminal_view::TerminalPane;
use tokio::sync::{broadcast, oneshot};

strings! {
    VIEW_TERMINAL => "Serial Terminal", "串口终端";
    VIEW_DIAGNOSTICS => "Diagnostics", "诊断";
    VIEW_NETWORK => "Network", "网络";
    VIEW_ASSISTANT => "Assistant", "助手";
    FOCUS_SIDEBAR => "Sidebar", "侧栏";
    FOCUS_CENTER => "Terminal", "终端";
}

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
    pub fn label(self, lang: Lang) -> &'static str {
        match self {
            View::Terminal => t(VIEW_TERMINAL, lang),
            View::Diagnostics => t(VIEW_DIAGNOSTICS, lang),
            View::Network => t(VIEW_NETWORK, lang),
            View::Assistant => t(VIEW_ASSISTANT, lang),
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
    pub fn label(self, lang: Lang) -> &'static str {
        match self {
            Focus::Sidebar => t(FOCUS_SIDEBAR, lang),
            Focus::Center => t(FOCUS_CENTER, lang),
            Focus::Assistant => t(VIEW_ASSISTANT, lang),
        }
    }
}

/// Single-line (and simple multi-line) editor used by every form.
///
/// `text` and `cursor` are private on purpose: the caret is a **byte** index
/// that has to stay inside `text` *and* on a `char` boundary, while
/// `display()`, `home()` and `end()` slice straight at it. The form re-seeders
/// assign a whole new value to a field — `seed_form()` for a `--lan` target,
/// `aim()` for a freshly scanned BLE device, `aim_host()` for the IP WiFi just
/// confirmed — so writing `text` directly left the caret at the end of the
/// previous, longer value and the next frame panicked on the slice. Under
/// `panic = "abort"` that takes the whole TUI down mid-paste (P2). Every write
/// therefore goes through [`Self::set`]/[`Self::clear`], which move the caret
/// with the text, and [`Self::caret`] repairs a stale one before any slice.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextField {
    text: String,
    /// Byte index of the caret; always on a `char` boundary.
    cursor: usize,
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

    /// The caret, forced back inside `text` and onto a `char` boundary.
    ///
    /// The fields being private means outside code cannot get this wrong; the
    /// clamp is the second line of defence for everything that still can — the
    /// tests in this module and any future `pub(crate)` shortcut. One stale
    /// byte index would otherwise abort the process on the next frame, so the
    /// render path never trusts `cursor` as-is.
    fn caret(&self) -> usize {
        let mut at = self.cursor.min(self.text.len());
        while at > 0 && !self.text.is_char_boundary(at) {
            at -= 1;
        }
        at
    }

    /// Repair a stale caret before an edit that slices at it.
    fn settle(&mut self) {
        self.cursor = self.caret();
    }

    pub fn insert_char(&mut self, c: char) {
        self.settle();
        let mut text = std::mem::take(&mut self.text);
        text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
        self.text = text;
    }

    pub fn insert_str(&mut self, s: &str) {
        self.settle();
        let mut text = std::mem::take(&mut self.text);
        text.insert_str(self.cursor, s);
        self.cursor += s.len();
        self.text = text;
    }

    pub fn backspace(&mut self) {
        self.settle();
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
        self.settle();
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
        self.settle();
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
        self.settle();
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
        self.settle();
        self.cursor = self.text[..self.cursor]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
    }

    pub fn end(&mut self) {
        self.settle();
        self.cursor = self.text[self.cursor..]
            .find('\n')
            .map(|i| self.cursor + i)
            .unwrap_or(self.text.len());
    }

    /// Rendered text plus caret column (maskable for passwords).
    pub fn display(&self, mask: Option<char>) -> (String, usize) {
        let at = self.caret();
        match mask {
            None => (self.text.clone(), self.text[..at].chars().count()),
            Some(m) => {
                let masked: String = self.text.chars().map(|_| m).collect();
                let col = self.text[..at].chars().count();
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
/// Grace period after the last resize event before the frame is painted a
/// second time. A conhost keeps reflowing after it stopped emitting events;
/// cells written during that window are lost (K1, the stale frame Ctrl+L
/// clears). Sized to sit well below one human-visible flicker but above the
/// time a reflow takes.
pub const RESIZE_SETTLE: Duration = Duration::from_millis(150);

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
    /// What the quit confirm (`Ctrl+Q`) displaced, restored when the user
    /// declines it. Overwriting `dialog` used to throw the open box away: an
    /// approval's dropped sender reads to the agent as "rejected", and a
    /// half-typed AI configuration or a scan result list was simply lost.
    pub dialog_return: Option<Box<Dialog>>,
    pub notices: Notices,

    // Assistant runtime.
    pub exec_mode: ExecMode,
    /// `executionModeExpiresAt` of `web/device_executor.js`: the wall-clock
    /// millisecond the unattended window ends, `0` when none is armed. Kept
    /// beside `exec_mode` as well as inside the runtime because the panel
    /// decides when the window *starts* — possibly long before the runtime
    /// exists (the TUI spawns it on the first question).
    pub exec_expires_at: u64,
    pub broker: TuiBroker,
    pub agent: Option<AgentRuntime>,

    /// `Ctrl+Shift+V` answer in flight (`clipboard::spawn_read`). Collected
    /// by `mod::poll_clipboard`; never waited on, because the frame loop is
    /// the thread that draws.
    pub pending_paste: Option<std::sync::mpsc::Receiver<Option<String>>>,
    /// Inbound `OSC 52` writes in flight (`clipboard::spawn_write`); `false`
    /// means no helper took the payload.
    pub clipboard_jobs: Vec<std::sync::mpsc::Receiver<bool>>,

    // Serial watch (findings shown in the sidebar).
    pub watch: SerialWatch,
    pub watch_ok: bool,

    pub quit: bool,
    pub started: Instant,
    pub force_redraw: bool,
    /// A second full repaint, scheduled for the moment the console settles.
    /// `Event::Resize` only reports that the window *changed*: a Windows
    /// conhost keeps reflowing for a while after the last event, and cells we
    /// paint while it is still moving are lost — the frame then stays stale
    /// until Ctrl+L (K1). Every resize event pushes the deadline forward, so a
    /// drag collapses into exactly one extra repaint once the mouse stops.
    pub settle_repaint_at: Option<Instant>,

    /// Visible height of the center pane in rows; the layout feeds it back so
    /// the VT grid and the scroll helpers know their geometry.
    pub center_height: u16,
    /// Width of the center pane. Together with `center_height` it lets the key
    /// handler compute the same scroll limit the renderer paints against.
    pub center_width: u16,
    /// Top-left corner of the center pane in frame coordinates. The mouse
    /// reports screen coordinates, so a press has to be put back into the
    /// pane before it can name a grid cell.
    pub center_x: u16,
    pub center_y: u16,
    /// Height of the whole frame. Overlays size themselves against it, and the
    /// arrow keys need it to know how far a scrollable overlay may move.
    pub screen_height: u16,
    /// Scroll offset of the non-terminal center views (Diagnostics/Network).
    pub center_scroll: u16,

    /// In-flight (re)connect started by [`super::connect::connect`]; polled by
    /// the event loop so the UI never blocks on the transport handshake.
    pub pending_connect: Option<oneshot::Receiver<Result<SessionHandle, String>>>,

    /// In-flight BLE sweep started by [`super::connect::begin_scan`] when the
    /// sidebar's *Switch device* entry is pressed. The web can lean on
    /// `requestDevice()` for both halves; the TUI owns the scan itself, so it
    /// has to park the oneshot somewhere the loop keeps draining.
    pub pending_scan:
        Option<oneshot::Receiver<Result<Vec<crate::transport::DiscoveredDevice>, String>>>,

    /// In-flight `@s?` capture (web `requestDeviceState()`): `@s?` is the only
    /// place the bridge reports its LAN access token, so a BLE session asks
    /// for it once and the reply fills the token field and the store.
    pub socket_query: Option<oneshot::Receiver<Result<crate::protocol::MgmtReply, String>>>,
    /// Device id of the BLE session that owns the captured token; the store
    /// keys tokens by `device:<id>`, never by address.
    pub lan_device: Option<String>,
}

impl App {
    /// Refresh the cached `SessionInfo`; cheap (mutex clone).
    pub fn refresh_info(&mut self) {
        self.info = self.session.info();
        if self.info.connected {
            self.set_connection_state(ConnectionState::Connected);
        } else if self.state == ConnectionState::Connected {
            self.set_connection_state(ConnectionState::Disconnected);
        }
    }

    /// Apply a connection state change, running the "on connect" side effects
    /// **once per transition**: spec §6.4 resets the serial journal "on
    /// connect and on Clear". The feed reads one shared bus, so without this
    /// the assistant would quote the session that just ended as live evidence
    /// in the next one. Every path that lands on `Connected` goes through here
    /// (`refresh_info`, the `CoreEvent::Connection` arm, `connect::adopt`), and
    /// `Disconnected`/`Failed` stay free of side effects.
    pub(crate) fn set_connection_state(&mut self, next: ConnectionState) {
        let was = self.state;
        self.state = next;
        if next == ConnectionState::Connected && was != ConnectionState::Connected {
            self.reset_assistant_evidence();
            self.end_unattended_window();
        }
    }

    /// `AgentHandle::reset_journal()` on whatever runtime is up (`None` before
    /// the assistant is first used — an empty journal needs no resetting).
    pub(crate) fn reset_assistant_evidence(&mut self) {
        if let Some(runtime) = self.agent.as_ref() {
            runtime.handle.reset_journal();
        }
    }

    /// `connectionChanged()` of `web/agent_panel.js`: coming back up drops the
    /// unattended window and says so, because the run that armed it is gone
    /// and nobody was watching the gap. Any other mode is left alone.
    pub(crate) fn end_unattended_window(&mut self) {
        if self.exec_mode != ExecMode::FullAuto {
            return;
        }
        let notice = t(
            super::assistant_view::ASST_FULL_AUTO_RECONNECTED,
            self.lang(),
        )
        .to_string();
        self.exec_mode = ExecMode::Auto;
        self.exec_expires_at = 0;
        if let Some(runtime) = self.agent.as_ref() {
            runtime.handle.set_mode(ExecMode::Auto);
        }
        self.assistant
            .entries
            .push(super::assistant_view::Entry::System(notice));
    }

    pub fn connected(&self) -> bool {
        self.state == ConnectionState::Connected
    }

    /// The transport choice is pinned while a link is up **and** while an
    /// attempt is being made — web parity: `bleModeBtn`, `lanModeBtn` and
    /// `switchDeviceButton` are all disabled with
    /// `connecting || state.connected` (web/app.js:1949-1950, 847, 1945).
    ///
    /// Pinning the attempt is what closes the dead end behind "connect, then
    /// search": the row stayed live mid-dial, so a switch to BLE during a LAN
    /// attempt left the form on BLE while the status line still reported the
    /// LAN dial — and "Switch device" then only answered "Connecting…", with
    /// no way back to a state where either worked. The row now says
    /// `（已锁定）` for the length of the attempt instead.
    ///
    /// The cost is the one an earlier version of this guard already paid and
    /// was reported for: `--tui` boots with a deferred connect in flight, so
    /// the row sits on `（已锁定）` and every switch is refused for those
    /// seconds (the reported "cannot switch to LAN"). Web reaches that state
    /// only after a click — its restore puts the device back
    /// (`app.js:2079`) but never calls `connect()` from `init()` — so it has
    /// no boot window of its own; what is shared is the predicate it disables
    /// on, `connecting || state.connected`, applied here to an attempt this
    /// binary started by itself. The window is the *attempt*, and it is wider
    /// than the dial behind it: LAN is `CONNECT_TIMEOUT` (15 s) plus a 5 s
    /// handshake **per try**, and a refused dial or a bridge that hangs up is
    /// retried three more times with a 0.7 / 1.6 / 3 s backoff
    /// (`transport::lan::RETRY_DELAYS`, the shape measured on hardware as
    /// G1) — roughly a minute at worst, not 15 s. (`BLE_TIMEOUT`'s 8 s is
    /// the scan timeout; the BLE dial carries no clock of its own.) The
    /// moment it settles — connected, failed or dropped — the row unlocks
    /// again.
    ///
    /// The row and the toggle still move together: the row renders
    /// [`Self::transport_choice`], which reports the *live* link only while one
    /// is up and the stored choice otherwise — and during an attempt the
    /// stored choice is the form the attempt was started from (F1).
    pub fn transport_locked(&self) -> bool {
        self.connected() || self.state == ConnectionState::Connecting
    }

    /// Transport of the session in play: the link that is up, or the attempt
    /// still being made — either way it is a known transport. `None` once the
    /// session is gone.
    ///
    /// [`SessionInfo::kind`] is written when the session is *built* and
    /// [`crate::session`] never clears it on teardown, so after a disconnect
    /// it keeps naming the link that just closed. Reading it as-is is what
    /// left the status line saying `未连接 · BLE` forever; only a session that
    /// still exists may speak for itself.
    pub fn live_kind(&self) -> Option<crate::transport::TransportKind> {
        if self.state == ConnectionState::Disconnected {
            None
        } else {
            self.info.kind
        }
    }

    /// Management commands require BLE (WEB_UX_SPEC section 3.6 matrix).
    pub fn ble_connected(&self) -> bool {
        self.connected() && self.info.kind == Some(crate::transport::TransportKind::Ble)
    }

    /// Interface language of the running session (`linkr-lang`).
    pub fn lang(&self) -> super::i18n::Lang {
        self.settings.lang
    }

    pub fn capabilities(&self) -> u32 {
        if self.ble_connected() {
            self.info.capabilities
        } else {
            0
        }
    }

    /// What the next dial would use, and therefore what the unlocked sidebar
    /// row shows.
    ///
    /// Only a link that is **up** may override the stored choice. It used to
    /// read `info.kind` unconditionally, and that field survives the session
    /// that wrote it: after a disconnect it still said `Ble`, so the row
    /// rendered `传输方式：BLE` while the toggle flipped `settings.transport`
    /// behind it — the reported "switch to LAN does nothing", which then also
    /// dialed BLE again. While an attempt is in flight the same staleness
    /// (there the kind is live, but no link exists yet) hid the switch. With
    /// no link the choice *is* what would be dialled, so it is shown, and it
    /// is exactly what the toggle writes: unlocked ⇒ responsive (F1).
    pub fn transport_choice(&self) -> TransportChoice {
        if !self.connected() {
            return self.settings.transport;
        }
        match self.live_kind() {
            Some(crate::transport::TransportKind::Ble) => TransportChoice::Ble,
            Some(crate::transport::TransportKind::Lan) => TransportChoice::Lan,
            None => self.settings.transport,
        }
    }

    /// Consume the "repaint every cell" request. The console can repaint
    /// itself behind our back (a Windows conhost reflows its buffer when the
    /// window changes size) and ratatui's cell diff would then keep skipping
    /// every cell it believes is already correct, leaving stale frames on
    /// screen (F2). The event loop clears the terminal when this returns
    /// `true`.
    pub fn take_force_redraw(&mut self) -> bool {
        std::mem::take(&mut self.force_redraw)
    }

    /// Repaint now, and again once [`RESIZE_SETTLE`] has passed without
    /// another resize event (see `settle_repaint_at`).
    pub fn schedule_resize_repaint(&mut self, now: Instant) {
        self.force_redraw = true;
        self.settle_repaint_at = Some(now + RESIZE_SETTLE);
    }

    /// Deliver the delayed repaint when its deadline is due. Called once per
    /// frame; `now` is passed in so the deadline can be tested without
    /// sleeping.
    pub fn poll_settle_repaint(&mut self, now: Instant) {
        if self.settle_repaint_at.is_some_and(|at| now >= at) {
            self.settle_repaint_at = None;
            self.force_redraw = true;
        }
    }

    /// True while a modal overlay (command palette or dialog) owns a slice of
    /// the frame.
    pub fn overlay_open(&self) -> bool {
        self.palette.is_some() || self.dialog.is_some()
    }

    /// Arm a full repaint when the overlay state moved since `before`.
    ///
    /// An overlay covers a large part of the screen, and a console that
    /// repaints itself underneath us (conhost reflows whenever the window or
    /// the buffer changes) leaves the panel's text behind: the cell diff
    /// believes those cells already hold the right characters and never
    /// writes them again — cancelling the palette then looked like text that
    /// only `Ctrl+L` could wipe, because the clear blanks the viewport first
    /// (K5, the same family as F2). Repainting at the moment an overlay opens
    /// or closes removes it without waiting for the user to notice, and it
    /// costs one repaint per key press that touches an overlay.
    pub fn sync_overlay_repaint(&mut self, before: bool) {
        if self.overlay_open() != before {
            self.force_redraw = true;
        }
    }

    /// What the left sidebar currently says about the link: the connection
    /// state, the transport it offers and the link actually in play.
    pub fn link_signature(
        &self,
    ) -> (
        ConnectionState,
        TransportChoice,
        Option<crate::transport::TransportKind>,
    ) {
        (self.state, self.transport_choice(), self.live_kind())
    }

    /// Arm a full repaint when the link state moved since `before`.
    ///
    /// Switching between LAN (WiFi) and BLE rewrites most of the sidebar —
    /// transport row, address, state line — and a console that repainted
    /// itself underneath us (conhost, whenever it reflows or paints behind
    /// our back) leaves the previous transport's text standing: the cell diff
    /// believes those cells already hold the right characters and never
    /// writes them again, so the two states end up stacked until `Ctrl+L`
    /// blanks the viewport (the same family as F2/K5, K6). Link changes are
    /// rare and user-triggered, so one full repaint per transition is free.
    pub fn sync_link_repaint(
        &mut self,
        before: (
            ConnectionState,
            TransportChoice,
            Option<crate::transport::TransportKind>,
        ),
    ) {
        if self.link_signature() != before {
            self.force_redraw = true;
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
                let lang = self.lang();
                self.notices
                    .push(NoticeLevel::Warn, tr!(t(MSG_SAVE_SETTINGS, lang), err));
            }
        }
        self.view = view;
        // The offset counts lines skipped from the top. The assistant's
        // composer is the last line of its transcript, so it enters pinned to
        // the bottom; every other view reads from the start.
        self.center_scroll = match view {
            View::Assistant => super::layout::PIN_END,
            _ => 0,
        };
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
        // The web key bar and the presets are disabled until `setConnected()`
        // says so, so a keystroke with no link is inert there. Without this
        // gate every character typed after a drop raised its own "session gone"
        // error line, evicting real notices from the log.
        if !self.connected() {
            return;
        }
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
    ///
    /// Gated on the link because that is what the web key bar does (its
    /// buttons are `disabled` until `setConnected()`), and because a keystroke
    /// typed after a drop used to raise its own "session gone" notice per key,
    /// evicting the real ones. Answers to the **device** are a different thing
    /// and go through [`Self::send_reply`].
    pub fn send_bytes(&mut self, bytes: Vec<u8>) {
        if !self.connected() {
            return;
        }
        if let Err(err) = self.session.send_uart(bytes) {
            self.notices.push(NoticeLevel::Error, err.to_string());
        }
    }

    /// Hand the terminal's own answers (`DSR`/`CPR` from
    /// [`super::terminal_view::TermGrid::take_pending_reports`]) to the
    /// session.
    ///
    /// Deliberately **not** gated on [`Self::connected`]: that state lags the
    /// transport by a poll tick (and sits on `Connecting` for the length of an
    /// adopt), while the question itself only exists because those bytes just
    /// arrived from the device. Dropping the reply leaves the program that
    /// asked waiting for an answer it will never get — the gate is about typed
    /// keys, not about a protocol response to a message we already consumed.
    pub fn send_reply(&mut self, bytes: Vec<u8>) {
        if let Err(err) = self.session.send_uart(bytes) {
            self.notices.push(NoticeLevel::Error, err.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::i18n;

    #[test]
    fn every_state_message_is_translated() {
        // The view and focus labels are the only messages this module defines;
        // the settings save failure reuses the shared i18n table.
        i18n::assert_bilingual(ALL);
        assert!(ALL.len() >= 6, "views and focus carry 6 messages");
        i18n::assert_bilingual(&[("MSG_SAVE_SETTINGS", i18n::MSG_SAVE_SETTINGS)]);
    }

    /// The P2 crash at the smallest possible scale: a form re-seeder swapped
    /// the value under a caret that still pointed past the end of the old,
    /// longer one. The render slices at the caret **every frame**, so a stale
    /// byte index is a guaranteed abort (`panic = "abort"`) — nothing may trust
    /// `cursor` as-is.
    #[test]
    fn a_caret_left_past_the_end_of_a_shorter_value_still_renders_and_edits() {
        let mut field = TextField::new("ws://192.0.2.9:99999/ws");
        // What the seed paths did while `text` was public: value replaced,
        // caret left where it was — 24 bytes into a 9-byte string.
        field.text = "192.0.2.9".to_string();

        assert_eq!(
            field.display(None),
            ("192.0.2.9".to_string(), 9),
            "the caret clamps to the end of the new value"
        );
        // Every edit repairs the caret before it slices, so none of these panic.
        field.home();
        field.end();
        field.left();
        field.right();
        field.backspace();
        field.delete();
        field.insert_char('x');
        field.insert_str("yy");
        assert!(field.cursor <= field.text.len());
        assert!(field.text.is_char_boundary(field.cursor));
    }

    /// …and the same when the stale caret lands inside a multi-byte character.
    #[test]
    fn a_caret_inside_a_multibyte_character_walks_back_to_a_boundary() {
        let mut field = TextField::new("中文");
        field.text = "中文测试".to_string();
        field.cursor = 4; // two bytes into 文, which starts at 3

        assert_eq!(
            field.display(Some('*')),
            ("****".to_string(), 1),
            "the caret backs up to the boundary before the character"
        );
        field.insert_char('!');
        assert_eq!(
            field.as_str(),
            "中!文测试",
            "the edit lands on that boundary"
        );
        assert_eq!(field.cursor, 4, "…and advances by the inserted char");
    }

    /// G-2: the evidence window resets on the **edge** into Connected — once
    /// per connect, never on a poll that already sees the state, and never for
    /// `Disconnected`/`Failed`. The feed reads a shared bus, so a reset that
    /// fired on every tick would wipe evidence as fast as it arrived.
    #[test]
    fn only_a_transition_into_connected_resets_the_evidence() {
        let mut app = crate::tui::test_app();
        let handle = crate::tui::attach_agent(&mut app);

        app.state = ConnectionState::Disconnected;
        crate::tui::feed_journal(&app, &handle, b"live bytes");
        app.set_connection_state(ConnectionState::Connected);
        assert_eq!(handle.journal_len(), 0, "the connect edge resets");

        crate::tui::feed_journal(&app, &handle, b"more live bytes");
        app.set_connection_state(ConnectionState::Connected);
        assert!(
            handle.journal_len() > 0,
            "a poll that already sees Connected must not wipe live evidence"
        );

        app.set_connection_state(ConnectionState::Disconnected);
        crate::tui::feed_journal(&app, &handle, b"bytes after the drop");
        app.set_connection_state(ConnectionState::Failed);
        assert!(
            handle.journal_len() > 0,
            "Failed is not a connect, so it resets nothing"
        );
    }

    /// `connectionChanged()` of `web/agent_panel.js`: reconnecting drops the
    /// unattended window and leaves the `fullAutoReconnected` note behind; a
    /// poll that already sees `Connected` does neither.
    #[test]
    fn reconnecting_ends_the_unattended_window_with_a_note() {
        let mut app = crate::tui::test_app();
        let lang = app.lang();
        let handle = crate::tui::attach_agent(&mut app);
        crate::tui::assistant_view::set_mode(&mut app, ExecMode::FullAuto);
        assert_eq!(handle.mode(), ExecMode::FullAuto);

        app.state = ConnectionState::Disconnected;
        app.set_connection_state(ConnectionState::Connected);

        assert_eq!(app.exec_mode, ExecMode::Auto, "the panel follows");
        assert_eq!(handle.mode(), ExecMode::Auto, "…and so does the runtime");
        let notice = t(crate::tui::assistant_view::ASST_FULL_AUTO_RECONNECTED, lang);
        assert!(
            app.assistant.entries.iter().any(
                |entry| matches!(entry, crate::tui::assistant_view::Entry::System(text) if text == notice)
            ),
            "the note explains the fallback"
        );

        app.assistant.entries.clear();
        app.set_connection_state(ConnectionState::Connected);
        assert!(
            app.assistant.entries.is_empty(),
            "no second note without a reconnect"
        );
    }

    #[test]
    fn the_view_and_focus_labels_follow_the_language() {
        assert_eq!(View::Terminal.label(Lang::En), "Serial Terminal");
        assert_eq!(View::Terminal.label(Lang::Zh), "串口终端");
        assert_eq!(View::Diagnostics.label(Lang::Zh), "诊断");
        assert_eq!(View::Network.label(Lang::Zh), "网络");
        assert_eq!(Focus::Center.label(Lang::En), "Terminal");
        assert_eq!(Focus::Center.label(Lang::Zh), "终端");
        assert_eq!(Focus::Sidebar.label(Lang::Zh), "侧栏");
        assert_eq!(Focus::Assistant.label(Lang::Zh), "助手");
    }

    /// F2: the event loop *consumes* the repaint request. It used to be set
    /// from `Event::Resize` and never read anywhere, so the "full repaint"
    /// the console needs after it reflows itself never happened.
    #[test]
    fn a_forced_repaint_is_consumed_exactly_once() {
        let mut app = crate::tui::test_app();
        assert!(!app.take_force_redraw());
        app.force_redraw = true;
        assert!(app.take_force_redraw());
        assert!(!app.take_force_redraw());
    }

    /// K1: a resize repaints at once and again once the console has had
    /// [`RESIZE_SETTLE`] to finish reflowing; the delayed repaint is delivered
    /// exactly once.
    #[test]
    fn a_resize_repaints_again_once_the_console_settled() {
        let mut app = crate::tui::test_app();
        let start = Instant::now();

        app.schedule_resize_repaint(start);
        assert!(app.take_force_redraw(), "the resize repaints right away");
        assert!(!app.take_force_redraw());

        app.poll_settle_repaint(start + Duration::from_millis(50));
        assert!(!app.force_redraw, "the console has not settled yet");

        app.poll_settle_repaint(start + RESIZE_SETTLE);
        assert!(app.take_force_redraw(), "the settled repaint arrives");

        app.poll_settle_repaint(start + Duration::from_secs(5));
        assert!(!app.force_redraw, "and exactly once");
    }

    /// K1: dragging the window fires one resize per mouse move. Every event
    /// must move the deadline forward, so the drag collapses into a single
    /// repaint instead of flickering once per event.
    #[test]
    fn a_resize_drag_ends_in_a_single_settled_repaint() {
        let mut app = crate::tui::test_app();
        let start = Instant::now();

        app.schedule_resize_repaint(start);
        app.schedule_resize_repaint(start + Duration::from_millis(40));
        assert!(app.take_force_redraw(), "each resize repaints at once");
        // 160 ms in, i.e. 120 ms after the last event: still inside the window.
        app.poll_settle_repaint(start + Duration::from_millis(160));
        assert!(!app.force_redraw, "the deadline follows the latest event");

        app.poll_settle_repaint(start + Duration::from_millis(40) + RESIZE_SETTLE);
        assert!(app.take_force_redraw(), "one repaint after the drag ends");
    }

    /// K5: the loop watches this flag, so it has to read as "open" for both
    /// overlays and as "closed" again afterwards — a miss would silently skip
    /// the repaint that wipes the panel's text.
    #[test]
    fn the_palette_and_a_dialog_read_as_an_open_overlay() {
        let mut app = crate::tui::test_app();
        assert!(!app.overlay_open(), "nothing covers the frame at boot");

        app.palette = Some(PaletteState::new(app.lang()));
        assert!(app.overlay_open(), "the command palette covers the frame");
        app.palette = None;

        app.dialog = Some(Dialog::Help(0));
        assert!(app.overlay_open(), "a dialog covers the frame");
        app.dialog = None;
        assert!(!app.overlay_open(), "and both read as closed again");
    }

    /// K5: opening and closing repaint; a frame in which the overlay did not
    /// move must not repaint (otherwise every key press would clear the
    /// screen).
    #[test]
    fn only_an_overlay_transition_arms_a_full_repaint() {
        let mut app = crate::tui::test_app();

        let before = app.overlay_open();
        app.palette = Some(PaletteState::new(app.lang()));
        app.sync_overlay_repaint(before);
        assert!(app.take_force_redraw(), "opening repaints the screen");

        let before = app.overlay_open();
        app.sync_overlay_repaint(before);
        assert!(!app.force_redraw, "an unchanged overlay arms nothing");

        app.palette = None;
        app.sync_overlay_repaint(before);
        assert!(app.take_force_redraw(), "closing repaints the screen");
    }

    /// K6: switching WiFi/LAN ↔ BLE rewrites the whole link block of the
    /// sidebar (transport row, address, state line), so the event loop arms a
    /// full repaint the moment any part of the signature moved — conhost
    /// repaints itself underneath us and ratatui's cell diff then skips the
    /// sidebar cells it believes already hold the right characters, leaving
    /// the two transports stacked until `Ctrl+L`. A frame in which nothing
    /// moved must arm nothing, or every key press would clear the screen.
    #[test]
    fn only_a_link_transition_arms_a_full_repaint() {
        let mut app = crate::tui::test_app();

        // The transport toggle is the reported case (K6).
        let before = app.link_signature();
        app.settings.transport = match app.settings.transport {
            TransportChoice::Lan => TransportChoice::Ble,
            _ => TransportChoice::Lan,
        };
        app.sync_link_repaint(before);
        assert!(app.take_force_redraw(), "flipping the transport repaints");

        let before = app.link_signature();
        app.sync_link_repaint(before);
        assert!(!app.force_redraw, "an unchanged link arms nothing");

        // The connection state is part of the same sidebar block …
        app.state = ConnectionState::Connecting;
        app.sync_link_repaint(before);
        assert!(app.take_force_redraw(), "a state change repaints");

        // … and so is the transport a live session actually runs on, which
        // can move without `settings.transport` moving at all.
        let before = app.link_signature();
        app.info.kind = Some(crate::transport::TransportKind::Ble);
        app.sync_link_repaint(before);
        assert!(app.take_force_redraw(), "a live link change repaints");
    }

    /// Web parity: an attempt pins the transport choice exactly like a live
    /// session does — `web/app.js:1949-1950` disables both mode buttons with
    /// `connecting || state.connected`, and `:847/:1945` does the same for
    /// `switchDeviceButton`. Pinning the attempt is what closes the dead end
    /// behind "connect, then search": flipping the form mid-dial is how a BLE
    /// session ended up labelled LAN (F1), and leaving it live stranded the
    /// user on the BLE form with a LAN attempt running, where "Switch device"
    /// only answered "Connecting…".
    ///
    /// The price is the one this guard used to pay before it was narrowed:
    /// `--tui` boots with a deferred attempt in flight, so the row sits on
    /// `（已锁定）` and switching is refused for that window (the reported
    /// "cannot switch to LAN") — the same window web refuses in — bounded by
    /// the attempt and gone the moment it settles.
    #[test]
    fn connecting_and_live_links_lock_the_transport_choice() {
        let mut app = crate::tui::test_app();

        // The state alone says it: an attempt in flight pins the choice, with
        // or without a receiver parked on `pending_connect`.
        app.state = ConnectionState::Connecting;
        app.pending_connect = None;
        assert!(app.transport_locked(), "an attempt pins the choice");
        app.pending_connect = Some(tokio::sync::oneshot::channel().1);
        assert!(app.transport_locked());

        // Settled with nothing up: the row moves again.
        app.pending_connect = None;
        app.state = ConnectionState::Disconnected;
        assert!(!app.transport_locked());

        // A failed attempt is settled too — retrying over another transport
        // has to stay possible (`setConnecting(false)` on the web side).
        app.state = ConnectionState::Failed;
        assert!(!app.transport_locked());

        app.state = ConnectionState::Connected;
        assert!(app.transport_locked());
    }

    /// The reported "switch to LAN does nothing". `SessionInfo::kind` is
    /// written when the session is built and survives its teardown, so after
    /// a disconnect it still named the closed link: the row kept rendering
    /// `BLE`, the toggle flipped a setting nobody could see, and the next
    /// dial went to BLE regardless of what the user had chosen. Neither the
    /// row nor the status line may speak for a session that is gone — only
    /// an *up* link overrides the stored choice (F1).
    #[test]
    fn a_gone_session_cannot_hold_on_to_the_transport() {
        let mut app = crate::tui::test_app();
        app.settings.transport = TransportChoice::Lan;
        app.info.kind = Some(crate::transport::TransportKind::Ble);

        app.state = ConnectionState::Disconnected;
        assert_eq!(
            app.transport_choice(),
            TransportChoice::Lan,
            "the dead link must not outlive the dial it would replace"
        );
        assert_eq!(app.live_kind(), None, "no link, no transport to report");
        assert!(!app.transport_locked(), "…so the row must move on Enter");

        // A link that is up still wins over the stored choice.
        app.state = ConnectionState::Connected;
        assert_eq!(app.transport_choice(), TransportChoice::Ble);
        assert_eq!(app.live_kind(), Some(crate::transport::TransportKind::Ble));
        assert!(app.transport_locked());

        // An attempt in flight pins the row as well (web `connecting`), while
        // the status line may still say what is being dialled: the row renders
        // the stored choice — the form the attempt started from — so a locked
        // row never promises a toggle it cannot honour.
        app.state = ConnectionState::Connecting;
        assert_eq!(app.transport_choice(), TransportChoice::Lan);
        assert_eq!(app.live_kind(), Some(crate::transport::TransportKind::Ble));
        assert!(app.transport_locked());
    }
}
