//! Network view: WiFi and WebDAV management, as the design's
//! `tui_network_zh_dark.png` lays it out (WEB_UX_SPEC section 6).
//!
//! The model is the accessory layer's wire data — a WiFi status, a WebDAV
//! status, scan results — and the painter is the pure function from that data
//! plus a width to the pane's lines. Everything management-channel related is
//! BLE-only, so on a LAN session the view says so rather than showing controls
//! that cannot work.

use ratatui::text::{Line, Span};

use crate::accessory::{WebdavStatus, WifiStatus};
use crate::app::App;
use crate::dialogs::{self, ConfirmAction};
use crate::text;
use crate::theme::Theme;

/// One row the network view can be stood on.
///
/// Named rather than numbered because the view draws them in a fixed order but
/// hides two of them when a network is already joined, and an index that meant
/// "password" on an unconnected board and "off" on a connected one would be a
/// key that does something other than what the highlighted row promises.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkRow {
    /// The network's name.
    Ssid,
    /// Its password. Only reachable while nothing is joined.
    Password,
    /// `@w scan` — ask the board what it can see.
    Scan,
    /// `@w=<ssid>,<password>` — join.
    Connect,
    /// `@w off` — forget.
    WifiOff,
    /// `@w?` — read the status back.
    WifiStatus,
    /// Where the log goes.
    Url,
    /// `@d=<url>`.
    WebdavSet,
    /// `@d off`.
    WebdavOff,
    /// `@d?`.
    WebdavStatus,
}

/// Every row, in the order the view draws them.
pub const ROWS: [NetworkRow; 10] = [
    NetworkRow::Ssid,
    NetworkRow::Password,
    NetworkRow::Scan,
    NetworkRow::Connect,
    NetworkRow::WifiOff,
    NetworkRow::WifiStatus,
    NetworkRow::Url,
    NetworkRow::WebdavSet,
    NetworkRow::WebdavOff,
    NetworkRow::WebdavStatus,
];

impl NetworkRow {
    /// The rows reachable from here, in order.
    ///
    /// The password row is left out while a network is joined: there is no
    /// password to set on a network that is already up, and a row that did
    /// nothing when pressed would be worse than a row that is not there.
    fn reachable(self, connected: bool) -> bool {
        !connected || self != NetworkRow::Password
    }

    /// The next row down, wrapping.
    fn next(self, connected: bool) -> NetworkRow {
        self.step(1, connected)
    }

    /// The next row up, wrapping.
    fn previous(self, connected: bool) -> NetworkRow {
        self.step(-1, connected)
    }

    /// Walk `direction` rows, skipping the ones not reachable now.
    fn step(self, direction: isize, connected: bool) -> NetworkRow {
        let here = ROWS
            .iter()
            .position(|row| *row == self)
            .expect("every row is in the table");
        let count = ROWS.len() as isize;
        let mut offset = 1;
        // Bounded by the table's own length, so a table with no reachable row
        // but this one comes back here rather than spinning.
        while offset <= count {
            let index = (here as isize + direction * offset).rem_euclid(count) as usize;
            let candidate = ROWS[index];
            if candidate.reachable(connected) {
                return candidate;
            }
            offset += 1;
        }
        self
    }
}

/// Everything the network view knows. Filled by the session layer; read here
/// as plain data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkState {
    /// The device's last reported WiFi status (`@w?`).
    pub wifi: Option<WifiStatus>,
    /// The device's last reported WebDAV status (`@d?`).
    pub webdav: Option<WebdavStatus>,
    /// The SSID typed into the form, echoed back verbatim.
    pub ssid_input: String,
    /// Scan results, as the device reported them.
    pub scan_results: Vec<String>,
    /// A read or action is in flight.
    pub loading: bool,
    /// The last failure, already worded for the reader.
    pub error: Option<String>,
    /// The row the keyboard is on.
    pub focus: NetworkRow,
}

impl Default for NetworkState {
    fn default() -> NetworkState {
        NetworkState {
            wifi: None,
            webdav: None,
            ssid_input: String::new(),
            scan_results: Vec::new(),
            loading: false,
            error: None,
            focus: NetworkRow::Ssid,
        }
    }
}

impl NetworkState {
    /// Whether the WiFi section shows a live connection.
    pub fn wifi_connected(&self) -> bool {
        self.wifi.as_ref().is_some_and(|w| w.state == "connected")
    }

