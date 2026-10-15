//! Application state: everything a frame needs, and nothing a frame does not.
//!
//! The state is plain data with small, total transitions. No view owns state
//! and no state knows about a view: `ui::*` reads `&App`, the event loop
//! mutates it, and every mutation is a method here so it can be tested without
//! a terminal.

use std::collections::VecDeque;
use std::io;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::accessory::{self, UartSettings};
use crate::device::{Conversation, Query, Reply};
use crate::event::{ConnectionState, NoticeLevel, View};
use crate::i18n::Lang;
use crate::session::{Dial, Session};
use crate::theme::{Theme, ThemeChoice};
use crate::transport::{Link, LinkEvent};
use crate::views::diagnostics::DiagnosticsState;
use crate::views::terminal::TerminalPane;
use crate::watch::{SerialWatch, WatchOptions};
use crate::{dialogs, palette};

/// Which link carries the session.
///
/// The sidebar names it and the command line pins it, and it is the same list
/// the transport module implements — re-exported rather than mirrored, so a
/// link cannot exist without the chrome having a word for it.
pub use crate::transport::LinkKind as Transport;


/// Whether this session has a LAN token in hand, for the sidebar to say.
///
/// Three states rather than a bool, because "no token" and "there is no token
/// because this bridge wants none" are different facts and a user needs to know
/// which one they are looking at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LanCapture {
    /// Nothing asked yet.
    #[default]
    Unknown,
    /// A token is in hand.
    Captured,
    /// The bridge said `token=none`: LAN access is open.
    None,
}

/// How long the interface listens for boards before showing what it heard.
///
/// Longer than `--scan`'s own default: a user who picks "find my board" from a
/// palette is looking at a screen and is willing to wait, where a shell command
/// is watched while it runs. Short enough that a bench with the board switched
/// off does not leave the dialog sitting there for half a minute.
pub const BLE_SCAN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(12);

/// A Bluetooth scan running in the background, and where its answer lands.
///
/// Separate from the link because a scan has to keep running while the user
/// reads the console, and a scan that blocked the pump would be a scan that
/// froze the frame it is filling. The radio library is async and this program
/// is not, so the scan owns a thread of its own exactly as the BLE link does.
/// `Debug` by hand: `App` derives it, and the two channels behind a scan do not
/// implement it and should not — what a scan is, in a diagnostic, is how many
/// devices it heard and whether it is still listening.
impl std::fmt::Debug for BleScan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BleScan")
            .field("devices", &self.devices.len())
            .field("running", &self.running)
            .finish()
    }
}

#[derive(Default)]
pub struct BleScan {
    /// What it has found so far.
    pub devices: Vec<crate::cli::FoundDevice>,
    /// Whether the scan is still listening.
    running: bool,
    /// Where the scan's thread posts what it saw.
    rx: Option<std::sync::mpsc::Receiver<Vec<crate::cli::FoundDevice>>>,
    /// Where the scan's thread posts a reason it could not run at all.
    failure: Option<std::sync::mpsc::Receiver<String>>,
}

impl BleScan {
    /// Start listening, unless one is already running.
    ///
    /// `Err` is a sentence rather than a panic because the three ways this
    /// fails — no adapter, no Bluetooth on the host, a library that refuses —
    /// are all ordinary facts about the machine and all things a user can act
    /// on, unlike a bug in this program.
    pub fn start(&mut self, timeout: std::time::Duration) -> Result<(), String> {
        if self.running {
            return Err("a scan is already running".to_string());
        }
        let (tx, rx) = std::sync::mpsc::channel();
        self.running = true;
        self.rx = Some(rx);
        self.devices.clear();
        // A second channel for the "no adapter, no point going on" case, because
        // that answer is a sentence and not a device: posting it through the
        // device channel would make a caller think a scan succeeded and came
        // back with nothing.
        let (fail_tx, fail_rx) = std::sync::mpsc::channel::<String>();
        std::thread::spawn(move || {
            let outcome = crate::cli::scan_devices(timeout);
            match outcome {
                Ok(devices) => {
                    let _ = tx.send(devices);
                }
                Err(why) => {
                    let _ = fail_tx.send(why);
                }
            }
        });
        self.failure = Some(fail_rx);
        Ok(())
    }

    /// Whether a scan is in flight.
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// What the scan has seen, and whether it has finished.
    ///
    /// `Ok(false)` means "still running, nothing new". The devices are replaced
    /// rather than appended because the channel posts the whole list each time
    /// it has one: a device that went out of range then disappears from the
    /// count rather than sticking on the screen as a ghost.
    pub fn poll(&mut self) -> Result<bool, String> {
        if !self.running {
            return Ok(false);
        }
        // The failure channel is checked first so a scan that could not start
        // reports its reason instead of its empty list: a user who sees "no
        // devices" when the machine has no adapter is a user who goes looking
        // for a firmware bug.
        if let Some(fail_rx) = self.failure.as_ref() {
            if let Ok(why) = fail_rx.try_recv() {
                self.running = false;
                return Err(why);
            }
        }
        let Some(rx) = self.rx.as_ref() else {
            self.running = false;
            return Ok(false);
        };
        match rx.try_recv() {
            Ok(devices) => {
                self.devices = devices;
                self.running = false;
                Ok(true)
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => Ok(false),
            Err(_) => {
                self.running = false;
                Ok(false)
            }
        }
    }

    /// Why the scan could not run, when it could not.
    pub fn take_failure(&mut self) -> Option<String> {
        let fail_rx = self.failure.as_ref()?;
        let why = fail_rx.try_recv().ok()?;
        Some(why)
    }

    /// Stop listening and forget what was heard.
    pub fn reset(&mut self) {
        self.running = false;
        self.devices.clear();
        self.rx = None;
        self.failure = None;
    }
}

/// What `@s?` reported: LAN access state and the token the bridge expects.
///
/// Deliberately *not* part of the network view's state. A token is a secret and
/// a secret belongs somewhere a frame will not paint it, so this is read by the
/// LAN dial and by the sidebar's "captured" marker and by nothing else.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LanStatus {
    pub ws: String,
    /// The token as the bridge spells it, or empty for `token=none`.
    pub token: String,
    pub loading: bool,
}

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
    /// Whether the rail occupies its column. Ctrl+Up closes it to give the
    /// centre the whole band, and opens it again. It starts open, which is the
    /// designed layout; closing it is the user's call, never a response to a
    /// narrow terminal — that case is handled inside [`crate::ui::regions`],
    /// which shrinks the rail rather than dropping it.
    pub sidebar_visible: bool,
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
    /// The LAN access state `@s?` reported, token included.
    pub lan_status: LanStatus,
    /// Whether a token is in hand, which is what the sidebar shows.
    pub lan_captured: LanCapture,
    /// A Bluetooth scan in flight, for the pick-a-board dialog.
    pub ble_scan: BleScan,
    /// The assistant's session state for the assistant view.
    pub assistant: crate::views::assistant::AssistantState,
    /// The agent's durable state: tasks, notes and the saved session.
    ///
    /// None until `main` opens it, which is also what the tests get. A store
    /// touches the filesystem, and a `Default` that writes to the user's home
    /// directory is not a default anyone can run a test suite against.
    pub agent_store: Option<crate::agent::store::JsonFile>,
    /// The model transport, started on first use rather than at boot: a thread
    /// and a Tokio runtime are not something a `Default` should cost.
    pub agent_client: Option<crate::agent::runtime::AgentClient>,
    /// Where the replies the model is still thinking about arrive — one channel
    /// per question in flight, drained on the same beat as the serial link, so
    /// a slow model cannot freeze the interface and a reply lands in the task
    /// it belongs to.
    ///
    /// A vector, not a single slot. The slot was the first version, and it meant
    /// a second prompt submitted while the first was still being answered
    /// dropped the first one's channel — the reply the user was waiting for,
    /// lost silently.
    pub agent_replies: Vec<
        std::sync::mpsc::Receiver<(String, crate::agent::runtime::Progress)>,
    >,
    /// The device's own output, as the serial pane shows it.
    pub terminal: TerminalPane,
    /// The questions asked of the bridge about itself, and their answers.
    ///
    /// The bridge answers these on a channel of its own, so they are never
    /// mixed with the target's output — but something still has to remember
    /// which question is out, and what to do with an answer that never comes.
    /// See `device`.
    pub talk: Conversation,
    /// Management responses arrive as separately notified fragments, and one
    /// answer is often several; this is what puts them back together.
    ///
    /// Kept in the state rather than rebuilt per event for the same reason a
    /// `String` is: a fragment belonging to a message already half-read cannot
    /// survive a local variable.
    pub management: crate::protocol::ManagementReassembler,
    /// The watch that reads the target's output for a target that is not well:
    /// kernel panics, OOM kills, a board that boots and reboots before it ever
    /// settles. Held here rather than rebuilt per line because a reboot loop
    /// is only visible over a *window* of output, not at any one line of it.
    pub watch_engine: SerialWatch,
    /// How the Enter key is sent on the wire, when the command line named a
    /// translation. The pane asks for this per keystroke, so it belongs to the
    /// state and not to the event loop that set it.
    pub enter: crate::serial::keys::EnterMode,
    /// The byte that ends the session when it is typed, `^]` by default.
    ///
    /// A byte stream has no out-of-band signal, so one byte has to be reserved
    /// for "stop listening" — and it has to be a *byte* rather than a key,
    /// because what arrives at the device is bytes and the reservation has to
    /// hold on the far side of the wire too.
    pub escape: u8,
    /// Where the bytes on the wire are recorded, when the command line asked
    /// for them. See `trace`.
    pub trace: crate::trace::ByteTrace,
    /// Whether a typed line is held until Enter, as `--line-mode` asks.
    pub line_mode: bool,
    /// The line being held. Empty unless line mode is on, and cleared when the
    /// link goes down: a half-typed command belongs to the session that typed
    /// it, and carrying it into the next one would send a stranger your
    /// keystrokes.
    pub pending_line: String,
    /// Whether the target's output is kept as well as painted.
    ///
    /// Off by default, because a terminal pane is already a copy of everything
    /// the target has said and a second one is a leak. On for the script mode,
    /// which has no pane and would otherwise have no way to print what came
    /// back.
    pub collect_output: bool,
    /// What the target has said, kept only while [`Self::collect_output`] is on.
    pub output: VecDeque<Vec<u8>>,
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
            sidebar_visible: true,
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
            lan_status: LanStatus::default(),
            lan_captured: LanCapture::default(),
            ble_scan: BleScan::default(),
            assistant: crate::views::assistant::AssistantState::default(),
            agent_store: None,
            agent_client: None,
            agent_replies: Vec::new(),
            terminal: TerminalPane::new(),
            enter: crate::serial::keys::EnterMode::default(),
            escape: 0x1d,
            line_mode: false,
            pending_line: String::new(),
            trace: crate::trace::ByteTrace::none(),
            collect_output: false,
            output: VecDeque::new(),
            talk: Conversation::new(),
            management: crate::protocol::ManagementReassembler::default(),
            watch_engine: SerialWatch::new(WatchOptions::default()),
            session: Session::new(),
            log: Vec::new(),
            palette: None,
            dialog: None,
            last_dial: None,
            quit: false,
        }
    }
}

