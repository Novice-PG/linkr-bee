//! Application state: everything a frame needs, and nothing a frame does not.
//!
//! The state is plain data with small, total transitions. No view owns state
//! and no state knows about a view: `ui::*` reads `&App`, the event loop
//! mutates it, and every mutation is a method here so it can be tested without
//! a terminal.

use std::io;
use std::path::PathBuf;

use crate::event::{ConnectionState, NoticeLevel, View};
use crate::i18n::Lang;
use crate::session::{Dial, Session};
use crate::text::human_bytes;
use crate::theme::{Theme, ThemeChoice};
use crate::transfer::zmodem::{Event as TransferEvent, LiveTransfer};
use crate::transport::{Link, LinkEvent};
use crate::views::diagnostics::DiagnosticsState;
use crate::views::terminal::TerminalPane;
use crate::views::transfer::TransferState;
use crate::{dialogs, palette};

/// Which link carries the session.
///
/// The sidebar names it and the command line pins it, and it is the same list
/// the transport module implements — re-exported rather than mirrored, so a
/// link cannot exist without the chrome having a word for it.
pub use crate::transport::LinkKind as Transport;

/// A shortcut in the sidebar's quick-send list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickSend {
    /// The line that is sent when the row is activated.
    pub command: String,
    /// Whether the row asks for confirmation first (`reboot` and friends).
    pub dangerous: bool,
}

impl QuickSend {
    /// A plain shortcut.
    pub fn new(command: impl Into<String>) -> QuickSend {
        QuickSend {
            command: command.into(),
            dangerous: false,
        }
    }

    /// A shortcut that must be confirmed before it goes out.
    pub fn guarded(command: impl Into<String>) -> QuickSend {
        QuickSend {
            command: command.into(),
            dangerous: true,
        }
    }
}

/// A message the interface is showing right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub text: String,
    pub level: NoticeLevel,
}

/// Which region the keyboard is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Focus {
    Sidebar,
    #[default]
    Terminal,
}

/// How many toasts stack at once before the oldest is dropped.
pub const MAX_NOTICES: usize = 3;

/// How many notices the *log* keeps. Deeper than the toast stack, because the
/// log dialog is where a user goes to find out what happened while they were
/// looking elsewhere, and three rows is not a history.
pub const MAX_LOG: usize = 200;

/// Everything one frame paints.
///
/// Not `Clone`: the session owns a live link, and a second copy of a session is
/// not a second session — it is a second handle on the same wire. A frame that
/// needs to be kept is a *rendering*, and `ui::render_to_text` is how one is
/// taken.
#[derive(Debug)]
pub struct App {
    pub lang: Lang,
    pub theme: ThemeChoice,
    pub view: View,
    pub state: ConnectionState,
    pub focus: Focus,
    pub transport: Transport,
    /// True when a CLI flag pinned the transport, so the sidebar says `locked`.
    pub transport_locked: bool,
    /// The device the session targets (or, once connected, the one it found).
    pub device_name: String,
    pub rx: u64,
    pub tx: u64,
    /// The link speed once the device has reported one.
    pub baud: Option<u32>,
    /// The BLE link level the device reports (`L1`, `L2`). Unknown until the
    /// link exists, which is why the chrome shows a placeholder while dialling.
    pub link_level: Option<u8>,
    /// The signal the link is carrying, in dBm. Also unknown until connected.
    pub signal_dbm: Option<i32>,
    pub autoscroll: bool,
    /// `HH:MM:SS`, filled by the event loop so a frame is a pure function.
    pub clock: String,
    /// Which selectable sidebar row the keyboard is on. The rows are numbered
    /// by [`crate::ui::sidebar::targets`], so a cursor can never point at a row
    /// that is not selectable.
    pub sidebar_cursor: usize,
    /// Marches while an operation is in flight; drives the busy indicator in
    /// the top bar. The event loop advances it, a frame only reads it.
    pub tick: usize,
    pub quick_send: Vec<QuickSend>,
    /// Findings from the panic/reboot watch.
    pub watch: Vec<String>,
    pub notices: Vec<Notice>,
    /// What the device last said when asked `@i?`, and whether a read is out.
    pub diagnostics: DiagnosticsState,
    /// WiFi/WebDAV management state for the network view.
    pub network: crate::views::network::NetworkState,
    /// Transfer records for the transfer view.
    pub transfer: crate::views::transfer::TransferState_,
    /// The ZMODEM transfer in flight, if any.
    ///
    /// Held as one value rather than as "an engine and a flag": a direction kept
    /// beside a session is a direction that can outlive it. While this is
    /// `Some`, the link belongs to the transfer and the device has no keyboard
    /// — see [`App::send_bytes`].
    pub live_transfer: Option<LiveTransfer>,
    /// The assistant's session state for the assistant view.
    pub assistant: crate::views::assistant::AssistantState,
    /// The device's own output, as the serial pane shows it.
    pub terminal: TerminalPane,
    /// The live link, when one is open. Owns the threads and the counters; the
    /// frame only ever reads what the pump folded out of it.
    pub session: Session,
    /// The notice log: every announcement that has been made, oldest first.
    /// The toasts read [`App::notices`]; the log dialog reads this.
    pub log: Vec<Notice>,
    /// The command palette, when it has taken the keyboard.
    pub palette: Option<palette::PaletteState>,
    /// The modal that has taken the keyboard, if any. A dialog is at most one.
    pub dialog: Option<dialogs::Dialog>,
    /// The dial the session last opened, so `connect` has something to re-dial.
    /// Kept rather than re-derived from the command line, because the keyboard
    /// may have changed the transport since.
    pub last_dial: Option<Dial>,
    /// Set by `quit` (the palette action and the chord share it), read by the
    /// event loop. A flag rather than a return value, so an action run from the
    /// palette can end the session the same way a keypress does.
    pub quit: bool,
}

impl Default for App {
    fn default() -> App {
        App {
            lang: Lang::default(),
            theme: ThemeChoice::default(),
            view: View::Terminal,
            state: ConnectionState::Disconnected,
            focus: Focus::default(),
            transport: Transport::default(),
            transport_locked: false,
            device_name: "Linkr BLE UART".to_string(),
            rx: 0,
            tx: 0,
            baud: None,
            link_level: None,
            signal_dbm: None,
            autoscroll: true,
            clock: "00:00:00".to_string(),
            sidebar_cursor: 0,
            tick: 0,
            quick_send: vec![
                QuickSend::new("help"),
                QuickSend::new("version"),
                QuickSend::new("uname -a"),
                QuickSend::new("df -h"),
                QuickSend::guarded("reboot"),
            ],
            watch: Vec::new(),
            notices: Vec::new(),
            diagnostics: DiagnosticsState::default(),
            network: crate::views::network::NetworkState::default(),
            transfer: crate::views::transfer::TransferState_::default(),
            live_transfer: None,
            assistant: crate::views::assistant::AssistantState::default(),
            terminal: TerminalPane::new(),
            session: Session::new(),
            log: Vec::new(),
            palette: None,
            dialog: None,
            last_dial: None,
            quit: false,
        }
    }
}