    /// Move the focus down one reachable row.
    pub fn focus_next(&mut self) {
        let connected = self.wifi_connected();
        self.focus = self.focus.next(connected);
    }

    /// Move the focus up one reachable row.
    pub fn focus_previous(&mut self) {
        let connected = self.wifi_connected();
        self.focus = self.focus.previous(connected);
    }
}

/// One `label：value` row, in the design's inset style: two-space indent, the
/// label in focus colour, the value in body text.
fn field_row(label: &str, value: &str, selected: bool, width: u16, theme: &Theme) -> Line<'static> {
    let s = text_of_label_value(label, value);
    if selected {
        // The design paints the selected row as a full chip, marker included.
        let chip = format!("▸ {s}");
        return Line::from(Span::styled(
            text::clip_to(&chip, width as usize),
            theme.selected(),
        ));
    }
    let budget = width as usize;
    let indent = 2;
    let label_style = theme.focus();
    let mut spans = vec![Span::styled(" ".repeat(indent), theme.text())];
    let label_cols = text::columns(label);
    if indent + label_cols >= budget {
        spans.push(Span::styled(
            text::clip_to(label, budget.saturating_sub(indent)),
            label_style,
        ));
        return Line::from(spans);
    }
    spans.push(Span::styled(label.to_string(), label_style));
    let rest = budget - indent - label_cols;
    spans.push(Span::styled(text::clip_to(value, rest), theme.text()));
    Line::from(spans)
}

fn text_of_label_value(label: &str, value: &str) -> String {
    format!("{label}{value}")
}

/// One `[action]（仅限 BLE）` row.
fn action_row(
    action: &str,
    ble_only: &str,
    selected: bool,
    width: u16,
    theme: &Theme,
) -> Line<'static> {
    let row = format!("  {action}{ble_only}");
    if selected {
        // The marker is added inside the clip rather than outside it: a chip
        // built first and clipped after would lose the action's own name at a
        // narrow width and keep the marker, which is the wrong thing to lose.
        let chip = format!(
            "▸ {}",
            text::clip_to(&row, width.saturating_sub(2) as usize)
        );
        return Line::from(Span::styled(chip, theme.selected()));
    }
    Line::from(Span::styled(
        text::clip_to(&row, width as usize),
        theme.text(),
    ))
}

/// A section heading in the accent colour.
fn heading(text: &str, width: u16, theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(
        text::clip_to(text, width as usize),
        theme.section_header(),
    ))
}

/// A muted note line.
fn note(text: &str, width: u16, theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(
        text::clip_to(text, width as usize),
        theme.muted(),
    ))
}

