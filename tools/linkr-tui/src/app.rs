//! Application state: everything a frame needs, and nothing a frame does not.
//!
//! The state is plain data with small, total transitions. No view owns state
//! and no state knows about a view: `ui::*` reads `&App`, the event loop
//! mutates it, and every mutation is a method here so it can be tested without
//! a terminal.

use crate::event::{ConnectionState, NoticeLevel, View};
use crate::i18n::Lang;
use crate::theme::{Theme, ThemeChoice};
use crate::views::diagnostics::DiagnosticsState;

/// Which link carries the session. The sidebar names it and the CLI pins it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Transport {
    #[default]
    Ble,
    Lan,
}

impl Transport {
    /// The word the sidebar shows for this link.
    pub fn name(self) -> &'static str {
        match self {
            Transport::Ble => "BLE",
            Transport::Lan => "LAN",
        }
    }

    /// The other one — what the toggle switches to.
    pub fn toggled(self) -> Transport {
        match self {
            Transport::Ble => Transport::Lan,
            Transport::Lan => Transport::Ble,
        }
    }
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

/// Everything one frame paints.
#[derive(Debug, Clone)]
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
    /// The assistant's session state for the assistant view.
    pub assistant: crate::views::assistant::AssistantState,
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
            assistant: crate::views::assistant::AssistantState::default(),
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

    /// The transport as the sidebar shows it, with its lock suffix when a CLI
    /// flag pinned it.
    pub fn transport_text(&self) -> String {
        let s = self.strings();
        if self.transport_locked {
            format!("{} {}", self.transport.name(), s.transport_locked)
        } else {
            self.transport.name().to_string()
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

    /// Show a notice, dropping the oldest when the stack is full.
    pub fn push_notice(&mut self, notice: Notice) {
        self.notices.push(notice);
        while self.notices.len() > MAX_NOTICES {
            self.notices.remove(0);
        }
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
}