impl App {
    /// The theme in force.
    pub fn theme(&self) -> Theme {
        Theme::for_choice(self.theme)
    }

    /// The current language's table.
    pub fn strings(&self) -> &'static crate::i18n::Strings {
        self.lang.strings()
    }

    /// A view's title in the current language.
    pub fn view_name(&self) -> &'static str {
        self.lang.view_name(self.view)
    }

    /// The connection state as text, in the current language.
    pub fn state_name(&self) -> &'static str {
        self.lang.state_name(self.state)
    }

    /// Whether management commands can go out at all. They travel the BLE
    /// management channel only, so a LAN session cannot reach them however
    /// connected it is — the diagnostics view says so instead of failing later.
    pub fn ble_connected(&self) -> bool {
        self.state == ConnectionState::Connected && self.transport == Transport::Ble
    }

    /// Whether the device can be written to.
    pub fn connected(&self) -> bool {
        self.state == ConnectionState::Connected && self.session.is_live()
    }

    /// The transport as the sidebar shows it, with its lock suffix when a CLI
    /// flag pinned it.
    pub fn transport_text(&self) -> String {
        let s = self.strings();
        if self.transport_locked {
            format!("{} {}", self.transport.label(), s.transport_locked)
        } else {
            self.transport.label().to_string()
        }
    }

    /// The placeholder a slot shows while the device has not reported a value.
    pub fn unknown(&self) -> &'static str {
        self.strings().value_unknown
    }

    /// The baud slot: the speed once known, the placeholder before that.
    pub fn baud_text(&self) -> String {
        match self.baud {
            Some(baud) => baud.to_string(),
            None => self.unknown().to_string(),
        }
    }

    /// The link level, as the chrome names it (`link L1`), or the placeholder
    /// while there is no link to report one.
    pub fn link_text(&self) -> String {
        match self.link_level {
            Some(level) => format!("{}{level}", self.strings().diag_link_level),
            None => self.unknown().to_string(),
        }
    }

    /// The signal reading, or the placeholder while there is none.
    pub fn signal_text(&self) -> String {
        match self.signal_dbm {
            Some(dbm) => format!("{dbm} {}", self.strings().signal_unit),
            None => self.unknown().to_string(),
        }
    }

    /// The glyph for the current state. The chrome and the sidebar row both ask
    /// this, so one frame can never show two different glyphs for one state.
    pub fn state_glyph(&self) -> &'static str {
        self.state.glyph()
    }

    /// Toggle the theme and return the notice that announces it, so the caller
    /// can log exactly one message and the wording stays in one place.
    pub fn toggle_theme(&mut self) -> Notice {
        self.theme = self.theme.toggled();
        let s = self.strings();
        let name = match self.theme {
            ThemeChoice::Dark => s.theme_dark,
            ThemeChoice::Light => s.theme_light,
        };
        Notice {
            text: format!("{}: {}", s.theme_switched_prefix, name),
            level: NoticeLevel::Info,
        }
    }

    /// Show a notice, dropping the oldest when either stack is full.
    ///
    /// One door for both: the toast stack is what is on screen and the log is
    /// what happened. Announcing through a second path would let the two
    /// disagree about the same event.
    pub fn push_notice(&mut self, notice: Notice) {
        self.notices.push(notice.clone());
        while self.notices.len() > MAX_NOTICES {
            self.notices.remove(0);
        }
        self.log.push(notice);
        while self.log.len() > MAX_LOG {
            self.log.remove(0);
        }
    }

    /// Whether a modal has taken the keyboard, so the event loop can route the
    /// key to the overlay instead of the pane.
    pub fn overlay_open(&self) -> bool {
        self.dialog.is_some() || self.palette.is_some()
    }

    /// Switch the interface language, and return the notice announcing it.
    pub fn toggle_language(&mut self) -> Notice {
        self.lang = match self.lang {
            Lang::Zh => Lang::En,
            Lang::En => Lang::Zh,
        };
        let s = self.strings();
        let name = match self.lang {
            Lang::Zh => s.language_zh,
            Lang::En => s.language_en,
        };
        Notice {
            text: format!("{}: {}", s.language_switched_prefix, name),
            level: NoticeLevel::Info,
        }
    }

    /// Cycle the transport to the next link, and return the notice announcing
    /// it. A transport the command line pinned cannot be switched: the flag
    /// decided, and a keypress that quietly overrode it would make the
    /// sidebar's `locked` suffix a lie.
    pub fn toggle_transport(&mut self) -> Notice {
        if self.transport_locked {
            return Notice {
                text: self.strings().transport_is_locked.to_string(),
                level: NoticeLevel::Warn,
            };
        }
        let index = Transport::ALL
            .iter()
            .position(|kind| *kind == self.transport)
            .unwrap_or(0);
        self.transport = Transport::ALL[(index + 1) % Transport::ALL.len()];
        Notice {
            text: format!(
                "{}: {}",
                self.strings().transport_label,
                self.transport.label()
            ),
            level: NoticeLevel::Info,
        }
    }

    /// Ask the event loop to stop. A flag rather than a return value, so the
    /// palette's `quit` action and the `Ctrl+Q` chord end the session the same
    /// way.
    pub fn request_quit(&mut self) {
        self.quit = true;
    }

    /// The file a log save goes to when the user does not name one: the working
    /// directory and a stamp, so two saves never overwrite each other.
    pub fn terminal_log_path() -> String {
        format!(
            "linkr-terminal-{}.log",
            chrono::Local::now().format("%Y%m%d-%H%M%S")
        )
    }

    /// Write the pane's retained output to `path`, returning the bytes written
    /// and the name they went to.
    pub fn save_terminal_log_to(&self, path: &std::path::Path) -> io::Result<(usize, String)> {
        let bytes = self.terminal.save_log(path)?;
        Ok((bytes, path.display().to_string()))
    }

    /// Save the pane's log to the default path.
    pub fn save_terminal_log(&self) -> io::Result<(usize, String)> {
        let path = Self::terminal_log_path();
        self.save_terminal_log_to(std::path::Path::new(&path))
    }

    /// Select a view.
    pub fn set_view(&mut self, view: View) {
        self.view = view;
    }

    /// How many sidebar rows the cursor can reach: the transport row, the two
    /// connection actions, one row per view, and one per quick send. This is
    /// the same number [`crate::ui::sidebar::targets`] produces, and a test
    /// pins the two together so navigation can never walk off the list.
    pub fn sidebar_rows(&self) -> usize {
        3 + View::ALL.len() + self.quick_send.len()
    }

    /// Move the sidebar cursor down one row, stopping on the last.
    pub fn cursor_down(&mut self) {
        self.sidebar_cursor = self
            .sidebar_cursor
            .saturating_add(1)
            .min(self.last_cursor());
    }

    /// Move the sidebar cursor up one row, stopping on the first.
    pub fn cursor_up(&mut self) {
        self.sidebar_cursor = self.sidebar_cursor.saturating_sub(1);
    }

    /// The index of the last selectable row; zero when the list is empty, so a
    /// degenerate sidebar still leaves a valid cursor.
    fn last_cursor(&self) -> usize {
        self.sidebar_rows().saturating_sub(1)
    }

    /// Step to the next view in the rota, wrapping at the end.
    pub fn next_view(&mut self) {
        let index = View::ALL.iter().position(|v| *v == self.view).unwrap_or(0);
        self.view = View::ALL[(index + 1) % View::ALL.len()];
    }

    /// Step to the previous view in the rota, wrapping at the start.
    pub fn prev_view(&mut self) {
        let index = View::ALL.iter().position(|v| *v == self.view).unwrap_or(0);
        let last = View::ALL.len() - 1;
        self.view = View::ALL[(index + last) % View::ALL.len()];
    }

    // --- the link ----------------------------------------------------------

    /// Open a link and start the session. The sidebar's transport follows the
    /// link that was actually opened, so the chrome can never name a link other
    /// than the one carrying the session.
    pub fn connect(&mut self, link: Box<dyn Link>) -> io::Result<()> {
        let kind = link.kind();
        self.session.connect(link)?;
        self.transport = kind;
        self.state = ConnectionState::Connecting;
        Ok(())
    }

    /// Dial what the command line asked for.
    pub fn dial(&mut self, dial: &Dial) -> io::Result<()> {
        let link = dial.open()?;
        // Remembered only once the link opened: a dial that failed is not
        // something `connect` should try again behind the user's back.
        self.last_dial = Some(dial.clone());
        // A dial the command line decided cannot be switched from the keyboard,
        // which is exactly what the sidebar's `locked` suffix says.
        self.transport_locked = true;
        self.connect(link)
    }

    /// Open the link again, or say why there is nothing to open.
    ///
    /// This is the palette's `connect`: it re-dials the last link rather than
    /// inventing one, so the pair of actions can never dial something the
    /// command line did not name.
    pub fn reconnect(&mut self) {
        if self.session.is_live() {
            return;
        }
        let Some(dial) = self.last_dial.clone() else {
            let text = self.strings().no_link.to_string();
            self.push_notice(Notice {
                text,
                level: NoticeLevel::Warn,
            });
            return;
        };
        if let Err(err) = self.dial(&dial) {
            let text = format!("{}{err}", self.strings().link_failed);
            self.push_notice(Notice {
                text,
                level: NoticeLevel::Error,
            });
        }
    }

    /// Hang up, and say so once. Announcing it here rather than in the pump is
    /// what keeps a deliberate hang-up from reading as a dropped link.
    pub fn disconnect(&mut self) {
        if !self.session.is_live() {
            return;
        }
        let name = self.session.describe().to_string();
        self.session.close();
        self.state = ConnectionState::Disconnected;
        let text = format!("{}{}", self.strings().link_closed, name);
        self.push_notice(Notice {
            text,
            level: NoticeLevel::Info,
        });
    }

    /// One pump: fold whatever the link produced into the frame, and announce
    /// the two transitions that are worth a toast.
    ///
    /// The session already folded the counters and the state; what is left is
    /// the *edge*: a link that came up and a link that went away. Announcing
    /// the state instead of the transition would raise a toast on every tick.
    pub fn pump(&mut self) {
        let was_live = self.session.is_live();
        let was_connected = self.state == ConnectionState::Connected;
        let describe = self.session.describe().to_string();
        // While a transfer owns the link the device's bytes are a protocol
        // dialogue, not output. They go to the codec and the codec's answers go
        // back out; a pane that also painted them would show a screenful of
        // escapes, and a pane that answered one of the emulator's own questions
        // would inject a report into the middle of a block.
        let mut outbound = Vec::new();
        for event in self.session.pump() {
            match event {
                LinkEvent::Data(bytes) => match self.live_transfer.as_mut() {
                    Some(live) => outbound.extend(live.feed(&bytes)),
                    None => self.terminal.push(&bytes),
                },
                // Something the far end said in words: a bridge announcement,
                // not target output. It is shown and never typed, and it is
                // scrubbed first because what a bridge says about itself is
                // where its access token would be.
                LinkEvent::Notice(text) => {
                    let notice = Notice {
                        text: crate::lan_token_store::redact(&text),
                        level: NoticeLevel::Info,
                    };
                    self.push_notice(notice);
                }
                LinkEvent::Opened | LinkEvent::Closed(_) => {}
            }
        }
        if self.live_transfer.is_none() {
            // A program that asks a question — where is the cursor, how wide is
            // the screen — blocks until it hears back, so the emulator's answers
            // go out on the tick that produced them. Always *after* the feed: an
            // answer is about the screen those bytes have just painted. With no
            // link they are dropped, which is the right way round — a cursor
            // report delivered late is worse than one never sent, because the
            // program that asked has already given up and moved on.
            for reply in self.terminal.take_replies() {
                self.session.send(&reply);
            }
        }
        self.fold_transfer();
        // Written after the fold, so a session that ended on this tick still
        // gets its closing frames out: the sign-off is part of the dialogue, not
        // a consequence of it. One write for the whole tick, because it is one
        // byte stream either way.
        if !outbound.is_empty() {
            self.session.send(&outbound);
        }
        let now_live = self.session.is_live();
        if let Some(state) = self.session.state() {
            self.state = state;
        }
        if !was_connected && self.state == ConnectionState::Connected {
            let text = format!("{}{}", self.strings().link_opened, describe);
            self.push_notice(Notice {
                text,
                level: NoticeLevel::Info,
            });
        } else if was_live && !now_live {
            let s = self.strings();
            let (prefix, level) = if self.state == ConnectionState::Failed {
                (s.link_failed, NoticeLevel::Error)
            } else {
                (s.link_closed, NoticeLevel::Warn)
            };
            let text = format!("{prefix}{describe}");
            self.push_notice(Notice { text, level });
        }
        if !now_live && self.live_transfer.is_some() {
            // The wire went away with a file half moved. Give up on it and say
            // which: a transfer that kept waiting for a link that no longer
            // exists would sit in the view reading "running" for ever.
            let prefix = self.strings().xfer_cancelled;
            self.end_transfer(prefix, NoticeLevel::Warn);
        }
        self.rx = self.session.rx();
        self.tx = self.session.tx();
    }

    /// Send raw bytes. Silent when there is no link, because this is the path a
    /// *keystroke* takes and a toast per character would bury the screen.
    pub fn send_bytes(&mut self, bytes: &[u8]) -> bool {
        // A transfer owns the wire. A byte typed into a ZMODEM dialogue is not
        // input, it is corruption in the middle of a block — and the CRCs do not
        // catch it: they check the far end's data, not what this end
        // interleaved with it.
        if !self.connected() || self.transferring() {
            return false;
        }
        if !self.session.send(bytes) {
            return false;
        }
        // The counter is part of the *frame*, so it moves here rather than at
        // the next pump: a keystroke's byte should show in the top bar on the
        // same repaint as the character it produced.
        self.tx = self.session.tx();
        if self.terminal.echo {
            self.terminal.push(bytes);
        }
        true
    }

    /// Send a whole command, and say so when there is nothing to send it over.
    ///
    /// One-shot commands go through this door: `send_bytes` refusing in
    /// silence is right for a keystroke and wrong for a preset, where the user
    /// pressed a button and is owed an answer.
    pub fn send_command(&mut self, text: &str) -> bool {
        // The transfer is checked first, so a preset pressed while one is
        // running is told what is actually in the way rather than "not
        // connected" — which would send the user looking at the wrong thing.
        if self.transferring() {
            let message = self.strings().xfer_busy.to_string();
            self.push_notice(Notice {
                text: message,
                level: NoticeLevel::Warn,
            });
            return false;
        }
        if !self.connected() {
            let message = self.strings().not_connected.to_string();
            self.push_notice(Notice {
                text: message,
                level: NoticeLevel::Warn,
            });
            return false;
        }
        let mut payload = text.as_bytes().to_vec();
        if !payload.ends_with(b"\n") {
            payload.push(b'\r');
        }
        self.send_bytes(&payload)
    }

    /// Throw the pane's history away. The device is not told.
    pub fn clear_terminal(&mut self) {
        self.terminal.clear();
    }

    /// Keep the pane pinned to the newest row again.
    pub fn scroll_terminal_to_bottom(&mut self) {
        self.terminal.autoscroll = true;
        self.terminal.scroll = 0;
    }

    /// Walk the pane back through its history. Leaving the bottom turns
    /// autoscroll off, which is what a terminal does: the user asked to stop
    /// following.
    pub fn scroll_terminal(&mut self, lines_up: usize) {
        if lines_up == 0 {
            return;
        }
        self.terminal.autoscroll = false;
        self.terminal.scroll = self.terminal.scroll.saturating_add(lines_up);
    }

    // --- the transfer ------------------------------------------------------

    /// Whether a transfer owns the link right now.
    ///
    /// The keyboard asks this. While a transfer is in flight, a byte typed into
    /// the device is not a keystroke — it is corruption in the middle of a
    /// block, and ZMODEM will not notice: the CRCs check the far end's data, not
    /// what this end interleaved with it.
    pub fn transferring(&self) -> bool {
        self.live_transfer.is_some()
    }

    /// Offer a local file to the device.
    ///
    /// Refuses out loud rather than quietly doing nothing: the operator picked
    /// it from a list and is owed an answer. The `rz\r` wakeup goes out with the
    /// first frame, so the device's shell starts a receiver — which is what a
    /// real `sz` on the far side of this bridge does.
    pub fn start_send(&mut self, path: PathBuf) -> bool {
        if let Some(refusal) = self.transfer_refusal() {
            self.push_notice(refusal);
            return false;
        }
        match LiveTransfer::send(vec![path]) {
            Ok(live) => {
                self.begin_transfer(live);
                true
            }
            Err(err) => {
                let text = format!("{}{err}", self.strings().xfer_start_failed);
                self.push_notice(Notice {
                    text,
                    level: NoticeLevel::Error,
                });
                false
            }
        }
    }

    /// Take whatever the device offers into `dir`.
    ///
    /// The far end has to be *sending*: this end announces a receiver and waits
    /// for the `sz` on the other side to answer, which is why nothing appears
    /// until the device's first file does.
    pub fn start_receive(&mut self, dir: PathBuf) -> bool {
        if let Some(refusal) = self.transfer_refusal() {
            self.push_notice(refusal);
            return false;
        }
        self.begin_transfer(LiveTransfer::receive(dir));
        true
    }

    /// Give up on the transfer in flight, and leave nothing half-written.
    pub fn cancel_transfer(&mut self) {
        if self.live_transfer.is_none() {
            return;
        }
        let prefix = self.strings().xfer_cancelled;
        self.end_transfer(prefix, NoticeLevel::Warn);
    }

    /// Why a transfer cannot start, as a notice the caller can show. `None` when
    /// it can.
    fn transfer_refusal(&self) -> Option<Notice> {
        if self.transferring() {
            return Some(Notice {
                text: self.strings().xfer_busy.to_string(),
                level: NoticeLevel::Warn,
            });
        }
        if !self.connected() {
            return Some(Notice {
                text: self.strings().xfer_needs_link.to_string(),
                level: NoticeLevel::Warn,
            });
        }
        None
    }

    /// Store the session and write its opening frames.
    fn begin_transfer(&mut self, mut live: LiveTransfer) {
        let bytes = live.opening_bytes();
        let (upload, target) = (live.upload, live.target.clone());
        self.live_transfer = Some(live);
        if !bytes.is_empty() {
            self.session.send(&bytes);
        }
        let s = self.strings();
        let (prefix, level) = if upload {
            (s.xfer_started_out, NoticeLevel::Info)
        } else {
            (s.xfer_started_in, NoticeLevel::Info)
        };
        let text = format!("{prefix}{target}");
        self.push_notice(Notice { text, level });
    }

    /// Let the session go, discarding what it was writing.
    fn end_transfer(&mut self, prefix: &str, level: NoticeLevel) {
        let Some(live) = self.live_transfer.take() else {
            return;
        };
        // Before the engine is dropped: a cancelled download has a half file open
        // on disk, and a partial file that looks complete is the one outcome a
        // cancel must not leave behind.
        live.discard_partial();
        self.transfer.abandon();
        let text = format!("{prefix}{}", live.target);
        self.push_notice(Notice { text, level });
    }

    /// Fold the transfer in flight into the records and the counters, and let it
    /// go when it is over.
    fn fold_transfer(&mut self) {
        // The borrow of `live_transfer` has to end before the records can be
        // touched, so everything wanted from it is taken in one go.
        let (events, progress, upload, done) = match self.live_transfer.as_mut() {
            None => return,
            Some(live) => (
                live.take_events(),
                live.progress().cloned(),
                live.upload,
                live.is_done(),
            ),
        };
        for event in events {
            self.record_transfer(event, upload);
        }
        if let Some(progress) = progress {
            self.transfer
                .counted(&progress.name, upload, progress.bytes, progress.total);
        }
        if done {
            self.live_transfer = None;
            let text = self.strings().xfer_finished.to_string();
            self.push_notice(Notice {
                text,
                level: NoticeLevel::Info,
            });
        }
    }

    /// One thing that happened, as the records and the notices show it.
    ///
    /// `upload` is the session's, not the event's: a session runs one way, so a
    /// file that ends belongs to whichever direction the session is.
    fn record_transfer(&mut self, event: TransferEvent, upload: bool) {
        match event {
            TransferEvent::Started { name, total, .. } => {
                self.transfer.started(name, total, upload);
            }
            TransferEvent::Saved { name, bytes } => {
                self.transfer
                    .ended(&name, upload, TransferState::Saved, Some(bytes));
                let text = format!(
                    "{}{} · {}",
                    self.strings().xfer_saved_notice,
                    name,
                    human_bytes(bytes)
                );
                self.push_notice(Notice {
                    text,
                    level: NoticeLevel::Info,
                });
            }
            TransferEvent::Failed { name, reason } => {
                // A session that failed before any file was opened has no name to
                // put on a row; the reason is the whole of what is known, and a
                // row with a blank name would read as a file called "".
                if !name.is_empty() {
                    self.transfer
                        .ended(&name, upload, TransferState::Failed, None);
                }
                let text = if name.is_empty() {
                    format!("{}{reason}", self.strings().xfer_failed_notice)
                } else {
                    format!("{}{name} · {reason}", self.strings().xfer_failed_notice)
                };
                self.push_notice(Notice {
                    text,
                    level: NoticeLevel::Error,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The defaults are the frame the screenshots show: the serial terminal
    /// selected, autoscroll on, the five quick sends with only `reboot` guarded.
    #[test]
    fn defaults_match_the_designed_first_frame() {
        let app = App::default();
        assert_eq!(app.view, View::Terminal);
        assert_eq!(app.focus, Focus::Terminal);
        assert_eq!(app.transport, Transport::Ble);
        assert!(!app.transport_locked);
        assert!(app.autoscroll);
        assert_eq!(app.baud, None);
        assert_eq!(app.link_level, None);
        assert_eq!(app.signal_dbm, None);
        assert_eq!(app.rx, 0);
        assert_eq!(app.tx, 0);
        assert!(app.notices.is_empty());
        assert!(app.watch.is_empty());
        let commands: Vec<&str> = app.quick_send.iter().map(|q| q.command.as_str()).collect();
        assert_eq!(
            commands,
            vec!["help", "version", "uname -a", "df -h", "reboot"]
        );
        let guarded: Vec<&str> = app
            .quick_send
            .iter()
            .filter(|q| q.dangerous)
            .map(|q| q.command.as_str())
            .collect();
        assert_eq!(guarded, vec!["reboot"]);
    }

    /// The transport row shows the lock only when a flag pinned the link.
    #[test]
    fn the_transport_row_carries_its_lock() {
        let mut app = App {
            lang: Lang::En,
            ..App::default()
        };
        assert_eq!(app.transport_text(), "BLE");
        app.transport_locked = true;
        assert_eq!(app.transport_text(), "BLE (locked)");
        app.transport = Transport::Lan;
        assert_eq!(app.transport_text(), "LAN (locked)");
    }

    /// The baud slot is the placeholder until a speed is known, then the speed.
    #[test]
    fn the_baud_slot_fills_in_once_known() {
        let mut app = App::default();
        assert_eq!(app.baud_text(), app.unknown());
        assert_eq!(app.baud_text(), "─");
        app.baud = Some(115_200);
        assert_eq!(app.baud_text(), "115200");
    }

    /// A theme toggle flips the choice and returns the notice for it, in the
    /// current language — the one place the wording is written.
    #[test]
    fn toggling_the_theme_announces_the_new_one() {
        let mut app = App {
            lang: Lang::Zh,
            ..App::default()
        };
        let notice = app.toggle_theme();
        assert_eq!(app.theme, ThemeChoice::Light);
        assert_eq!(notice.text, "主题: 浅色");
        assert_eq!(notice.level, NoticeLevel::Info);
        let notice = app.toggle_theme();
        assert_eq!(app.theme, ThemeChoice::Dark);
        assert_eq!(notice.text, "主题: 深色");

        app.lang = Lang::En;
        let notice = app.toggle_theme();
        assert_eq!(notice.text, "Theme: light");
    }

    /// Toasts are bounded: the stack never grows past the cap, and the oldest
    /// is the one dropped.
    #[test]
    fn notices_are_bounded_and_drop_the_oldest() {
        let mut app = App::default();
        for i in 0..MAX_NOTICES + 2 {
            app.push_notice(Notice {
                text: format!("n{i}"),
                level: NoticeLevel::Info,
            });
        }
        assert_eq!(app.notices.len(), MAX_NOTICES);
        let texts: Vec<&str> = app.notices.iter().map(|n| n.text.as_str()).collect();
        assert_eq!(texts, vec!["n2", "n3", "n4"]);
    }

    /// The view rota walks all five and wraps in both directions: no view is
    /// unreachable and none is skipped.
    #[test]
    fn the_view_rota_wraps_both_ways() {
        let mut app = App::default();
        let mut seen = vec![app.view];
        for _ in 0..View::ALL.len() - 1 {
            app.next_view();
            seen.push(app.view);
        }
        assert_eq!(seen, View::ALL.to_vec());
        app.next_view();
        assert_eq!(app.view, View::ALL[0], "next wraps to the first view");
        app.prev_view();
        assert_eq!(
            app.view,
            View::ALL[View::ALL.len() - 1],
            "prev wraps to the last"
        );
    }

    /// The cursor stops at the last row and never walks past it — a cursor
    /// pointing at nothing would leave the sidebar with no highlighted row and
    /// the keyboard with nowhere to go.
    #[test]
    fn the_cursor_stops_at_both_ends() {
        let mut app = App::default();
        let last = app.sidebar_rows() - 1;
        for _ in 0..last + 10 {
            app.cursor_down();
        }
        assert_eq!(app.sidebar_cursor, last);
        for _ in 0..last + 10 {
            app.cursor_up();
        }
        assert_eq!(app.sidebar_cursor, 0);
    }

    /// The count the cursor clamps to is the count the sidebar actually
    /// offers: the two are computed apart, so a test holds them together.
    #[test]
    fn the_cursor_range_matches_the_sidebar() {
        let mut app = App::default();
        assert_eq!(app.sidebar_rows(), crate::ui::sidebar::targets(&app).len());
        app.quick_send.clear();
        assert_eq!(app.sidebar_rows(), crate::ui::sidebar::targets(&app).len());
        app.quick_send.push(QuickSend::new("ls"));
        assert_eq!(app.sidebar_rows(), crate::ui::sidebar::targets(&app).len());
    }

    /// A first frame has nothing from the device: the diagnostics grid starts
    /// empty, not with a stale read.
    #[test]
    fn the_diagnostics_grid_starts_empty() {
        let app = App::default();
        assert!(!app.diagnostics.has_data());
        assert!(!app.diagnostics.done);
        assert!(!app.diagnostics.loading);
        assert_eq!(app.diagnostics.updated, None);
    }

    /// Management commands are BLE-only: a LAN session is connected and still
    /// unable to reach them, which is what the diagnostics view reports.
    #[test]
    fn management_commands_need_the_ble_link() {
        let mut app = App {
            state: ConnectionState::Connected,
            ..App::default()
        };
        assert!(app.ble_connected(), "a connected BLE session can manage");
        app.transport = Transport::Lan;
        assert!(!app.ble_connected(), "a LAN session cannot");
        app.state = ConnectionState::Connecting;
        app.transport = Transport::Ble;
        assert!(!app.ble_connected(), "dialling is not connected");
        app.state = ConnectionState::Failed;
        assert!(!app.ble_connected());
        app.state = ConnectionState::Disconnected;
        assert!(!app.ble_connected());
    }

    /// Theme and language shortcuts read the state, so a view never has to know
    /// which of the two enums is in force.
    #[test]
    fn shortcuts_read_the_current_selection() {
        let mut app = App {
            lang: Lang::En,
            ..App::default()
        };
        assert_eq!(app.view_name(), "Serial Terminal");
        assert_eq!(app.state_name(), "disconnected");
        app.lang = Lang::Zh;
        app.view = View::Network;
        app.state = ConnectionState::Connecting;
        assert_eq!(app.view_name(), "网络");
        assert_eq!(app.state_name(), "连接中…");
        assert_eq!(app.theme(), Theme::DARK);
        app.theme = ThemeChoice::Light;
        assert_eq!(app.theme(), Theme::LIGHT);
    }

    /// Pump until `predicate` holds, up to two seconds. The pump is what moves
    /// the session along, so a test has to keep calling it.
    fn pump_until(app: &mut App, mut predicate: impl FnMut(&App) -> bool) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            app.pump();
            if predicate(app) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        app.pump();
        predicate(app)
    }

    /// The whole non-terminal path, end to end: dial a bridge, watch the link
    /// come up, type a command, and read the answer out of the pane.
    ///
    /// This is the test that says the transport is *wired in* rather than
    /// merely written: every piece here — the dial, the reader thread, the
    /// session's counters, the pane's decoder and the notices — is the code the
    /// TUI runs, with only the terminal itself replaced by assertions.
    #[test]
    fn a_dialled_bridge_reaches_the_pane_and_the_counters() {
        let mut app = App {
            lang: Lang::En,
            ..App::default()
        };
        app.dial(&Dial::Stdio("-".to_string()))
            .expect("the loopback opens");

        assert_eq!(
            app.transport,
            Transport::Stdio,
            "the sidebar follows the link"
        );
        assert!(app.transport_locked, "a dialled link cannot be switched");
        assert_eq!(app.state, ConnectionState::Connecting);

        assert!(pump_until(&mut app, |app| app.state == ConnectionState::Connected));
        assert!(pump_until(&mut app, |app| !app.terminal.is_empty()));
        assert!(
            app.terminal.visible_text().contains("loopback device"),
            "the banner never reached the pane: {:?}",
            app.terminal.visible_text()
        );
        assert!(app.rx > 0, "the receive counter moved with the bytes");
        assert!(
            app.notices
                .iter()
                .any(|notice| notice.text.contains("link open")),
            "the link coming up is announced: {:?}",
            app.notices
        );

        // A keystroke goes out and the device's answer comes back.
        assert!(app.send_command("uname -a"));
        assert_eq!(app.tx, 9, "the carriage return is part of what was sent");
        assert!(pump_until(&mut app, |app| app
            .terminal
            .visible_text()
            .contains("aarch64")));
    }

    /// A device's *questions* are answered on the wire.
    ///
    /// This is the half of a terminal that is easy to forget, and what it hides
    /// is a hang rather than a wrong pixel: `ESC [ 6 n` is a *blocking* read for
    /// the program that sends it, so an emulator that paints the screen and never
    /// replies leaves that program waiting forever.
    ///
    /// The memory link is what makes this testable. It is a link the test can
    /// also read back from, so the bytes that went out are asserted rather than
    /// inferred from a screen.
    #[test]
    fn a_devices_questions_are_answered_on_the_wire() {
        let mut app = App {
            lang: Lang::En,
            ..App::default()
        };
        app.terminal.set_size(80, 24);
        let wire = crate::transport::testing::MemoryLink::new();
        app.connect(wire.link()).expect("the memory link opens");
        assert!(pump_until(&mut app, |app| app.connected()));
        assert!(wire.take_sent().is_empty(), "nothing has been asked yet");

        // "Where is the cursor?" and "how big is the text area?" — six bytes of
        // answer for the first, ten for the second. The wait is on the *wire*,
        // not on the counter: the writer is its own thread, so the counter moves
        // at the hand-off.
        wire.feed(b"\x1b[6n\x1b[18t");
        assert!(pump_until(&mut app, |_| wire.sent_len() >= 16));
        assert_eq!(
            wire.take_sent(),
            b"\x1b[1;1R\x1b[8;24;80t".to_vec(),
            "the cursor report and the size report go back out"
        );
        assert_eq!(app.tx, 16, "and the byte counter counted them");
        assert!(app.terminal.vt().is_blank(), "a question is not text");
    }

    /// A command with no link is refused *out loud*: a preset that vanished
    /// into nothing reads as success, which for `reboot` is the worst lie the
    /// interface can tell.
    #[test]
    fn a_command_with_no_link_is_refused_out_loud() {
        let mut app = App {
            lang: Lang::En,
            ..App::default()
        };
        // Even a frame that *claims* connected cannot send: the link is what
        // decides, not the status dot.
        app.state = ConnectionState::Connected;

        assert!(!app.send_command("reboot"));
        assert_eq!(app.tx, 0, "nothing was counted");
        assert_eq!(
            app.notices.last().map(|notice| notice.text.as_str()),
            Some("Not connected — nothing was sent.")
        );
        // A keystroke, by contrast, is dropped without a word: a toast per
        // character would bury the screen.
        let before = app.notices.len();
        assert!(!app.send_bytes(b"x"));
        assert_eq!(app.notices.len(), before);
    }

    /// Hanging up deliberately is announced once — and the pump does not
    /// announce it again, which would turn one hang-up into a stream of toasts.
    #[test]
    fn a_deliberate_hang_up_is_announced_once() {
        let mut app = App {
            lang: Lang::En,
            ..App::default()
        };
        app.dial(&Dial::Stdio("-".to_string())).expect("opens");
        assert!(pump_until(&mut app, |app| app.state == ConnectionState::Connected));

        let before = app.notices.len();
        app.disconnect();
        assert!(!app.session.is_live(), "the link is gone");
        assert_eq!(app.state, ConnectionState::Disconnected);
        assert_eq!(app.notices.len(), before + 1);
        assert!(
            app.notices
                .last()
                .is_some_and(|notice| notice.text.contains("link closed")),
            "{:?}",
            app.notices
        );

        app.pump();
        app.pump();
        assert_eq!(
            app.notices.len(),
            before + 1,
            "a second pump re-announced the hang-up: {:?}",
            app.notices
        );
    }

    /// A dial that cannot be built changes nothing: the frame keeps its own
    /// state and no session appears behind it.
    #[test]
    fn a_failed_dial_leaves_the_frame_alone() {
        use crate::transport::ble::Selection;

        let mut app = App {
            lang: Lang::En,
            ..App::default()
        };
        app.state = ConnectionState::Connecting;
        // An address made of nothing, so this fails without touching a radio:
        // the point of the test is what a refused dial does to the frame, not
        // whether BlueZ is reachable.
        assert!(app
            .dial(&Dial::Ble(Selection::Address("nonsense".to_string())))
            .is_err());
        assert!(!app.session.is_live());
        assert_eq!(app.state, ConnectionState::Connecting);
        assert_eq!(app.tx, 0);
    }

    /// The pane's scroll controls leave the bottom, which turns autoscroll off
    /// — a user who asked to read back did not ask to be dragged forward.
    #[test]
    fn scrolling_the_pane_leaves_the_bottom() {
        let mut app = App::default();
        app.terminal.push_text("one\ntwo\nthree\n");
        assert!(app.terminal.autoscroll);

        app.scroll_terminal(2);
        assert!(!app.terminal.autoscroll);
        assert_eq!(app.terminal.scroll, 2);
        app.scroll_terminal(3);
        assert_eq!(app.terminal.scroll, 5, "the scroll accumulates");

        app.scroll_terminal_to_bottom();
        assert!(app.terminal.autoscroll);
        assert_eq!(app.terminal.scroll, 0);
        // A zero scroll is not a scroll: it must not take the pane off the tail.
        app.scroll_terminal(0);
        assert!(app.terminal.autoscroll);
    }

    /// Every announcement lands in the log as well as the toast stack, and the
    /// log is the deeper of the two — a dialog that showed only the three
    /// toasts already on screen would be a second view of the same three rows.
    #[test]
    fn an_announcement_lands_in_the_log_too() {
        let mut app = App::default();
        for i in 0..MAX_NOTICES + 5 {
            app.push_notice(Notice {
                text: format!("n{i}"),
                level: NoticeLevel::Info,
            });
        }
        assert_eq!(app.notices.len(), MAX_NOTICES);
        assert_eq!(app.log.len(), MAX_NOTICES + 5);
        assert_eq!(
            app.log.first().map(|n| n.text.as_str()),
            Some("n0"),
            "the log keeps the oldest"
        );
        // The log bounds itself as well, so a long session cannot grow forever.
        for i in 0..MAX_LOG + 10 {
            app.push_notice(Notice {
                text: format!("x{i}"),
                level: NoticeLevel::Info,
            });
        }
        assert_eq!(app.log.len(), MAX_LOG);
    }

    /// The transport cycles through every link and wraps, so the row is never a
    /// dead end — but a transport the command line pinned refuses, and says so.
    #[test]
    fn the_transport_cycles_unless_it_is_locked() {
        let mut app = App {
            lang: Lang::En,
            ..App::default()
        };
        let start = app.transport;
        let mut seen = vec![start];
        for _ in 1..Transport::ALL.len() {
            let notice = app.toggle_transport();
            assert_eq!(notice.level, NoticeLevel::Info);
            seen.push(app.transport);
        }
        seen.sort_by_key(|kind| Transport::ALL.iter().position(|other| other == kind));
        assert_eq!(seen, Transport::ALL.to_vec(), "every link is reachable");
        app.toggle_transport();
        assert_eq!(app.transport, start, "and it wraps back to where it began");

        app.transport_locked = true;
        let before = app.transport;
        let notice = app.toggle_transport();
        assert_eq!(app.transport, before, "a pinned transport does not move");
        assert_eq!(notice.level, NoticeLevel::Warn);
        assert_eq!(notice.text, "The transport is pinned by the command line.");
    }

    /// Switching the language flips it, says so in the *new* language, and the
    /// two names are each written in their own language — the way a language
    /// picker names a language.
    #[test]
    fn switching_the_language_says_so() {
        let mut app = App {
            lang: Lang::Zh,
            ..App::default()
        };
        let notice = app.toggle_language();
        assert_eq!(app.lang, Lang::En);
        assert_eq!(notice.text, "Interface language: English");
        let notice = app.toggle_language();
        assert_eq!(app.lang, Lang::Zh);
        assert_eq!(notice.text, "界面语言: 中文");
    }

    /// A save writes exactly what the pane shows, to the name it reports.
    #[test]
    fn a_save_writes_the_pane_to_disk() {
        let mut app = App::default();
        app.terminal.push_text("uname -a\r\nLinux bee\r\n");
        let path =
            std::env::temp_dir().join(format!("linkr-tui-save-test-{}.log", std::process::id()));
        let (bytes, name) = app.save_terminal_log_to(&path).expect("the save succeeds");
        let written = std::fs::read_to_string(&path).expect("the file exists");
        assert_eq!(bytes, written.len());
        assert_eq!(name, path.display().to_string());
        assert!(written.contains("Linux bee"), "{written}");
        let _ = std::fs::remove_file(&path);

        // The default name is a plain file in the working directory, stamped so
        // two saves cannot collide.
        let default = App::terminal_log_path();
        assert!(default.starts_with("linkr-terminal-"), "{default}");
        assert!(default.ends_with(".log"), "{default}");
        assert!(!default.contains('/'), "{default}");
    }

    /// `connect` with nothing to re-dial says so instead of doing nothing: a
    /// button that silently does nothing reads as broken.
    #[test]
    fn reconnect_without_a_dial_says_so() {
        let mut app = App {
            lang: Lang::En,
            ..App::default()
        };
        app.reconnect();
        assert_eq!(
            app.log.last().map(|notice| notice.text.as_str()),
            Some("No link configured — nothing to dial.")
        );
        assert!(app.last_dial.is_none());
    }

    /// A dialled link is remembered, and `connect` opens it again rather than
    /// inventing one — the palette's `connect` and the command line name the
    /// same link.
    #[test]
    fn a_dialled_link_is_remembered_and_reopened() {
        let mut app = App {
            lang: Lang::En,
            ..App::default()
        };
        app.dial(&Dial::Stdio("-".into()))
            .expect("the loopback opens");
        assert_eq!(app.last_dial, Some(Dial::Stdio("-".into())));
        assert!(pump_until(&mut app, |app| app.state == ConnectionState::Connected));

        app.disconnect();
        assert!(!app.session.is_live());
        let before = app.log.len();
        app.reconnect();
        assert!(app.session.is_live(), "the link is open again");
        assert_eq!(
            app.log.len(),
            before,
            "a re-dial that worked is not a toast"
        );
        assert!(pump_until(&mut app, |app| app.state == ConnectionState::Connected));
    }

    /// A modal takes the keyboard, and closing it gives the keyboard back.
    #[test]
    fn an_overlay_takes_the_keyboard() {
        let mut app = App::default();
        assert!(!app.overlay_open());
        app.dialog = Some(crate::dialogs::Dialog::Help(0));
        assert!(app.overlay_open());
        app.dialog = None;
        palette::open(&mut app);
        assert!(app.overlay_open(), "the palette also takes the keyboard");
    }

    /// Quitting is a flag the event loop reads, so the palette's `quit` action
    /// and the chord end the session the same way.
    #[test]
    fn quitting_is_a_flag() {
        let mut app = App::default();
        assert!(!app.quit);
        app.request_quit();
        assert!(app.quit);
    }

    // --- the transfer ------------------------------------------------------

    /// A connected app on a memory link, which is what every transfer test
    /// needs and none of them should re-derive.
    fn connected(lang: Lang) -> (App, crate::transport::testing::MemoryLink) {
        let mut app = App {
            lang,
            ..App::default()
        };
        let wire = crate::transport::testing::MemoryLink::new();
        app.connect(wire.link()).expect("the memory link opens");
        assert!(pump_until(&mut app, |app| app.connected()));
        (app, wire)
    }

    /// Somewhere a transfer can write that is not the crate's own directory.
    fn scratch(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("linkr-xfer-{label}-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    /// A transfer with no link is refused *out loud*: the operator picked it
    /// from a list and is owed an answer, not silence.
    #[test]
    fn a_transfer_with_no_link_is_refused_out_loud() {
        let mut app = App {
            lang: Lang::En,
            ..App::default()
        };
        // Even a frame that claims connected cannot start one: the link decides.
        app.state = ConnectionState::Connected;
        assert!(!app.start_receive(scratch("nolink")));
        assert!(!app.start_send(scratch("nolink").join("nothing.bin")));
        assert!(!app.transferring());
        assert_eq!(
            app.log.last().map(|notice| notice.text.as_str()),
            Some("Not connected — no transfer was started.")
        );
    }

    /// A send wakes the far end's shell before it says anything else, which is
    /// what makes the direction usable without the operator first typing `rz` on
    /// the device.
    #[test]
    fn a_send_wakes_the_far_ends_shell_first() {
        let (mut app, wire) = connected(Lang::En);
        let path = scratch("wakeup").join("fw.bin");
        std::fs::write(&path, b"payload").unwrap();

        assert!(app.start_send(path.clone()));
        assert!(app.transferring());
        assert!(pump_until(&mut app, |_| wire.sent_len() >= 3));
        let sent = wire.take_sent();
        assert!(
            sent.starts_with(b"rz\r"),
            "the wakeup never went out: {sent:?}"
        );
        assert!(
            app.log
                .iter()
                .any(|notice| notice.text.starts_with("sending to the device: ")),
            "{:?}",
            app.log
        );
        let _ = std::fs::remove_file(&path);
    }

    /// While a transfer owns the link the device has no keyboard: a keystroke is
    /// dropped and a preset is refused, because either would land in the middle
    /// of a block where nothing checks it.
    #[test]
    fn a_transfer_takes_the_keyboard_off_the_device() {
        let (mut app, wire) = connected(Lang::En);
        assert!(app.start_receive(scratch("keyboard")));
        // The announce is on the wire; from here on only the transfer may write.
        assert!(pump_until(&mut app, |_| wire.sent_len() > 0));
        let _ = wire.take_sent();
        // The transfer's own announce is already counted; nothing further is.
        let before_tx = app.tx;

        assert!(!app.send_bytes(b"x"), "a keystroke is dropped");
        assert!(!app.send_command("reboot"), "a preset is refused");
        assert_eq!(app.tx, before_tx, "nothing further was counted as sent");
        assert_eq!(wire.sent_len(), 0, "and nothing reached the wire");
        assert_eq!(
            app.log.last().map(|notice| notice.text.as_str()),
            Some("A transfer is already running."),
            "and the preset is told what is in the way"
        );
    }

    /// Two dialogues on one wire is not two transfers, it is none — so a second
    /// one is refused rather than interleaved with the first.
    #[test]
    fn a_second_transfer_is_refused_while_one_is_running() {
        let (mut app, _wire) = connected(Lang::En);
        assert!(app.start_receive(scratch("busy")));
        assert!(!app.start_receive(scratch("busy")));
        assert!(!app.start_send(scratch("busy").join("nothing.bin")));
        assert_eq!(
            app.log.last().map(|notice| notice.text.as_str()),
            Some("A transfer is already running.")
        );
    }

    /// A send that cannot describe its file says so and starts nothing: an error
    /// naming the cause is worth more than a session with nothing in it.
    #[test]
    fn a_send_of_a_file_that_is_not_there_starts_nothing() {
        let (mut app, _wire) = connected(Lang::En);
        let missing = scratch("missing").join("not-here.bin");
        assert!(!app.start_send(missing));
        assert!(!app.transferring());
        assert!(
            app.log
                .last()
                .is_some_and(|notice| notice.text.starts_with("cannot start the transfer: ")),
            "{:?}",
            app.log
        );
        assert_eq!(
            app.log.last().map(|notice| notice.level),
            Some(NoticeLevel::Error)
        );
    }

    /// Cancelling says which transfer it was, closes every row still in flight,
    /// and gives the link back to the device.
    #[test]
    fn cancelling_a_transfer_closes_its_rows_and_the_wire() {
        let (mut app, _wire) = connected(Lang::En);
        assert!(app.start_receive(scratch("cancel")));
        app.transfer.started("half.bin".into(), Some(100), false);
        app.transfer.counted("half.bin", false, 40, Some(100));

        app.cancel_transfer();
        assert!(!app.transferring(), "the link is the device's again");
        assert!(
            app.send_bytes(b"x"),
            "a keystroke goes to the device once the transfer is over"
        );
        assert_eq!(app.transfer.records[0].state, TransferState::Failed);
        assert!(
            app.log
                .last()
                .is_some_and(|notice| notice.text.starts_with("Transfer cancelled: ")),
            "{:?}",
            app.log
        );
        // Cancelling twice is not a second announcement.
        let before = app.log.len();
        app.cancel_transfer();
        assert_eq!(app.log.len(), before);
    }

    /// A link that goes away takes the transfer with it. A session waiting on a
    /// wire that no longer exists would sit in the view reading "running" for
    /// ever, and its half file would look like a whole one.
    #[test]
    fn a_link_that_ends_cancels_the_transfer() {
        let (mut app, wire) = connected(Lang::En);
        assert!(app.start_receive(scratch("dropped")));
        app.transfer.started("half.bin".into(), Some(100), false);
        wire.close();

        assert!(pump_until(&mut app, |app| !app.transferring()));
        assert_eq!(app.transfer.records[0].state, TransferState::Failed);
        assert!(
            app.log
                .iter()
                .any(|notice| notice.text.starts_with("Transfer cancelled: ")),
            "{:?}",
            app.log
        );
    }

    /// A transfer that is given up on leaves no half-written download behind.
    /// The partial file is the one outcome a cancel must not produce.
    #[test]
    fn a_cancelled_receive_leaves_no_half_file() {
        use crate::zmodem::{
            encode_header, subpacket, Encoding, FileInfo, FrameKind, Framend, Header, Width,
        };

        let dir = scratch("partial");
        let (mut app, wire) = connected(Lang::En);
        assert!(app.start_receive(dir.clone()));

        // A name block and part of the file, with no end-of-file after it: the
        // transfer is mid-file, which is exactly when a cancel happens. It goes
        // in over the *link*, so this is the routing the loop really runs and
        // not a shortcut into the engine.
        let info = FileInfo::new(*b"half.bin", 4096);
        let bin32 = |kind, data| encode_header(Header::new(kind, data), Encoding::Bin32);
        let mut stream = bin32(FrameKind::File, [1, 0, 0, 0]);
        stream.extend(subpacket::encode(
            &info.to_block(),
            Framend::Zcrcw,
            Width::ThirtyTwo,
        ));
        stream.extend(bin32(FrameKind::Data, [0, 0, 0, 0]));
        stream.extend(subpacket::encode(
            b"0123456789",
            Framend::Zcrce,
            Width::ThirtyTwo,
        ));
        wire.feed(&stream);

        let half = dir.join("half.bin");
        assert!(
            pump_until(&mut app, |_| half.exists()),
            "the file never opened"
        );
        assert!(
            app.transfer
                .records
                .iter()
                .any(|record| record.name == "half.bin" && record.bytes == Some(10)),
            "the row is the far end's name and the bytes that arrived: {:?}",
            app.transfer.records
        );

        app.cancel_transfer();
        assert!(
            !half.exists(),
            "a cancelled receive left a file that looks complete"
        );
        assert_eq!(app.transfer.records[0].state, TransferState::Failed);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