/// The whole body. The order is the design's: WiFi (note, ssid, password,
/// actions), scan results, WebDAV (url, actions).
pub fn lines(app: &App, theme: &Theme, width: u16) -> Vec<Line<'static>> {
    let s = app.strings();
    let state = &app.network;
    let mut lines = vec![
        Line::from(""),
        heading(
            app.lang.view_name(crate::event::View::Network),
            width,
            theme,
        ),
        Line::from(""),
        heading(s.net_wifi_section, width, theme),
        note(s.net_wifi_needs_ble, width, theme),
    ];

    if state.loading {
        lines.push(note(s.diag_loading, width, theme));
    }
    if let Some(error) = &state.error {
        lines.push(Line::from(Span::styled(
            text::clip_to(error, width as usize),
            theme.danger(),
        )));
    }

    // A live connection replaces the form with the connected summary; a
    // connected network's name is shown where the input field was.
    let connected = state.wifi_connected();
    let on = |row: NetworkRow| state.focus == row && row.reachable(connected);
    let ssid_value = if connected {
        state
            .wifi
            .as_ref()
            .map(|w| {
                if w.ssid.is_empty() {
                    s.net_empty_value
                } else {
                    w.ssid.as_str()
                }
            })
            .unwrap_or(s.net_empty_value)
    } else if state.ssid_input.is_empty() {
        s.net_empty_value
    } else {
        state.ssid_input.as_str()
    };
    lines.push(field_row(
        &format!("{}{}", s.net_ssid_label, s.label_separator),
        ssid_value,
        on(NetworkRow::Ssid),
        width,
        theme,
    ));
    if !connected {
        lines.push(field_row(
            &format!("{}{}", s.net_password_label, s.label_separator),
            s.net_empty_value,
            on(NetworkRow::Password),
            width,
            theme,
        ));
    }
    for (row, label) in [
        (NetworkRow::Scan, s.net_scan),
        (NetworkRow::Connect, s.net_connect),
        (NetworkRow::WifiOff, s.net_wifi_off),
        (NetworkRow::WifiStatus, s.net_status),
    ] {
        lines.push(action_row(label, s.net_ble_only, on(row), width, theme));
    }

    // The state row, once the device has answered `@w?`.
    if let Some(wifi) = &state.wifi {
        let state_word = if wifi.state == "connected" {
            s.net_state_on
        } else if wifi.state == "off" {
            s.net_state_off
        } else {
            &wifi.state
        };
        let ip = if wifi.ip.is_empty() { "-" } else { &wifi.ip };
        lines.push(note(&format!("WiFi: {state_word} · IP {ip}"), width, theme));
    }

    lines.push(Line::from(""));
    lines.push(heading(s.net_results_section, width, theme));
    if state.scan_results.is_empty() {
        lines.push(note(&format!("  {}", s.net_no_results), width, theme));
    } else {
        for result in &state.scan_results {
            lines.push(note(&format!("  {result}"), width, theme));
        }
    }

    lines.push(Line::from(""));
    lines.push(heading(s.net_webdav_section, width, theme));
    let url = state
        .webdav
        .as_ref()
        .map(|d| d.url.as_str())
        .filter(|u| !u.is_empty())
        .unwrap_or(s.net_empty_value);
    lines.push(field_row(
        &format!("{}{}", s.net_url_label, s.label_separator),
        url,
        on(NetworkRow::Url),
        width,
        theme,
    ));
    for (row, word) in [
        (NetworkRow::WebdavSet, s.net_set),
        (NetworkRow::WebdavOff, s.net_disable),
        (NetworkRow::WebdavStatus, s.net_status),
    ] {
        lines.push(action_row(word, s.net_ble_only, on(row), width, theme));
    }
    if let Some(dav) = &state.webdav {
        let state_word = if dav.state == "off" {
            s.net_state_off
        } else {
            &dav.state
        };
        lines.push(note(&format!("WebDAV: {state_word}"), width, theme));
    }

    lines.push(Line::from(""));
    lines.push(note(s.net_keys_hint, width, theme));
    lines
}

/// Handle one key press while the network view has the keyboard.
///
/// The view is a form, so it takes the arrow keys for itself; everything else
/// it does not understand is left for the caller, which is what keeps the
/// global chords working here.
///
/// A row that would change how the bridge is set up does not do it directly: it
/// raises the approval card naming the change. Joining a network is not undoable
/// — the board drops off the old one before it is on the new one — and a form
/// that changed the device as you walked down it would be a form nobody dared
/// use.
pub fn handle_key(app: &mut App, key: crossterm::event::KeyEvent) -> bool {
    use crossterm::event::{KeyCode, KeyModifiers};
    let plain = key.modifiers == KeyModifiers::NONE || key.modifiers == KeyModifiers::SHIFT;
    if plain && key.code == KeyCode::Down {
        app.network.focus_next();
        return true;
    }
    if plain && key.code == KeyCode::Up {
        app.network.focus_previous();
        return true;
    }
    if key.code == KeyCode::Enter && key.modifiers == KeyModifiers::NONE {
        activate(app);
        return true;
    }
    false
}