/// The most characters of screen an OSC 52 copy will carry.
///
/// A full scrollback is far more than any emulator accepts in one sequence,
/// and an over-long OSC 52 is not truncated — it is discarded whole. Trimming
/// before the base64 turns "nothing arrives" into "the last of it arrives",
/// which is what a capped copy means: keep the end, which is where the answer
/// the user just read always is.
const MAX_OSC52_CHARS: usize = 64 * 1024;

/// Put `text` on the terminal's clipboard with OSC 52.
///
/// Written straight to stdout rather than through the frame: a clipboard copy
/// is not a cell, and routing it through the buffer would either double it or
/// put it inside a frame's diff. One long escape sequence, flushed, and the
/// screen is untouched — the sequence carries no glyphs.
///
/// No ST terminator on purpose. The BEL form is the one every emulator that
/// supports the sequence accepts, and the ST form ends in a byte that some
/// terminals mistake for text.
pub fn osc52_copy(text: &str) -> usize {
    use std::io::Write;

    if text.is_empty() {
        return 0;
    }
    let payload = crate::base64::encode_base64(text.as_bytes());
    let mut out = std::io::stdout().lock();
    let _ = write!(out, "\x1b]52;c;{payload}\x07");
    let _ = out.flush();
    text.chars().count()
}

impl App {
    /// The theme in force.
    pub fn theme(&self) -> Theme {
        Theme::for_choice(self.theme)
    }

    /// The current language's table.
    /// The key the agent's per-device state is stored under.
    ///
    /// The name the user gave the link, or the address when they did not name
    /// one. Two boards that are both "Linkr BLE UART" share a store, which is
    /// the one case this gets wrong and the reason the address wins whenever
    /// there is one to use.
    pub fn agent_device_key(&self) -> String {
        if self.device_name.trim().is_empty() {
            "unknown".to_string()
        } else {
            self.device_name.clone()
        }
    }