/// Do what the focused row says.
fn activate(app: &mut App) {
    use crate::device::Query;
    // The password row is not reachable while a network is joined, so a focus
    // left on it by an earlier state is moved rather than acted on.
    let row = app.network.focus;
    if app.network.wifi_connected() && row == NetworkRow::Password {
        app.network.focus = NetworkRow::Ssid;
        return;
    }
    match row {
        NetworkRow::Ssid | NetworkRow::Password => {
            crate::dialogs::prompt_wifi_ssid(app);
        }
        NetworkRow::Url | NetworkRow::WebdavSet => dialogs::prompt_webdav_url(app),
        NetworkRow::Connect => {
            // The name is already in the form, so joining goes straight to the
            // password: asking for a name the user can see on the row above it
            // would be asking twice.
            dialogs::prompt_wifi_ssid(app);
        }
        NetworkRow::Scan => dialogs::ask(app, ConfirmAction::Accessory("@w scan".to_string())),
        NetworkRow::WifiOff => dialogs::ask(app, ConfirmAction::Accessory("@w off".to_string())),
        NetworkRow::WebdavOff => dialogs::ask(app, ConfirmAction::Accessory("@d off".to_string())),
        // Reads are not guarded: they change nothing, and a card asking
        // permission to look at something is a card that teaches people to
        // press yes without reading.
        NetworkRow::WifiStatus => {
            if let Err(why) = app.ask_device(Query::Wifi) {
                app.push_notice(crate::app::Notice {
                    text: why,
                    level: crate::event::NoticeLevel::Error,
                });
            }
        }
        NetworkRow::WebdavStatus => {
            if let Err(why) = app.ask_device(Query::Webdav) {
                app.push_notice(crate::app::Notice {
                    text: why,
                    level: crate::event::NoticeLevel::Error,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Transport;
    use crate::dialogs::Dialog;
    use crate::event::ConnectionState;
    use crate::i18n::Lang;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn app(lang: Lang) -> App {
        App {
            lang,
            state: ConnectionState::Connected,
            transport: Transport::Ble,
            ..App::default()
        }
    }

    fn body_text(rows: &[Line<'static>]) -> String {
        rows.iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<Vec<_>>()
            .join("")
    }

    /// The design's rows all appear, in its order, in Chinese.
    #[test]
    fn the_body_matches_the_designed_sections() {
        let theme = Theme::DARK;
        let mut a = app(Lang::Zh);
        a.network.wifi = Some(WifiStatus {
            state: "off".into(),
            ssid: String::new(),
            ip: String::new(),
        });
        let text = body_text(&lines(&a, &theme, 80));
        for needle in [
            "网络",
            "WiFi",
            "网络名称",
            "密码",
            "[扫描]",
            "[连接 WiFi]",
            "[WiFi 关闭]",
            "[WiFi 状态]",
            "扫描结果",
            "暂无结果",
            "WebDAV 日志上传",
            "目标 URL",
            "[设置]",
            "[关闭]",
            "仅限 BLE",
        ] {
            assert!(text.contains(needle), "missing {needle:?} in {text:?}");
        }
    }

    /// The English wording differs and carries the same structure.
    #[test]
    fn the_body_is_translated() {
        let theme = Theme::DARK;
        let a = app(Lang::En);
        let text = body_text(&lines(&a, &theme, 80));
        assert!(text.contains("Network name"), "{text}");
        assert!(text.contains("[Scan]"), "{text}");
        assert!(text.contains("(BLE only)"), "{text}");
        assert!(text.contains("Scan results"), "{text}");
    }

    /// A connected network replaces the password row with the live summary.
    #[test]
    fn a_connected_network_shows_its_summary() {
        let theme = Theme::DARK;
        let mut a = app(Lang::En);
        a.network.wifi = Some(WifiStatus {
            state: "connected".into(),
            ssid: "MyNet".into(),
            ip: "192.168.1.5".into(),
        });
        let text = body_text(&lines(&a, &theme, 80));
        assert!(text.contains("MyNet"), "{text}");
        assert!(text.contains("192.168.1.5"), "{text}");
        assert!(!text.contains("Password"), "{text}");
    }

    /// Scan results are listed when the device reported any.
    #[test]
    fn scan_results_are_listed() {
        let theme = Theme::DARK;
        let mut a = app(Lang::En);
        a.network.scan_results = vec!["Home  −58 dBm".into(), "Office".into()];
        let text = body_text(&lines(&a, &theme, 80));
        assert!(text.contains("Home"), "{text}");
        assert!(text.contains("Office"), "{text}");
        assert!(!text.contains("no results yet"), "{text}");
    }

    /// The focus walks the rows, and skips the password row while a network is
    /// joined — a row that did nothing when pressed would be worse than a row
    /// that is not there.
    #[test]
    fn the_focus_walks_the_reachable_rows() {
        let mut state = NetworkState::default();
        assert_eq!(state.focus, NetworkRow::Ssid);
        state.focus_next();
        assert_eq!(state.focus, NetworkRow::Password);
        state.focus_next();
        assert_eq!(state.focus, NetworkRow::Scan);
        state.focus_previous();
        assert_eq!(state.focus, NetworkRow::Password);

        state.wifi = Some(WifiStatus {
            state: "connected".into(),
            ssid: "Up".into(),
            ip: "10.0.0.1".into(),
        });
        // Standing on a row that has just gone away moves off it rather than
        // keeping a focus that no longer means anything.
        state.focus_next();
        assert_eq!(state.focus, NetworkRow::Scan, "the password row is skipped");
        state.focus_previous();
        assert_eq!(state.focus, NetworkRow::Ssid, "and is not walked into");
    }

    /// The focus wraps, so walking up from the first row lands on the last and
    /// the keyboard never reaches a dead end.
    #[test]
    fn the_focus_wraps_at_both_ends() {
        let mut state = NetworkState::default();
        state.focus_previous();
        assert_eq!(state.focus, NetworkRow::WebdavStatus);
        state.focus_next();
        assert_eq!(state.focus, NetworkRow::Ssid);
    }

    /// Enter on a row that changes the bridge asks first, and the card names
    /// the change rather than the command — the command carries the password.
    #[test]
    fn a_change_row_asks_before_it_changes_anything() {
        let mut a = app(Lang::En);
        a.network.focus = NetworkRow::WifiOff;
        assert!(handle_key(
            &mut a,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
        ));
        let Some(Dialog::Confirm(card)) = &a.dialog else {
            panic!("the approval card came up: {:?}", a.dialog);
        };
        assert!(
            card.prompt.contains("WiFi") && !card.prompt.contains("@w"),
            "the card words the change: {:?}",
            card.prompt
        );
        assert_eq!(card.action, ConfirmAction::Accessory("@w off".to_string()));
        assert!(!card.yes, "and defaults to not doing it");
    }

    /// Enter on a read asks the bridge at once: a read changes nothing, and a
    /// card asking permission to look at something teaches people to press yes
    /// without reading.
    #[test]
    fn a_read_row_just_reads() {
        let mut a = app(Lang::En);
        a.network.focus = NetworkRow::WifiStatus;
        assert!(handle_key(
            &mut a,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
        ));
        // Not connected, so the refusal is the answer — but it is a refusal and
        // not a card, which is the point.
        assert!(a.dialog.is_none(), "no card for a read");
        assert!(
            a.network.error.is_none(),
            "and nothing was said to be wrong"
        );
    }

    /// A key the view does not use is left for the caller, so the global chords
    /// and the device's own keystrokes still work here.
    #[test]
    fn keys_the_view_does_not_use_are_left_alone() {
        let mut a = app(Lang::En);
        assert!(!handle_key(
            &mut a,
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)
        ));
        assert!(!handle_key(
            &mut a,
            KeyEvent::new(KeyCode::Down, KeyModifiers::CONTROL)
        ));
    }

    /// Every row fits the pane it was given, in both languages, at any width.
    #[test]
    fn every_row_fits_the_pane() {
        let theme = Theme::DARK;
        for width in [8u16, 20, 40, 60, 100] {
            for lang in [Lang::Zh, Lang::En] {
                let mut a = app(lang);
                a.network.wifi = Some(WifiStatus {
                    state: "connected".into(),
                    ssid: "Some-Net".into(),
                    ip: "10.0.0.4".into(),
                });
                a.network.scan_results = vec!["A-very-long-network-name-indeed".into()];
                for line in lines(&a, &theme, width) {
                    let joined: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                    assert!(
                        text::columns(&joined) <= width as usize,
                        "{lang:?} at {width}: {joined:?}"
                    );
                }
            }
        }
    }

    /// An error is painted in the danger colour; the notes are muted.
    #[test]
    fn colours_come_from_the_theme() {
        let theme = Theme::DARK;
        let mut a = app(Lang::En);
        a.network.error = Some("timed out".into());
        let painted = lines(&a, &theme, 60);
        let flat: Vec<(String, Option<ratatui::style::Color>)> = painted
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| (s.content.to_string(), s.style.fg))
            .collect();
        let err = flat.iter().find(|(t, _)| t == "timed out").unwrap();
        assert_eq!(err.1, Some(theme.danger));
        let head = flat.iter().find(|(t, _)| t == "WiFi").unwrap();
        assert_eq!(head.1, Some(theme.accent));
    }
}