    /// Seconds since boot, for a timestamp that means something in a log and
    /// nothing to anyone trying to read one.
    ///
    /// The clock in the top bar is wall time, which is right for a person
    /// looking at a frame and wrong for a record that has to be ordered after
    /// a reboot across a timezone.
    pub fn environment_stamp(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

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

    /// Open or close the rail, and say so.
    ///
    /// Closing it while the keyboard is on a sidebar row would leave the arrows
    /// driving a rail the user cannot see, so focus follows the rail to the
    /// terminal. Opening it leaves the focus alone: whichever pane was last used
    /// is usually the one being returned to.
    pub fn toggle_sidebar(&mut self) -> Notice {
        self.sidebar_visible = !self.sidebar_visible;
        let s = self.strings();
        let text = if self.sidebar_visible {
            s.sidebar_shown.to_string()
        } else {
            s.sidebar_hidden.to_string()
        };
        if !self.sidebar_visible && self.focus == Focus::Sidebar {
            self.focus = Focus::Terminal;
        }
        Notice {
            text,
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
    ///
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
        // A LAN dial needs the token, and `@s?` is the only place there is to
        // get one. Asked while the old link is still up — before the new one
        // closes it — so a user who was on Bluetooth and picks LAN from the
        // palette has the field filled by the time the dial happens. A LAN
        // dial from a standing start has nothing to ask, and answers with the
        // token the operator typed.
        if let Dial::Lan { .. } = dial {
            if self.ble_connected() {
                let _ = self.ensure_lan_token();
            }
        }
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
    /// File any answer the model has finished thinking about.
    ///
    /// A reply arrives as `(task_id, outcome)` so it can be matched to the goal
    /// it belongs to even when two prompts are in flight and answer out of
    /// order. Every live channel is drained, not just one — draining a single
    /// slot would leave the rest to arrive on a later beat with their task ids
    /// still matching, which is the same answer worked twice.
    ///
    /// A reply nobody is waiting for is dropped rather than applied to the
    /// newest task: a stale answer filed against the wrong question is worse
    /// than none, because it reads as though the model said it.
    pub fn drain_agent_replies(&mut self) {
        // Taken out so the store can be borrowed while the channels are read,
        // and so a channel that is still live goes back rather than being
        // replaced by the next one's.
        let live = std::mem::take(&mut self.agent_replies);
        let mut arrived: Vec<(String, crate::agent::runtime::Progress)> = Vec::new();
        for receiver in live {
            // The channel is dropped only when the stream is over. Anything else
            // — nothing yet, or a delta in mid-answer — keeps it: a stream that
            // was cut off after its first chunk would otherwise lose the rest,
            // and the answer would stop half-way through without a `Done` to
            // close it.
            let mut closed = false;
            while let Ok(item) = receiver.try_recv() {
                closed = !matches!(
                    item.1,
                    crate::agent::runtime::Progress::Delta(_)
                );
                arrived.push(item);
            }
            if !closed {
                self.agent_replies.push(receiver);
            }
        }

        for (task_id, progress) in arrived {
            let Some(found) = self
                .assistant
                .tasks
                .iter()
                .find(|task| task.id == task_id)
                .cloned()
            else {
                continue;
            };

            // A delta is the reply arriving, so it goes into the summary the
            // view already paints: the answer is typed out as the model writes
            // it, which is the whole point of asking for a stream.
            let crate::agent::runtime::Progress::Delta(delta) = &progress else {
                // `Done` and `Failed` are the end of the answer, and both go
                // through the closing path below.
                Self::finish(self, &found, progress);
                continue;
            };
            if let Some(slot) = self
                .assistant
                .tasks
                .iter_mut()
                .find(|task| task.id == task_id)
            {
                slot.summary.push_str(delta);
                // Not "answered" yet: a partial reply is not a reply, and `Done`
                // is what says the model finished.
                if slot.status == "asking" {
                    slot.status = "typing".to_string();
                }
            }
        }
    }

/// Close one answer: file it, remember it, and say what it was.
///
/// Both `Done` and `Failed` come here, because the difference between them is
/// one field — the summary and the status — and everything else a finished
/// answer does is the same for both.
fn finish(app: &mut App, found: &crate::agent::tasks::TaskRecord, step: crate::agent::runtime::Progress) {
    let (summary, status, usage) = match &step {
        crate::agent::runtime::Progress::Done(reported) => {
            (found.summary.clone(), "answered", *reported)
        }
        _ => {
            let failure = match &step {
                crate::agent::runtime::Progress::Failed(failure) => failure.clone(),
                _ => crate::agent::runtime::Failure::Malformed,
            };
            (failure.describe(), "failed", crate::agent::usage::Usage::default())
        }
    };

    // The two numbers the view shows are a running total, so one more message is
    // folded in rather than replacing the count.
    {
        let running = crate::agent::usage::Usage {
            input: app.assistant.usage.0,
            output: app.assistant.usage.1,
            ..crate::agent::usage::Usage::default()
        };
        let merged = crate::agent::usage::add_usage(&running, &usage);
        app.assistant.usage = (merged.input, merged.output);
    }

    // The reply is the assistant's turn in the conversation. A failure is a turn
    // too: the thread of what was asked and what came back is the record, and a
    // gap in it is worse than a turn that says the model did not answer.
    app.assistant.display.push(crate::agent::session::DisplayLine {
        role: "assistant".to_string(),
        text: summary.clone(),
    });
    let keep = crate::agent::session::SESSION_MESSAGES;
    let from = app.assistant.display.len().saturating_sub(keep);
    app.assistant.display.drain(..from);
    // Persisted here rather than only at the view's Enter: the reply arrives on
    // this beat, and a session saved on the question alone would lose every
    // answer if the program ended between the two.
    let key = app.agent_device_key();
    let now = app.environment_stamp() * 1000;
    if let Some(store) = app.agent_store.as_mut() {
        let _ = crate::agent::session::save_session(
            store,
            &key,
            &app.assistant.display,
            &app.assistant.display,
            now,
        );
    }

    // Whatever happened, the answer is in the store: a task that says "failed"
    // is the record of a question that was asked.
    let saved = crate::agent::tasks::TaskRecord {
        summary,
        status: status.to_string(),
        ..found.clone()
    };
    if let Some(store) = app.agent_store.as_mut() {
        crate::agent::tasks::TaskStore::new(&mut *store).save(&saved);
    }
    if let Some(slot) = app
        .assistant
        .tasks
        .iter_mut()
        .find(|task| task.id == found.id)
    {
        slot.summary = saved.summary;
        slot.status = saved.status;
    }
}

    pub fn pump(&mut self) {
        self.poll_ble_scan();

        self.drain_agent_replies();
        let was_live = self.session.is_live();
        let was_connected = self.state == ConnectionState::Connected;
        let describe = self.session.describe().to_string();
        let now = Instant::now();
        for event in self.session.pump() {
            match event {
                LinkEvent::Data(bytes) => {
                    self.terminal.push(&bytes);
                    self.trace.rx(&bytes);
                    if self.collect_output {
                        self.output.push_back(bytes.clone());
                    }
                    {
                        // The same bytes, read a second time by the watch. It
                        // looks for a line saying `Kernel panic` or `Out of
                        // memory`, and where lines end is its own business — a
                        // bootloader that ends them with `\r` alone must not be
                        // invisible to it. The pane keeps the bytes whole.
                        let at = wall_clock_ms();
                        self.observe(&bytes, at);
                    }
                }
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
                // An answer from the bridge about itself. Reassembled here and
                // handed to whoever asked; nothing about it reaches the pane,
                // because it is not something the target said.
                LinkEvent::Management(fragment) => {
                    let decision = self.management.push(&fragment);
                    let outcome = match decision {
                        crate::protocol::Reassembly::Incomplete => None,
                        crate::protocol::Reassembly::Complete { payload, meta } => {
                            Some((meta, payload))
                        }
                        crate::protocol::Reassembly::Error(why) => {
                            let text = why.message(false).to_string();
                            self.push_notice(Notice {
                                text,
                                level: NoticeLevel::Error,
                            });
                            None
                        }
                    };
                    if let Some((header, payload)) = outcome {
                        if !self.talk.offer(&header, &payload) {
                            // An answer nobody asked for: a notification from
                            // the bridge, or a reply to a question a previous
                            // session left open. Neither is this session's to
                            // interpret, and both would otherwise be dropped
                            // with no trace of having arrived.
                            let _ = (header, payload);
                        }
                        // A scan streams: the bridge answers one network at a
                        // time rather than once at the end, and the list is
                        // where the user watches it happen. Folded here rather
                        // than only at finalize, so a network appears as it is
                        // heard — the alternative is a view that reads "scanning"
                        // for eleven seconds and then fills at once, which is
                        // the same wait with nothing to watch.
                        //
                        // The last line may be half a name while its frame is
                        // still in flight. It is corrected by the next one,
                        // which is what a streaming list looks like on the wire.
                        if self.talk.waiting() == Some(Query::Wifi) {
                            let partial = scan_results(self.talk.pending_lines());
                            if !partial.is_empty() {
                                self.network.scan_results = partial;
                            }
                        }
                    }
                }
                LinkEvent::Opened | LinkEvent::Closed(_) => {}
            }
        }
        // Answers are collected between ticks and read here, because the tick
        // that asked is never the tick that was answered. A timeout is not an
        // error until it is reported, and reporting it here is what stops a
        // grid reading "loading…" for want of a reply that is not coming.
        if let Some(reply) = self.talk.take_reply() {
            let stamp = App::clock_now();
            self.apply_reply(reply, &stamp);
        }
        if let Some(query) = self.talk.abandon(now) {
            self.report_unanswered(query);
        }
        // A program that asks a question — where is the cursor, how wide is the
        // screen — blocks until it hears back, so the emulator's answers go out
        // on the tick that produced them. Always *after* the feed: an answer is
        // about the screen those bytes have just painted. With no link they are
        // dropped, which is the right way round — a cursor report delivered late
        // is worse than one never sent, because the program that asked has
        // already given up and moved on.
        for reply in self.terminal.take_replies() {
            self.session.send(&reply);
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
        if was_live && !now_live {
            // The last thing a target says very often has no newline after it —
            // a prompt waiting for a command that will not come — and the watch
            // holds such a line until one arrives. Ending it here is what lets
            // it report what happened at the moment the link died, which is
            // when a report matters most.
            self.observe(b"\n", wall_clock_ms());
            // Nothing else can arrive on this link, so a question left open
            // would otherwise be abandoned only at its timeout — reporting a
            // hang-up as a slow bridge.
            self.talk.reset();
            self.management.reset();
            // And the half-typed line goes with it.
            self.pending_line.clear();
        }
        self.rx = self.session.rx();
        self.tx = self.session.tx();
    }

    /// What the target has printed since the last call, for a caller with no
    /// pane of its own. See [`App::collect_output`].
    pub fn take_output(&mut self) -> Vec<Vec<u8>> {
        self.output.drain(..).collect()
    }

    /// Whether a question is still waiting for its answer.
    ///
    /// Asked by the script mode, which has to know when to stop pumping; the
    /// frame never needs to, because a tick is always followed by another.
    pub fn waiting_on_device(&self) -> bool {
        self.talk.waiting().is_some()
    }

    /// The wall clock, `HH:MM:SS`, local time.
    ///
    /// Free rather than a method, because it is not about the state: a read's
    /// stamp answers "when did the device tell you this", which is a question
    /// about the world and not about `App`.
    pub fn clock_now() -> String {
        let now = chrono::Local::now();
        now.format("%H:%M:%S").to_string()
    }

    /// Hand target output to everything that reads rather than paints.
    fn observe(&mut self, bytes: &[u8], at: i64) {
        if self.watch_engine.feed_bytes(bytes, at).is_empty() {
            return;
        }
        // Re-described from the whole set rather than appended to, because a
        // boot loop is one finding replacing several banners: what the sidebar
        // shows has to be the engine's current opinion and not a history of its
        // changing mind.
        let language = if self.lang == Lang::Zh { "zh" } else { "en" };
        self.watch = crate::watch::describe_findings(&self.watch_engine.findings(), language);
    }

    /// Ask the bridge about itself. Returns why the question could not go out.
    ///
    /// The answers land in `self.diagnostics` and `self.network` on whichever
    /// pump they arrive on, so this does not block and does not return them:
    /// asking is one tick's work and being answered is another's.
    /// Make sure the LAN access token is in hand, asking for it if it is not.
    ///
    /// `@s?` is the only place the bridge reports its LAN access token, and a
    /// LAN dial without one is a dial that gets refused. So a caller dialling
    /// over LAN calls this first, and a link that has already been asked does
    /// not ask twice: the token is remembered for the session.
    ///
    /// Asked here rather than on a flag because the alternative is a user who
    /// has to know the token exists before they can learn it.
    /// Look for boards, and open the list to pick from.
    ///
    /// The question this answers is the one nothing else can: which board is
    /// *this* one, when there are two on the bench and neither was named on a
    /// command line. `--scan` answers it for a shell and leaves the interface
    /// unable to act on the answer, so re-typing the MAC it just showed is the
    /// work it was supposed to save.
    pub fn start_ble_scan(&mut self) -> Result<(), String> {
        self.ble_scan.reset();
        self.ble_scan.start(BLE_SCAN_TIMEOUT)?;
        self.dialog = Some(crate::dialogs::Dialog::DevicePick(
            crate::dialogs::DevicePick {
                devices: Vec::new(),
                cursor: 0,
                scanning: true,
                message: String::new(),
            },
        ));
        Ok(())
    }

    /// Stop listening and close the list.
    ///
    /// Closing the dialog matters as much as stopping the thread: a dialog left
    /// open over a finished scan would offer Enter on a list nothing is adding
    /// to, and a user who cancels wants the screen back.
    pub fn cancel_ble_scan(&mut self) {
        self.ble_scan.reset();
        self.dialog = None;
    }

    /// Dial the board with this address, and say which one it was.
    ///
    /// The address is what a scan result identifies a device by — a name is a
    /// substring, and two boards on a bench can both be called "Linkr Bee" —
    /// so the dial takes the id and the notice repeats the name the user
    /// picked so the log says what they meant and not just what went out.
    pub fn connect_to_board(&mut self, address: &str) {
        let selection = crate::transport::ble::Selection::Address(address.to_string());
        if let Err(why) = self.dial(&crate::session::Dial::Ble(selection)) {
            let text = format!("{}: {why}", self.strings().link_failed);
            self.push_notice(Notice {
                text,
                level: crate::event::NoticeLevel::Error,
            });
            return;
        }
        self.dialog = None;
        self.set_view(crate::event::View::Terminal);
        self.focus = crate::app::Focus::Terminal;
    }

    /// Fold whatever the background scan has seen into the open dialog.
    ///
    /// Called from `pump`, so the dialog fills on the same beat as the console:
    /// a scan whose results appeared only after the pump would look like a scan
    /// that had finished and then thought of something.
    fn poll_ble_scan(&mut self) {
        if !self.ble_scan.is_running() {
            // Finished or never started: clear the dialog's "listening" flag so
            // the legend stops offering a cancel that would do nothing.
            if let Some(crate::dialogs::Dialog::DevicePick(pick)) = &mut self.dialog {
                pick.scanning = false;
            }
            // A failure has a sentence, and a sentence is what the user is
            // owed: "no adapter" is not the same news as "no boards".
            if let Some(why) = self.ble_scan.take_failure() {
                if let Some(crate::dialogs::Dialog::DevicePick(pick)) = &mut self.dialog {
                    pick.message = why;
                }
                self.ble_scan.reset();
            }
            return;
        }
        match self.ble_scan.poll() {
            Ok(true) => {
                if let Some(crate::dialogs::Dialog::DevicePick(pick)) = &mut self.dialog {
                    pick.devices = self.ble_scan.devices.clone();
                    pick.cursor = pick.cursor.min(pick.devices.len().saturating_sub(1));
                }
            }
            Ok(false) => {}
            Err(why) => {
                if let Some(crate::dialogs::Dialog::DevicePick(pick)) = &mut self.dialog {
                    pick.message = why;
                }
                self.ble_scan.reset();
            }
        }
    }

    /// Ask for the LAN access token if it is not already in hand.
    pub fn ensure_lan_token(&mut self) -> Result<(), String> {
        self.ask_device(Query::Lan)
    }

    pub fn ask_device(&mut self, query: Query) -> Result<(), String> {
        if !self.connected() {
            return Err(self.strings().not_connected.to_string());
        }
        if !self.ble_connected() {
            // Asked of a LAN session this is not a failure to report later; it
            // is the wrong bearer, and the answer should say which before the
            // user waits for a reply that was never going to come.
            return Err(self.strings().diag_no_ble.to_string());
        }
        let (id, body) = self.talk.ask(query, Instant::now());
        let frame = crate::protocol::encode_management_request(id, body.as_bytes())
            .map_err(|why| why.message(false).to_string())?;
        if !self.session.send_management(frame) {
            return Err(self.strings().not_connected.to_string());
        }
        // A read that has gone out owns its own view slot: nothing else will
        // fill it, and leaving it untouched would let a stale answer look like
        // the one just asked for.
        match query {
            Query::Diagnostics => {
                self.diagnostics.loading = true;
                self.diagnostics.error = None;
            }
            Query::Uart | Query::Wifi | Query::Webdav => {
                self.network.loading = true;
                self.network.error = None;
            }
            // The LAN read carries a secret, so its slot is the settings it
            // fills and not the network view: a token that appears on screen
            // next to a wifi name is a token in a screenshot.
            Query::Lan => self.lan_status.loading = true,
        }
        Ok(())
    }

    /// Change the bridge's UART — baud, framing, flow control.
    ///
    /// The write and its confirmation are one act: the bridge is asked again
    /// what it ended up at, so the frame's own baud comes from the bridge
    /// rather than from what we meant to set. A target that refuses the rate
    /// therefore shows the rate it kept.
    pub fn set_uart(&mut self, settings: &UartSettings) -> Result<(), String> {
        let command = accessory::set_uart_command(settings)?;
        self.send_accessory(&command, Query::Uart)
    }

    /// Join a network, or forget the one it joined (`@w off` with no SSID).
    pub fn set_wifi(&mut self, ssid: &str, password: &str) -> Result<(), String> {
        let action = if ssid.trim().is_empty() {
            "off"
        } else {
            "connect"
        };
        let command = accessory::wifi_command(action, ssid, password)?;
        self.send_accessory(&command, Query::Wifi)
    }

    /// Send one management command the caller has already built, and wait for
    /// the read that confirms it.
    ///
    /// Which read follows is read off the command rather than passed in,
    /// because the command *is* the statement of what is being changed — and a
    /// caller that could name the read separately could name the wrong one.
    pub fn run_accessory(&mut self, command: &str) -> Result<(), String> {
        let query = match command.split([' ', '=']).next().unwrap_or("").as_bytes() {
            b"@u" => Query::Uart,
            b"@w" => Query::Wifi,
            b"@d" => Query::Webdav,
            _ => return Err(self.strings().net_bad_command.to_string()),
        };
        self.send_accessory(command, query)
    }

    /// Ask the bridge what networks it can see.
    ///
    /// This is the bridge scanning, not this host: the radio on this machine
    /// and the one on the board are in different places, and a laptop's list
    /// of networks says nothing about what the board can reach from where it
    /// is bolted down.
    pub fn scan_wifi(&mut self) -> Result<(), String> {
        let command = accessory::wifi_command("scan", "", "")?;
        self.send_accessory(&command, Query::Wifi)
    }

    /// Point log upload at `url`, or turn it off when there is none.
    pub fn set_webdav(&mut self, url: &str) -> Result<(), String> {
        let action = if url.trim().is_empty() { "off" } else { "on" };
        let command = accessory::webdav_command(action, url)?;
        self.send_accessory(&command, Query::Webdav)
    }

    /// Send an accessory command and wait for the answer to the question that
    /// follows it.
    ///
    /// The write and the read are one operation on purpose. `OK` alone is not
    /// enough to put anything in a view: it says the bridge accepted the
    /// command, not what it is doing now, and the two disagree whenever the
    /// firmware clamps a value — which is exactly when showing the command you
    /// sent rather than the state the bridge is in would mislead.
    fn send_accessory(&mut self, command: &str, query: Query) -> Result<(), String> {
        if !self.connected() {
            return Err(self.strings().not_connected.to_string());
        }
        if !self.ble_connected() {
            return Err(self.strings().net_wifi_needs_ble.to_string());
        }
        // Secrets never leave this function unredeacted, but the command *is*
        // the thing going to the bridge: only what is remembered and shown
        // goes through the scrubber.
        let shown = accessory::redact_command(command);
        let (id, _) = self.talk.ask(query, Instant::now());
        let frame = crate::protocol::encode_management_request(id, command.as_bytes())
            .map_err(|why| why.message(false).to_string())?;
        if !self.session.send_management(frame) {
            return Err(self.strings().not_connected.to_string());
        }
        self.trace.tx_command(&shown);
        self.network.loading = true;
        self.network.error = None;
        self.push_notice(Notice {
            text: format!("{} {shown}", self.strings().net_status),
            level: NoticeLevel::Info,
        });
        Ok(())
    }

    /// Fold one answer in, whatever it answered.
    fn apply_reply(&mut self, reply: Reply, stamp: &str) {
        // Whatever happened, nothing is waiting for it any more — and clearing
        // that first is what stops a failed read from leaving its view reading
        // "loading…" underneath an error.
        self.stop_waiting(reply.query);
        if !reply.status.ok {
            let error = reply.status.error.clone();
            match reply.query {
                Query::Diagnostics => self.diagnostics.error = Some(error.clone()),
                _ => self.network.error = Some(error.clone()),
            }
            self.push_notice(Notice {
                text: error,
                level: NoticeLevel::Error,
            });
            return;
        }
        let text = reply.lines.join("\n");
        match reply.query {
            Query::Diagnostics => self.diagnostics.apply(&reply.lines, stamp.to_string()),
            // A read is how the frame learns the wire's own rate. Asking the
            // target would be asking somebody who does not know: this is the
            // bridge's UART, not the target's console.
            Query::Uart => {
                if let Some(settings) = accessory::parse_uart_settings(&text) {
                    self.baud = Some(settings.baud);
                }
            }
            // `@w?` and `@w scan` are the same question on the wire and two
            // different answers to read. A status names one network; a scan
            // lists many, and a list is not something to squeeze into the
            // status slot — it goes to its own section of the view.
            Query::Wifi => {
                if let Some(status) = accessory::parse_wifi_status(&text) {
                    self.network.wifi = Some(status);
                } else {
                    let results = scan_results(&reply.lines);
                    if !results.is_empty() {
                        self.network.scan_results = results;
                    }
                }
            }
            // `@s?` is the only place the bridge reports its LAN access
            // token, and a client that does not ask has no way to learn one.
            // Capturing it here — during the BLE session that is already open
            // — is what leaves the LAN dial filled in without anybody typing a
            // token. What the frame shows is *that* it has one, never what it
            // is.
            Query::Lan => {
                if let Some(status) = accessory::parse_lan_status(&text) {
                    self.lan_status.ws = status.ws;
                    self.lan_status.token = status.token.clone();
                    if status.token.is_empty() {
                        self.lan_captured = LanCapture::None;
                    } else {
                        self.lan_captured = LanCapture::Captured;
                    }
                }
            }
            Query::Webdav => {
                if let Some(status) = accessory::parse_webdav_status(&text) {
                    self.network.webdav = Some(status);
                }
            }
        }
    }

    /// Say that a question went unanswered, and stop the view waiting on it.
    fn report_unanswered(&mut self, query: Query) {
        self.stop_waiting(query);
        let s = self.strings();
        let text = s.mgmt_no_answer.to_string();
        match query {
            Query::Diagnostics => self.diagnostics.error = Some(text.clone()),
            _ => self.network.error = Some(text.clone()),
        }
        self.push_notice(Notice {
            text,
            level: NoticeLevel::Warn,
        });
    }

    /// Clear the in-flight flag this question set, whichever view it set it on.
    fn stop_waiting(&mut self, query: Query) {
        match query {
            Query::Diagnostics => self.diagnostics.loading = false,
            Query::Uart | Query::Wifi | Query::Webdav => self.network.loading = false,
            Query::Lan => self.lan_status.loading = false,
        }
    }

    /// Paste a block of text, as one thing.
    ///
    /// A paste is a typing gesture that arrives whole, so it takes the same
    /// door a keystroke takes — `type_bytes` — and gets the same treatment:
    /// line mode holds it until Enter, and the echo, the trace and the `tx`
    /// counter all see it. It is not looped over as lines with an Enter
    /// guessed after each, because a paste of a shell script would then run
    /// each line as it was typed, which is the one thing a paste must never do
    /// to a device.
    ///
    /// Returns false when there is nothing to send, which is the same silence
    /// `send_bytes` gives a keystroke: a paste into a disconnected session is
    /// not worth a toast either.
    pub fn paste(&mut self, text: &str) -> bool {
        if text.is_empty() {
            return true;
        }
        // Newlines and tabs are what a paste carries and what a terminal sends:
        // \r\n, \n or \r all arrive as the newline a paste means, and a \r
        // inside the block is the shape every terminal actually emits.
        let normalised = text.replace("\r\n", "\n").replace('\r', "\n");
        self.type_bytes(normalised.ends_with('\n'), &normalised)
    }

    /// Put the emulator's contents on the terminal's clipboard, and say so.
    ///
    /// Returns the notice rather than pushing it, so the wording stays in one
    /// place with every other notice this key produces — `toggle_sidebar()` and
    /// `toggle_theme()` do the same, and a caller that formats its own is a
    /// caller whose wording can drift from the language table.
    ///
    /// An empty screen is not a copy: the terminal is blank until the target
    /// has said something, and "copied 0 chars" is worse than silence.
    pub fn copy_screen(&mut self) -> Notice {
        let text = self.terminal.visible_text();
        let kept: String = text
            .chars()
            .take(MAX_OSC52_CHARS)
            .collect();
        let copied = kept.chars().count();
        if copied > 0 {
            osc52_copy(&kept);
        }
        let s = self.strings();
        Notice {
            text: format!(
                "{} {} {}",
                s.copy_done_prefix,
                copied,
                s.copy_done_suffix
            ),
            level: NoticeLevel::Info,
        }
    }

    /// Send raw bytes. Silent when there is no link, because this is the path a
    /// *keystroke* takes and a toast per character would bury the screen.
    pub fn send_bytes(&mut self, bytes: &[u8]) -> bool {
        if !self.connected() {
            return false;
        }
        if !self.session.send(bytes) {
            return false;
        }
        self.trace.tx(bytes);
        // The counter is part of the *frame*, so it moves here rather than at
        // the next pump: a keystroke's byte should show in the top bar on the
        // same repaint as the character it produced.
        self.tx = self.session.tx();
        if self.terminal.echo {
            self.terminal.push(bytes);
        }
        true
    }

    /// Type one keystroke, holding it until the line ends when line mode is on.
    ///
    /// Line mode is `--line-mode`, and it exists because a key at a time is not
    /// what every target wants: a shell with a line discipline reads a whole
    /// line, and a half-finished command sent a character at a time cannot be
    /// taken back by the time you notice the typo in it.
    ///
    /// Only the *stream* is held. Named keys — arrows, function keys, Tab —
    /// still go straight out, because holding an arrow key until Enter would
    /// make a target's own line editor unusable.
    pub fn type_bytes(&mut self, ends_line: bool, bytes: &str) -> bool {
        if !self.line_mode {
            return self.send_bytes(bytes.as_bytes());
        }
        if !ends_line && !bytes.is_empty() {
            self.pending_line.push_str(bytes);
            // Echoed as it is held, so the line can be read back before it is
            // sent: local echo is the point of line mode, and a held line that
            // only appears once it is gone would be worse than neither.
            if self.terminal.echo {
                self.terminal.push(bytes.as_bytes());
            }
            return true;
        }
        let mut line = std::mem::take(&mut self.pending_line);
        line.push_str(bytes);
        self.send_bytes(line.as_bytes())
    }

    /// Send a whole command, and say so when there is nothing to send it over.
    ///
    /// One-shot commands go through this door: `send_bytes` refusing in
    /// silence is right for a keystroke and wrong for a preset, where the user
    /// pressed a button and is owed an answer.
    pub fn send_command(&mut self, text: &str) -> bool {
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

}

/// The networks out of a `@w scan` reply.
///
/// The status line is dropped rather than shown: `OK scan` is an answer to a
/// question nobody asked out loud, and listing it among the networks would put
/// a word where a network name belongs. Everything else is taken as it came,
/// which is the honest thing to do with a list whose shape is the firmware's
/// to choose.
fn scan_results(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .filter(|line| {
            let lower = line.to_ascii_lowercase();
            !accessory::starts_with_word(&lower, "ok")
                && !accessory::starts_with_word(&lower, "err")
        })
        .map(|line| line.to_string())
        .collect()
}

/// Milliseconds since the epoch, which is the clock the watch measures a
/// reboot loop in.
///
/// Wall time rather than the process's own: a board that rebooted three times
/// before this program started is still rebooting, and restarting the window
/// at every launch would hide exactly the failure the watch exists to report.
fn wall_clock_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod multi_reply {
    use super::*;

    fn task(id: &str, goal: &str) -> crate::agent::tasks::TaskRecord {
        crate::agent::tasks::TaskRecord {
            id: id.to_string(),
            device_key: "board".to_string(),
            updated_at: 0,
            goal: goal.to_string(),
            summary: String::new(),
            status: "asking".to_string(),
        }
    }

    /// Two prompts in flight, both answered, each filed against its own task.
    ///
    /// The single-slot version of `agent_replies` dropped the first channel when
    /// the second prompt was submitted, so the reply the user was waiting for
    /// was lost silently and the task stayed "asking" for ever.
    #[test]
    fn two_in_flight_prompts_are_both_answered() {
        let mut app = App::default();
        app.assistant.tasks = vec![task("t1", "first"), task("t2", "second")];

        let (tx1, rx1) = std::sync::mpsc::channel();
        let (tx2, rx2) = std::sync::mpsc::channel();
        // Out of order, which is the case a slot cannot handle at all: t2
        // answers first and t1's channel has already been replaced.
        // The answer arrives as a delta and then a done, which is the shape a
        // stream really has: the text is written as it comes, and the `Done`
        // step is what says the model finished.
        tx2.send((
            "t2".to_string(),
            crate::agent::runtime::Progress::Delta("second answer".to_string()),
        ))
        .expect("send");
        tx2.send((
            "t2".to_string(),
            crate::agent::runtime::Progress::Done(crate::agent::usage::Usage::default()),
        ))
        .expect("send");
        tx1.send((
            "t1".to_string(),
            crate::agent::runtime::Progress::Delta("first answer".to_string()),
        ))
        .expect("send");
        tx1.send((
            "t1".to_string(),
            crate::agent::runtime::Progress::Done(crate::agent::usage::Usage::default()),
        ))
        .expect("send");
        app.agent_replies = vec![rx1, rx2];

        app.drain_agent_replies();

        let summaries: Vec<(&str, &str)> = app
            .assistant
            .tasks
            .iter()
            .map(|t| (t.id.as_str(), t.summary.as_str()))
            .collect();
        assert_eq!(summaries[0], ("t1", "first answer"));
        assert_eq!(summaries[1], ("t2", "second answer"));
        // And both channels are gone, so nothing is waited on for ever.
        assert!(
            app.agent_replies.is_empty(),
            "a channel nobody answers was left waiting"
        );
    }

    /// A channel still waiting keeps its place rather than being dropped.
    #[test]
    fn a_channel_still_waiting_is_kept() {
        let mut app = App::default();
        app.assistant.tasks = vec![task("t1", "first")];
        let (tx, rx) = std::sync::mpsc::channel();
        app.agent_replies = vec![rx];
        app.drain_agent_replies();
        assert_eq!(app.agent_replies.len(), 1, "the channel was dropped");
        assert_eq!(app.assistant.tasks[0].status, "asking");
        // It answers on a later beat and is filed then.
        tx.send((
            "t1".to_string(),
            crate::agent::runtime::Progress::Delta("late ".to_string()),
        ))
        .expect("send");
        app.drain_agent_replies();
        // The delta alone does not close the task: a partial reply is not a
        // reply, and this is what stops "typing" from reading as "answered".
        assert_eq!(app.assistant.tasks[0].summary, "late ");
        assert_eq!(app.assistant.tasks[0].status, "typing");
        tx.send((
            "t1".to_string(),
            crate::agent::runtime::Progress::Delta("answer".to_string()),
        ))
        .expect("send");
        tx.send((
            "t1".to_string(),
            crate::agent::runtime::Progress::Done(crate::agent::usage::Usage::default()),
        ))
        .expect("send");
        app.drain_agent_replies();
        assert_eq!(app.assistant.tasks[0].summary, "late answer");
        assert_eq!(app.assistant.tasks[0].status, "answered");
        assert!(app.agent_replies.is_empty());
    }

    /// A reply for a task that is not waiting is dropped rather than filed
    /// against whichever task is on screen.
    #[test]
    fn a_stale_reply_is_dropped_not_filed() {
        let mut app = App::default();
        app.assistant.tasks = vec![task("t9", "the one on screen")];
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send((
            "ghost".to_string(),
            crate::agent::runtime::Progress::Delta("an answer to something else".to_string()),
        ))
        .expect("send");
        app.agent_replies = vec![rx];
        app.drain_agent_replies();
        assert!(
            app.assistant.tasks[0].summary.is_empty(),
            "a stale reply was filed against the wrong task"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

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

    /// Closing the rail moves the focus with it. Otherwise the arrows would
    /// drive a rail the user cannot see, and the row they were on would still
    /// be highlighted somewhere off-screen.
    #[test]
    fn closing_the_rail_moves_the_focus_to_the_terminal() {
        let mut app = App::default();
        assert!(app.sidebar_visible, "the rail starts open");
        app.focus = Focus::Sidebar;
        app.sidebar_cursor = 2;

        let notice = app.toggle_sidebar();
        assert!(!app.sidebar_visible);
        assert_eq!(app.focus, Focus::Terminal);
        assert_eq!(notice.text, app.strings().sidebar_hidden);
        assert_eq!(notice.level, NoticeLevel::Info);
        // The cursor survives: reopening the rail should land back where it was.
        assert_eq!(app.sidebar_cursor, 2);

        // Opening it again leaves the focus wherever the user put it.
        app.focus = Focus::Sidebar;
        let notice = app.toggle_sidebar();
        assert!(app.sidebar_visible);
        assert_eq!(app.focus, Focus::Sidebar);
        assert_eq!(notice.text, app.strings().sidebar_shown);
    }

    /// A paste is a typing gesture, so line mode holds it until Enter exactly
    /// as it would have held the same characters typed one at a time. What a
    /// paste must never be is chopped at its newlines into per-line sends —
    /// pasting a shell script would then run each line as it was typed.
    #[test]
    fn a_paste_is_held_whole_in_line_mode() {
        let mut app = App {
            line_mode: true,
            ..App::default()
        };

        // No trailing newline: nothing is sent, the whole block is held.
        assert!(app.paste("echo one\necho two"));
        assert_eq!(app.pending_line, "echo one\necho two");
    }

    /// The three newline shapes every terminal actually emits all arrive as the
    /// one newline a paste means: \r\n, \n, and a lone \r. A \r left in place
    /// would be a carriage return the target reads as "start of line again".
    #[test]
    fn every_newline_shape_a_paste_can_carry_is_normalised() {
        for raw in ["a\r\nb", "a\nb", "a\rb"] {
            let mut app = App {
                line_mode: true,
                ..App::default()
            };
            assert!(app.paste(raw), "{raw:?} was refused");
            assert_eq!(
                app.pending_line, "a\nb",
                "{raw:?} did not arrive as one newline"
            );
        }
    }

    /// An empty paste is a no-op, not a send. Terminals emit it for an empty
    /// selection, and a keystroke-sized toast for it would be noise.
    #[test]
    fn an_empty_paste_is_a_no_op() {
        let mut app = App {
            line_mode: true,
            ..App::default()
        };
        assert!(app.paste(""), "an empty paste should be accepted");
        assert!(app.pending_line.is_empty(), "an empty paste held something");
    }

    /// The copy cap keeps the END of the screen, not the beginning.
    ///
    /// OSC 52 that is too long is discarded whole by the emulator rather than
    /// truncated, so a copy that cannot fit has to decide which end to keep.
    /// The end is where the thing the user just read lives; the beginning is
    /// hours-old boot noise they have long scrolled past.
    #[test]
    fn the_copy_keeps_the_end_of_the_screen() {
        let mut pane = TerminalPane::new();
        pane.set_size(40, 8);
        // More than the cap, so the trimming actually has to choose.
        let mut text = String::new();
        for _ in 0..(MAX_OSC52_CHARS + 64) {
            text.push('x');
        }
        text.push_str("TAIL");
        pane.push_text(&text);

        let app = &mut App {
            terminal: pane,
            ..App::default()
        };
        let notice = app.copy_screen();
        let copied = notice
            .text
            .split_whitespace()
            .nth(1)
            .and_then(|n| n.parse::<usize>().ok())
            .expect("the notice names the count");
        assert_eq!(copied, MAX_OSC52_CHARS, "the cap is what was copied");
    }

    /// Copying a screen that has said nothing still reports — and reports the
    /// truth about what was on it, which for a fresh pane is its blank screen
    /// and not a lie about zero.
    #[test]
    fn copying_a_blank_screen_reports_what_was_there() {
        let mut app = App::default();
        let notice = app.copy_screen();
        let copied = notice
            .text
            .split_whitespace()
            .nth(1)
            .and_then(|n| n.parse::<usize>().ok())
            .expect("the notice names the count");
        // A fresh emulator dumps its blank screen, which is not zero, and
        // claiming otherwise would make the number meaningless.
        assert!(copied > 0, "a blank screen reported {copied}");
        assert_eq!(copied, app.terminal.visible_text().chars().count());
    }

    /// With no link the paste is refused in the same silence `send_bytes` gives
    /// a keystroke: the path is shared, so the behaviour cannot diverge.
    #[test]
    fn a_paste_with_no_link_is_refused_silently() {
        let mut app = App::default();
        assert!(!app.paste("anything"), "a paste went out with no link");
        assert!(app.notices.is_empty(), "a silent path announced itself");
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

    // --- the memory link -----------------------------------------------------

    /// A connected app on a memory link, which is what every test that puts
    /// bytes on the wire needs and none of them should re-derive.
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

    /// A session standing in for a Bluetooth one, because the management
    /// channel — and therefore diagnostics, UART, WiFi and WebDAV — exists on
    /// no other link.
    fn connected_over_ble(lang: Lang) -> (App, crate::transport::testing::MemoryLink) {
        let mut app = App {
            lang,
            ..App::default()
        };
        let wire = crate::transport::testing::MemoryLink::ble();
        app.connect(wire.link()).expect("the memory link opens");
        assert!(pump_until(&mut app, |app| app.connected()));
        assert_eq!(app.transport, Transport::Ble);
        (app, wire)
    }

    /// Build one *unfinished* management fragment: a response in progress.
    ///
    /// A scan streams its results this way — the firmware sends each one with
    /// no final flag, and only `@scan done` ends it — so a test that wants to
    /// see the list grow needs this rather than `answer`'s finished frame.
    fn partial(id: u32, body: &str) -> Vec<u8> {
        let mut frame = Vec::new();
        frame.extend_from_slice(b"LK");
        frame.push(crate::protocol::MGMT_API_MAJOR);
        frame.push(2);
        frame.extend_from_slice(&id.to_le_bytes());
        frame.extend_from_slice(&(body.len() as u16).to_le_bytes());
        frame.extend_from_slice(&0u16.to_le_bytes());
        frame.extend_from_slice(body.as_bytes());
        frame
    }

    /// Build one management response for `id`, framed the way the bridge
    /// frames them, ending with the flag that says the answer is complete.
    fn answer(id: u32, body: &str) -> Vec<u8> {
        let mut frame = Vec::new();
        frame.extend_from_slice(b"LK");
        frame.push(crate::protocol::MGMT_API_MAJOR);
        frame.push(2);
        frame.extend_from_slice(&id.to_le_bytes());
        frame.extend_from_slice(&(body.len() as u16).to_le_bytes());
        frame.extend_from_slice(&crate::protocol::MGMT_FLAG_FINAL.to_le_bytes());
        frame.extend_from_slice(body.as_bytes());
        frame
    }

    /// The management frames the host sent, once they have left for the wire.
    ///
    /// Polled rather than read once, because the bytes cross a thread on their
    /// way out and asking immediately is asking before they have arrived.
    fn asked_frames(wire: &crate::transport::testing::MemoryLink) -> Vec<Vec<u8>> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let frames = wire.take_management();
            if !frames.is_empty() || Instant::now() > deadline {
                return frames;
            }
            thread::sleep(Duration::from_millis(1));
        }
    }

    /// The id the host just asked with, read out of the frame it sent.
    ///
    /// Read from the bytes rather than parsed, because the header parser
    /// accepts *responses* only — a request's own header is deliberately not
    /// something it accepts, so that a reply can never be mistaken for the
    /// question that caused it.
    fn asked_id(wire: &crate::transport::testing::MemoryLink) -> u32 {
        // The *last* frame, not the first: a link that has just opened is
        // mid-sequence through the on-connect reads, so the frame this call is
        // about is the latest one out — and the earlier ones belong to the
        // questions that came before it.
        let frames = asked_frames(wire);
        assert!(!frames.is_empty(), "no question was asked");
        let bytes = frames.last().expect("one frame");
        assert_eq!(&bytes[..2], b"LK", "the magic");
        assert_eq!(bytes[3], crate::protocol::MGMT_REQUEST_TYPE);
        let expected = u16::from_le_bytes([bytes[8], bytes[9]]);
        assert!(expected > 0, "nothing was asked");
        u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]])
    }

    /// The command body of the last question that went out.
    ///
    /// The management header is a fixed prefix, so the body is everything after
    /// it: this is how a test says *which* question was asked without parsing
    /// the frame properly, since parsing belongs to the protocol module.
    /// Whether `expected` is among the questions that went out.
    ///
    /// All of them, not the last: opening a link asks the on-connect reads in
    /// front of whatever a test then does, so the frame a test cares about is
    /// rarely the newest one out. This is the assertion these tests actually
    /// mean — "the command was sent" — stated directly rather than inferred
    /// from a position that the hello has moved.
    fn asked_includes(wire: &crate::transport::testing::MemoryLink, expected: &[u8]) -> bool {
        asked_frames(wire)
            .iter()
            .any(|bytes| bytes[crate::protocol::MGMT_HEADER_SIZE..] == *expected)
    }

    /// Asking the bridge what it is must fill the grid it is drawn from.
    ///
    /// This is the whole point of the management channel, and it is worth an
    /// end-to-end test because every layer it touches — the ask, the frame, the
    /// reassembly, the parse, the grid — can be right alone and still leave the
    /// view empty.
    #[test]
    fn asking_the_bridge_about_itself_fills_the_diagnostics_grid() {
        let (mut app, wire) = connected_over_ble(Lang::Zh);
        app.ask_device(Query::Diagnostics).expect("the link is up");
        assert!(app.diagnostics.loading, "the grid says it is waiting");

        let id = asked_id(&wire);
        wire.feed_management(answer(
            id,
            "@info fw version=0.2.0 zephyr=4.4.1\n@info sys uptime_ms=123456\n@info done",
        ));

        assert!(pump_until(&mut app, |app| app.diagnostics.has_data()));
        assert!(!app.diagnostics.loading);
        assert_eq!(
            app.diagnostics.groups["fw"]["version"], "0.2.0",
            "the firmware version came from the device, not from anywhere else"
        );
        assert_eq!(app.diagnostics.groups["sys"]["uptime_ms"], "123456");
        assert!(
            app.diagnostics.updated.is_some(),
            "and it is stamped with when it was read"
        );
        // And the answer is not also typed into the target's terminal.
        assert!(!app.terminal.visible_text().contains("@info"));
    }

    /// The UART read is where the top bar's own speed number comes from.
    #[test]
    fn reading_the_uart_fills_the_baud_the_chrome_shows() {
        let (mut app, wire) = connected_over_ble(Lang::Zh);
        app.ask_device(Query::Uart).expect("asked");
        let id = asked_id(&wire);
        wire.feed_management(answer(id, "OK uart=230400,8,N,1,none"));
        assert!(pump_until(&mut app, |app| app.baud == Some(230400)));
        // Not "the network view has stopped loading": opening a link asks the
        // on-connect reads, and the one after this is still in flight. What
        // this read itself left behind is the baud, and that is what landed.
        assert_eq!(app.baud, Some(230400));
    }

    /// A WiFi read lands in the network view rather than in a toast nobody
    /// asked for.
    #[test]
    fn reading_wifi_fills_the_network_view() {
        let (mut app, wire) = connected_over_ble(Lang::En);
        app.ask_device(Query::Wifi).expect("asked");
        let id = asked_id(&wire);
        wire.feed_management(answer(
            id,
            "OK wifi=connected,ssid=BenchNet,ip=192.168.1.44",
        ));
        assert!(pump_until(&mut app, |app| app.network.wifi_connected()));
        let wifi = app.network.wifi.clone().expect("reported");
        assert_eq!(wifi.ssid, "BenchNet");
        assert_eq!(wifi.ip, "192.168.1.44");
    }

    /// Setting the UART asks for the setting back, so the frame shows what the
    /// bridge is running rather than what was meant. A firmware that clamps
    /// 1000000 to something it can reach must therefore be believed.
    #[test]
    fn setting_the_uart_shows_the_rate_the_bridge_kept() {
        let (mut app, wire) = connected_over_ble(Lang::Zh);
        let requested = accessory::UartSettings {
            baud: 1_000_000,
            data_bits: 8,
            parity: "n".to_string(),
            stop_bits: 1,
            flow: "none".to_string(),
        };
        app.set_uart(&requested).expect("sent");
        let id = asked_id(&wire);
        wire.feed_management(answer(id, "OK uart=921600,8,N,1,none"));
        assert!(pump_until(&mut app, |app| app.baud == Some(921_600)));
        assert_ne!(app.baud, Some(requested.baud), "the bridge wins");
    }

    /// An answer that never comes is reported as one, rather than leaving the
    /// view reading "loading…" until somebody notices.
    #[test]
    fn a_question_the_bridge_ignores_says_so() {
        let (mut app, _wire) = connected_over_ble(Lang::Zh);
        app.ask_device(Query::Diagnostics).expect("asked");
        assert!(app.diagnostics.loading);
        // Nothing answers. Wait past the conversation's own patience.
        thread::sleep(crate::device::REPLY_TIMEOUT + Duration::from_millis(50));
        app.pump();
        assert!(!app.diagnostics.loading, "it stopped waiting");
        assert!(app.diagnostics.error.is_some(), "and said why");
        assert!(app.notices.iter().any(|n| n.level == NoticeLevel::Warn));
    }

    /// A bridge that answers with an error keeps the last good grid: replacing
    /// a working reading with a failure puts numbers on screen that were never
    /// measured, which is worse than showing none.
    #[test]
    fn a_failed_read_leaves_the_last_good_reading_alone() {
        let (mut app, wire) = connected_over_ble(Lang::Zh);
        app.ask_device(Query::Diagnostics).expect("asked");
        let id = asked_id(&wire);
        wire.feed_management(answer(id, "@info fw version=0.2.0\n@info done"));
        assert!(pump_until(&mut app, |app| app.diagnostics.has_data()));

        app.ask_device(Query::Diagnostics).expect("asked again");
        let id = asked_id(&wire);
        wire.feed_management(answer(id, "ERR busy"));
        assert!(pump_until(&mut app, |app| app.diagnostics.error.is_some()));
        assert!(app.diagnostics.has_data(), "the earlier reading survives");
        assert_eq!(app.diagnostics.groups["fw"]["version"], "0.2.0");
    }

    /// Asking over a link with no management channel is refused here, with the
    /// reason, rather than sent nowhere: waiting three seconds to be told the
    /// bearer cannot do it is the wrong order.
    #[test]
    fn management_over_a_link_that_has_none_is_refused_by_name() {
        let (mut app, wire) = connected(Lang::Zh);
        let err = app
            .ask_device(Query::Diagnostics)
            .expect_err("stdio has none");
        assert!(
            err.contains("BLE"),
            "the refusal names the bearer it needs: {err}"
        );
        assert!(!app.diagnostics.loading, "so nothing is left waiting");
        assert!(wire.take_management().is_empty(), "and nothing went out");
    }

    /// A WiFi scan is the board's radio listening, not this host's — and the
    /// command is the third shape `@w` takes, which is why it has its own door
    /// rather than being `set_wifi` with an empty name.
    #[test]
    fn a_wifi_scan_is_asked_of_the_bridge_and_not_of_this_host() {
        let (mut app, wire) = connected_over_ble(Lang::En);
        app.scan_wifi().expect("asked");
        // Among all the frames, not the last: the on-connect reads go out
        // first and this is asked after them.
        assert!(
            asked_includes(&wire, b"@w scan"),
            "the whole command, nothing guessed at"
        );
    }

    /// A scan's answer is a list, and it goes to the view's own list rather
    /// than into the status slot: squeezing nine networks into "the network you
    /// are on" would show one where the board saw nine.
    #[test]
    fn a_wifi_scan_answer_fills_the_results_list() {
        let (mut app, wire) = connected_over_ble(Lang::En);
        app.scan_wifi().expect("asked");
        let id = asked_id(&wire);
        wire.feed_management(answer(id, "OK scan=9\nBenchNet -58 6\nGuest -71 11"));
        assert!(pump_until(&mut app, |app| !app
            .network
            .scan_results
            .is_empty()));
        assert_eq!(
            app.network.scan_results,
            vec!["BenchNet -58 6".to_string(), "Guest -71 11".to_string()],
            "the status line is dropped and the networks are kept"
        );
        assert!(app.network.wifi.is_none(), "a list is not a status");
    }

    /// A scan streams: the first network shows before the scan is over.
    ///
    /// The bridge answers one network at a time, so folding only at finalize
    /// would leave the view reading "scanning" for the whole scan and then fill
    /// at once — the same wait, with nothing to watch. This is the regression
    /// test for that: half an answer is fed, and the list already has a row.
    #[test]
    fn a_scan_fills_while_it_is_still_running() {
        let (mut app, wire) = connected_over_ble(Lang::En);
        app.scan_wifi().expect("asked");
        let id = asked_id(&wire);

        // One result line, without the final flag. The firmware streams a scan
        // exactly this way — `wifi_scan_respond()` in the firmware sends each
        // result with flags 0 and only `@scan done` carries FINAL — so this is
        // the real shape, not a convenience. A body with an `OK` line in it
        // would be recognised as a finished answer by shape alone, which is why
        // the status line is deliberately absent here.
        wire.feed_management(partial(id, "BenchNet -58 6"));

        assert!(pump_until(&mut app, |app| !app
            .network
            .scan_results
            .is_empty()));
        assert_eq!(
            app.network.scan_results,
            vec!["BenchNet -58 6".to_string()],
            "the first network is visible before the scan ends"
        );
        assert!(
            app.network.loading,
            "still waiting: the scan has not said it is finished"
        );
    }

    /// An approved change is the command, and the read that confirms it follows
    /// from the command rather than from whoever asked for it.
    #[test]
    fn an_approved_change_sends_the_command_the_card_showed() {
        let (mut app, wire) = connected_over_ble(Lang::En);
        app.run_accessory("@w=BenchNet,secret")
            .expect("the link is up");
        // Among all the frames, as above.
        assert!(
            asked_includes(&wire, b"@w=BenchNet,secret"),
            "the password goes to the wire"
        );
        assert!(
            app.waiting_on_device(),
            "and the read that confirms it is out"
        );
    }

    /// A command that is not one the bridge takes is refused rather than sent;
    /// the refusal is what keeps a bug in a caller from becoming traffic on the
    /// wire.
    #[test]
    fn a_command_the_bridge_does_not_take_is_refused() {
        let (mut app, wire) = connected_over_ble(Lang::En);
        let err = app
            .run_accessory("reboot")
            .expect_err("not a bridge command");
        assert!(!err.is_empty());
        let deadline = Instant::now() + Duration::from_millis(200);
        while wire.take_management().is_empty() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(2));
        }
        assert!(wire.take_management().is_empty(), "nothing went out for it");
    }

    /// Line mode holds a typed line until Enter, which is what a target with a
    /// line discipline wants — and it is echoed as it is held, so the line can
    /// be read before it is sent.
    #[test]
    fn a_line_is_held_until_enter_and_only_then_written() {
        let (mut app, wire) = connected(Lang::Zh);
        app.line_mode = true;
        app.terminal.echo = true;

        assert!(app.type_bytes(false, "ls"));
        assert!(app.type_bytes(false, " -l"));
        // Nothing has gone out yet, but the line can be read back.
        assert_eq!(app.pending_line, "ls -l");
        assert!(
            app.terminal.visible_text().contains("ls -l"),
            "what was typed is on the screen"
        );
        let deadline = Instant::now() + Duration::from_millis(200);
        while wire.sent_len() == 0 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(wire.sent_len(), 0, "nothing went out before Enter");

        assert!(app.type_bytes(true, "\n"));
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut sent = Vec::new();
        while sent.len() < 6 && Instant::now() < deadline {
            sent.extend(wire.take_sent());
            thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(String::from_utf8_lossy(&sent), "ls -l\n");
        assert!(app.pending_line.is_empty(), "the line went with it");
    }

    /// Without line mode a keystroke goes out at once: holding it would break
    /// every target that is doing its own editing, which is most of them.
    #[test]
    fn without_line_mode_a_keystroke_goes_straight_out() {
        let (mut app, wire) = connected(Lang::Zh);
        assert!(app.type_bytes(false, "x"));
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut sent = Vec::new();
        while sent.is_empty() && Instant::now() < deadline {
            sent.extend(wire.take_sent());
            thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(sent, b"x");
    }

    /// A half-typed line belongs to the session that typed it: carrying it into
    /// the next session would send a stranger your keystrokes.
    #[test]
    fn a_half_typed_line_does_not_survive_the_link() {
        let (mut app, wire) = connected(Lang::Zh);
        app.line_mode = true;
        assert!(app.type_bytes(false, "rm -rf"));
        assert_eq!(app.pending_line, "rm -rf");
        wire.close();
        assert!(pump_until(&mut app, |app| !app.connected()));
        assert!(app.pending_line.is_empty(), "the wire took it with it");
    }

    /// The watch reads every line the target prints, including the ones that
    /// arrive without a newline — which is exactly how a board dies: mid-line,
    /// with its prompt half written.
    #[test]
    fn the_watch_reports_a_panic_and_a_hanging_prompt_alike() {
        let (mut app, wire) = connected(Lang::Zh);
        assert!(app.watch.is_empty());
        wire.feed(b"Booting Linux on physical CPU 0x0\nKernel panic - not syncing\n");
        assert!(pump_until(&mut app, |app| !app.watch.is_empty()));
        let joined = app.watch.join(" | ");
        assert!(joined.contains("panic"), "{joined}");
    }

    /// Binary must not be read as though it were text: the watch
    /// looking for a word cannot be the thing that corrupts a file.
    #[test]
    fn binary_output_is_not_mistaken_for_a_finding() {
        let (mut app, wire) = connected(Lang::Zh);
        wire.feed(&[0x00, 0xff, 0xfe, b'\n', 0x80, 0x81, b'\n']);
        app.pump();
        thread::sleep(Duration::from_millis(10));
        app.pump();
        assert!(app.watch.is_empty(), "{:?}", app.watch);
    }
}
