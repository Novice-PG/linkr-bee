//! Network view: WiFi scan/connect form and WebDAV controls, all through
//! `@w`/`@d` management commands with the section 3.6 capability gates
//! (BLE-only; disabled over LAN).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use tokio::sync::oneshot;

use super::replies::{
    parse_scan_line, parse_webdav_reply, parse_wifi_reply, ScanResult, WebdavStatus, WifiStatus,
};
use super::state::{App, Focus, TextField};
use crate::event::NoticeLevel;
use crate::protocol::mgmt::{MGMT_CAP_ASYNC_EVENTS, MGMT_CAP_WEBDAV, MGMT_CAP_WIFI};
use crate::protocol::MgmtReply;

/// `ACCESSORY_LIMITS` of the web client.
pub const SSID_MAX: usize = 32;
pub const PASSWORD_MAX: usize = 64;
pub const WEBDAV_URL_MAX: usize = 256;
/// `WIFI_SCAN_TIMEOUT_MS` / `WIFI_OPERATION_TIMEOUT_SECS`.
const SCAN_WAIT: std::time::Duration = std::time::Duration::from_secs(35);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingKind {
    Scan,
    WifiConnect,
    WifiOff,
    WifiStatus,
    WebdavSet,
    WebdavOff,
    WebdavStatus,
}

/// One selectable row of the network view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetEntry {
    Ssid,
    Password,
    Webdav,
    Scan,
    WifiConnect,
    WifiOff,
    WifiStatus,
    WebdavSet,
    WebdavOff,
    WebdavStatus,
    Result(usize),
}

#[derive(Default)]
pub struct NetworkState {
    pub ssid: TextField,
    pub password: TextField,
    pub webdav: TextField,
    pub show_password: bool,
    pub selection: usize,
    pub scan: Vec<ScanResult>,
    pub scan_running: bool,
    /// Status line under the form (web `#wifiFeedback`).
    pub feedback: String,
    pub wifi_status: Option<WifiStatus>,
    pub webdav_status: Option<WebdavStatus>,
    pending: Vec<(PendingKind, oneshot::Receiver<Result<MgmtReply, String>>)>,
}

impl NetworkState {
    pub fn new() -> Self {
        Self {
            feedback: "Scan to select a nearby 2.4 GHz network.".to_string(),
            ..Default::default()
        }
    }

    pub fn can_wifi(&self, app: &App) -> bool {
        let caps = app.capabilities();
        app.ble_connected() && caps & MGMT_CAP_WIFI != 0
    }

    pub fn can_wifi_scan(&self, app: &App) -> bool {
        self.can_wifi(app) && app.capabilities() & MGMT_CAP_ASYNC_EVENTS != 0
    }

    pub fn can_webdav(&self, app: &App) -> bool {
        app.ble_connected() && app.capabilities() & MGMT_CAP_WEBDAV != 0
    }
}

pub fn entries(app: &App) -> Vec<NetEntry> {
    let mut list = vec![
        NetEntry::Ssid,
        NetEntry::Password,
        NetEntry::Scan,
        NetEntry::WifiConnect,
        NetEntry::WifiOff,
        NetEntry::WifiStatus,
        NetEntry::Webdav,
        NetEntry::WebdavSet,
        NetEntry::WebdavOff,
        NetEntry::WebdavStatus,
    ];
    for i in 0..app.network.scan.len() {
        list.push(NetEntry::Result(i));
    }
    list
}

fn field_line(label: &str, field: &TextField, mask: bool, selected: bool) -> Line<'static> {
    let (text, _) = field.display(if mask { Some('*') } else { None });
    let marker = if selected { "▸ " } else { "  " };
    let style = if selected {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    let value = if text.is_empty() {
        "–".to_string()
    } else {
        text
    };
    Line::from(vec![
        Span::styled(marker.to_string(), Style::default().fg(Color::Yellow)),
        Span::styled(format!("{label}: "), Style::default().fg(Color::Cyan)),
        Span::styled(value, style),
    ])
}

fn action_line(label: &str, enabled: bool, selected: bool, pending: bool) -> Line<'static> {
    let marker = if selected { "▸ " } else { "  " };
    let mut style = if selected {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else if !enabled {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default().fg(Color::White)
    };
    if pending {
        style = style.fg(Color::LightBlue);
    }
    let suffix = if pending {
        " …"
    } else if !enabled {
        " (BLE only)"
    } else {
        ""
    };
    Line::from(vec![
        Span::styled(marker.to_string(), Style::default().fg(Color::Yellow)),
        Span::styled(format!("[{label}]{suffix}"), style),
    ])
}

/// Body of the Network view (rendered in the center pane).
pub fn render_lines(app: &App) -> Vec<Line<'static>> {
    let net = &app.network;
    let mut lines: Vec<Line<'static>> = Vec::new();

    lines.push(Line::from(Span::styled(
        "WiFi",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));
    if let Some(status) = &net.wifi_status {
        lines.push(Line::from(format!(
            "  connected network: {} · device IP: {}",
            if status.ssid.is_empty() {
                "–"
            } else {
                &status.ssid
            },
            if status.ip.is_empty() {
                "–"
            } else {
                &status.ip
            },
        )));
    }
    if !app.ble_connected() {
        lines.push(Line::from(Span::styled(
            "  Connect over BLE to manage WiFi (LAN transport has no management channel).",
            Style::default().fg(Color::DarkGray),
        )));
    }

    let wifi_ok = net.can_wifi(app);
    let scan_ok = net.can_wifi_scan(app);
    let pending = |kind: PendingKind| net.pending.iter().any(|(k, _)| *k == kind);

    lines.push(field_line(
        "Network name",
        &net.ssid,
        false,
        is_sel(app, NetEntry::Ssid),
    ));
    lines.push(field_line(
        "Password",
        &net.password,
        !net.show_password,
        is_sel(app, NetEntry::Password),
    ));
    lines.push(action_line(
        "Scan",
        scan_ok,
        is_sel(app, NetEntry::Scan),
        pending(PendingKind::Scan),
    ));
    lines.push(action_line(
        "Connect WiFi",
        wifi_ok,
        is_sel(app, NetEntry::WifiConnect),
        pending(PendingKind::WifiConnect) || pending(PendingKind::WifiOff),
    ));
    lines.push(action_line(
        "WiFi off",
        wifi_ok,
        is_sel(app, NetEntry::WifiOff),
        pending(PendingKind::WifiOff),
    ));
    lines.push(action_line(
        "WiFi status",
        wifi_ok,
        is_sel(app, NetEntry::WifiStatus),
        pending(PendingKind::WifiStatus),
    ));

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Scan results",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));
    if net.scan.is_empty() {
        lines.push(Line::from(Span::styled(
            if net.scan_running {
                "  scanning…".to_string()
            } else {
                "  no results yet".to_string()
            },
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        for (i, result) in net.scan.iter().enumerate() {
            let selected_here = is_sel(app, NetEntry::Result(i));
            let marker = if selected_here { "▸ " } else { "  " };
            let rssi = match result.rssi {
                Some(v) => format!("{v} dBm"),
                None => "–".to_string(),
            };
            let channel = match result.channel {
                Some(v) => format!(" ch {v}"),
                None => String::new(),
            };
            let security = result
                .security
                .as_deref()
                .map(|s| format!(" {s}"))
                .unwrap_or_default();
            let style = if selected_here {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            lines.push(Line::from(vec![
                Span::styled(marker.to_string(), Style::default().fg(Color::Yellow)),
                Span::styled(result.ssid.clone(), style),
                Span::styled(
                    format!("  {rssi}{channel}{security}"),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "WebDAV log upload",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));
    if let Some(status) = &net.webdav_status {
        lines.push(Line::from(format!(
            "  {} · {}",
            status.state,
            if status.url.is_empty() {
                "–"
            } else {
                &status.url
            },
        )));
    }
    let dav_ok = net.can_webdav(app);
    lines.push(field_line(
        "Target URL",
        &net.webdav,
        false,
        is_sel(app, NetEntry::Webdav),
    ));
    lines.push(action_line(
        "Set",
        dav_ok,
        is_sel(app, NetEntry::WebdavSet),
        pending(PendingKind::WebdavSet),
    ));
    lines.push(action_line(
        "Off",
        dav_ok,
        is_sel(app, NetEntry::WebdavOff),
        pending(PendingKind::WebdavOff),
    ));
    lines.push(action_line(
        "Status",
        dav_ok,
        is_sel(app, NetEntry::WebdavStatus),
        pending(PendingKind::WebdavStatus),
    ));

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        net.feedback.clone(),
        Style::default().fg(Color::LightYellow),
    )));
    lines.push(Line::from(Span::styled(
        "↑/↓ select · Enter act or edit · Esc to terminal",
        Style::default().fg(Color::DarkGray),
    )));
    lines
}

fn is_sel(app: &App, entry: NetEntry) -> bool {
    entries(app).get(app.network.selection) == Some(&entry)
}

// --- validation --------------------------------------------------------------

pub fn validate_ssid(ssid: &str) -> Result<(), String> {
    if ssid.is_empty() {
        return Err("Enter a network name.".to_string());
    }
    if ssid.chars().count() > SSID_MAX {
        return Err(format!(
            "A network name is limited to {SSID_MAX} characters."
        ));
    }
    if ssid.contains(',') || ssid.chars().any(char::is_control) {
        return Err("A network name must not contain commas or control characters.".to_string());
    }
    Ok(())
}

pub fn validate_password(password: &str) -> Result<(), String> {
    if password.chars().count() > PASSWORD_MAX {
        return Err(format!(
            "A password is limited to {PASSWORD_MAX} characters."
        ));
    }
    if password.chars().any(char::is_control) {
        return Err("A password must not contain control characters.".to_string());
    }
    Ok(())
}

pub fn validate_webdav_url(url: &str) -> Result<(), String> {
    if url.is_empty() {
        return Err("Enter an http(s) URL first.".to_string());
    }
    if url.chars().count() > WEBDAV_URL_MAX {
        return Err(format!(
            "A WebDAV URL is limited to {WEBDAV_URL_MAX} characters."
        ));
    }
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("The WebDAV URL must start with http:// or https://.".to_string());
    }
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(
            "The WebDAV URL must not contain whitespace or control characters.".to_string(),
        );
    }
    Ok(())
}

// --- commands ----------------------------------------------------------------

fn start(app: &mut App, kind: PendingKind, cmd: String, wait_final: Option<std::time::Duration>) {
    let rx = app.session.request_mgmt(cmd, wait_final);
    app.network.pending.push((kind, rx));
}

pub fn action(app: &mut App, entry: NetEntry) {
    match entry {
        NetEntry::Ssid | NetEntry::Password | NetEntry::Webdav => {}
        NetEntry::Result(index) => {
            if let Some(result) = app.network.scan.get(index) {
                let ssid = result.ssid.clone();
                app.network.ssid.set(ssid.clone());
                app.network.feedback = format!("Selected {ssid}.");
                // Move the selection back to the password field for typing.
                let items = entries(app);
                if let Some(pos) = items.iter().position(|e| *e == NetEntry::Password) {
                    app.network.selection = pos;
                }
            }
        }
        NetEntry::Scan => {
            if !app.network.can_wifi_scan(app) {
                gate(app);
                return;
            }
            app.network.scan.clear();
            app.network.scan_running = true;
            app.network.feedback = "Scanning 2.4 GHz networks…".to_string();
            start(
                app,
                PendingKind::Scan,
                "@w scan".to_string(),
                Some(SCAN_WAIT),
            );
        }
        NetEntry::WifiConnect => {
            if !app.network.can_wifi(app) {
                gate(app);
                return;
            }
            let ssid = app.network.ssid.text.clone();
            let password = app.network.password.text.clone();
            if let Err(err) = validate_ssid(&ssid).and_then(|_| validate_password(&password)) {
                app.toast(NoticeLevel::Error, err);
                return;
            }
            app.network.feedback = format!("Connecting to {ssid}…");
            start(
                app,
                PendingKind::WifiConnect,
                format!("@w={ssid},{password}"),
                Some(SCAN_WAIT),
            );
        }
        NetEntry::WifiOff => {
            if !app.network.can_wifi(app) {
                gate(app);
                return;
            }
            app.network.feedback = "Disconnecting WiFi…".to_string();
            start(
                app,
                PendingKind::WifiOff,
                "@w off".to_string(),
                Some(SCAN_WAIT),
            );
        }
        NetEntry::WifiStatus => {
            if !app.network.can_wifi(app) {
                gate(app);
                return;
            }
            start(app, PendingKind::WifiStatus, "@w?".to_string(), None);
        }
        NetEntry::WebdavSet => {
            if !app.network.can_webdav(app) {
                gate(app);
                return;
            }
            let url = app.network.webdav.text.clone();
            if let Err(err) = validate_webdav_url(&url) {
                app.toast(NoticeLevel::Error, err);
                return;
            }
            start(app, PendingKind::WebdavSet, format!("@d={url}"), None);
        }
        NetEntry::WebdavOff => {
            if !app.network.can_webdav(app) {
                gate(app);
                return;
            }
            start(app, PendingKind::WebdavOff, "@d off".to_string(), None);
        }
        NetEntry::WebdavStatus => {
            if !app.network.can_webdav(app) {
                gate(app);
                return;
            }
            start(app, PendingKind::WebdavStatus, "@d?".to_string(), None);
        }
    }
}

fn gate(app: &mut App) {
    app.toast(
        NoticeLevel::Warn,
        "Connect over BLE to manage WiFi and WebDAV.",
    );
}

/// Drain finished requests (called once per loop iteration). The oneshot
/// result is consumed exactly once and handed to [`handle_reply`].
pub fn poll(app: &mut App) {
    let pending = std::mem::take(&mut app.network.pending);
    for (kind, mut rx) in pending {
        match rx.try_recv() {
            Ok(result) => handle_reply(app, kind, Ok(result)),
            Err(oneshot::error::TryRecvError::Empty) => app.network.pending.push((kind, rx)),
            Err(err) => handle_reply(app, kind, Err(err)),
        }
    }
}

fn handle_reply(
    app: &mut App,
    kind: PendingKind,
    outcome: Result<Result<MgmtReply, String>, oneshot::error::TryRecvError>,
) {
    if kind == PendingKind::Scan {
        app.network.scan_running = false;
    }
    let reply = match outcome {
        Ok(Ok(reply)) => reply,
        Ok(Err(err)) => {
            app.toast(NoticeLevel::Error, err);
            app.network.feedback = "Request failed.".to_string();
            return;
        }
        Err(oneshot::error::TryRecvError::Closed) => {
            app.toast(NoticeLevel::Error, "Request cancelled.");
            app.network.feedback = "Request failed.".to_string();
            return;
        }
        Err(oneshot::error::TryRecvError::Empty) => return,
    };
    let mut text = String::new();
    for line in reply.lines.iter().chain(reply.events.iter()) {
        text.push_str(line);
        text.push('\n');
    }

    match kind {
        PendingKind::Scan => {
            let mut results = Vec::new();
            for line in text.lines() {
                if let Some(result) = parse_scan_line(line) {
                    results.push(result);
                }
                if line.trim() == "@scan error" {
                    app.network.feedback = "Scan failed.".to_string();
                }
            }
            app.network.scan = results;
            app.network.feedback = format!("Found {} networks.", app.network.scan.len());
        }
        PendingKind::WifiConnect | PendingKind::WifiOff => {
            if let Some(status) = parse_wifi_reply(&text) {
                app.network.wifi_status = Some(status.clone());
                app.network.feedback = format!("WiFi: {}", status.state);
            } else {
                app.network.feedback = "WiFi request sent.".to_string();
            }
        }
        PendingKind::WifiStatus => match parse_wifi_reply(&text) {
            Some(status) => {
                app.network.feedback = format!(
                    "WiFi: {}{}",
                    status.state,
                    if status.ip.is_empty() {
                        String::new()
                    } else {
                        format!(" · {}", status.ip)
                    }
                );
                app.network.wifi_status = Some(status);
            }
            None => {
                app.toast(
                    NoticeLevel::Warn,
                    super::replies::redact_secrets(text.trim()),
                );
            }
        },
        PendingKind::WebdavSet | PendingKind::WebdavOff | PendingKind::WebdavStatus => {
            match parse_webdav_reply(&text) {
                Some(status) => {
                    app.network.feedback = format!("WebDAV: {}", status.state);
                    app.network.webdav_status = Some(status);
                }
                None => {
                    app.toast(
                        NoticeLevel::Warn,
                        super::replies::redact_secrets(text.trim()),
                    );
                }
            }
        }
    }
}

// --- keys --------------------------------------------------------------------

pub fn handle_key(app: &mut App, key: KeyEvent) {
    let items = entries(app);
    let len = items.len();
    match key.code {
        KeyCode::Up => app.network.selection = app.network.selection.saturating_sub(1),
        KeyCode::Down => {
            app.network.selection = (app.network.selection + 1).min(len.saturating_sub(1))
        }
        KeyCode::Tab => {
            app.network.selection = (app.network.selection + 1).min(len.saturating_sub(1))
        }
        KeyCode::BackTab => app.network.selection = app.network.selection.saturating_sub(1),
        KeyCode::Home => app.network.selection = 0,
        KeyCode::End => app.network.selection = len.saturating_sub(1),
        KeyCode::Esc => app.focus = Focus::Center,
        KeyCode::Enter => {
            if let Some(entry) = items.get(app.network.selection).copied() {
                match entry {
                    NetEntry::Ssid | NetEntry::Password | NetEntry::Webdav => {
                        // Enter in a field advances to the next row.
                        app.network.selection = (app.network.selection + 1).min(len - 1);
                    }
                    other => action(app, other),
                }
            }
        }
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            let Some(entry) = items.get(app.network.selection).copied() else {
                return;
            };
            match entry {
                NetEntry::Ssid => app.network.ssid.insert_char(c),
                NetEntry::Password => app.network.password.insert_char(c),
                NetEntry::Webdav => app.network.webdav.insert_char(c),
                // Quick toggles when no field is selected.
                NetEntry::Scan if c == 's' => action(app, NetEntry::Scan),
                NetEntry::WifiStatus if c == 'q' => action(app, NetEntry::WifiStatus),
                _ => {}
            }
        }
        KeyCode::Backspace => {
            let Some(entry) = items.get(app.network.selection).copied() else {
                return;
            };
            match entry {
                NetEntry::Ssid => app.network.ssid.backspace(),
                NetEntry::Password => app.network.password.backspace(),
                NetEntry::Webdav => app.network.webdav.backspace(),
                _ => {}
            }
        }
        KeyCode::Left => {
            let Some(entry) = items.get(app.network.selection).copied() else {
                return;
            };
            match entry {
                NetEntry::Ssid => app.network.ssid.left(),
                NetEntry::Password => app.network.password.left(),
                NetEntry::Webdav => app.network.webdav.left(),
                _ => {}
            }
        }
        KeyCode::Right => {
            let Some(entry) = items.get(app.network.selection).copied() else {
                return;
            };
            match entry {
                NetEntry::Ssid => app.network.ssid.right(),
                NetEntry::Password => app.network.password.right(),
                NetEntry::Webdav => app.network.webdav.right(),
                _ => {}
            }
        }
        KeyCode::Char('v') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            // Reveal / mask the password (`Ctrl+P` belongs to the palette).
            app.network.show_password = !app.network.show_password;
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accessory_limits_match_the_web_client() {
        assert_eq!(SSID_MAX, 32);
        assert_eq!(PASSWORD_MAX, 64);
        assert_eq!(WEBDAV_URL_MAX, 256);
    }

    #[test]
    fn ssid_rules() {
        assert!(validate_ssid("MyNet").is_ok());
        assert!(validate_ssid("").is_err());
        assert!(validate_ssid(&"x".repeat(33)).is_err());
        assert!(validate_ssid("a,b").is_err());
        assert!(validate_ssid("bad\tname").is_err());
        assert_eq!(
            validate_ssid(&"x".repeat(33)).unwrap_err(),
            "A network name is limited to 32 characters."
        );
    }

    #[test]
    fn password_and_url_rules() {
        assert!(validate_password("").is_ok());
        assert!(validate_password("secret").is_ok());
        assert!(validate_password(&"x".repeat(65)).is_err());
        assert!(validate_password("bad\npass").is_err());
        assert!(validate_webdav_url("http://host/dav/").is_ok());
        assert!(validate_webdav_url("https://host/d").is_ok());
        assert_eq!(
            validate_webdav_url("ftp://host").unwrap_err(),
            "The WebDAV URL must start with http:// or https://."
        );
        assert!(validate_webdav_url("").is_err());
        assert!(validate_webdav_url("http://h/o p").is_err());
        assert!(validate_webdav_url(&format!("http://h/{}", "x".repeat(300))).is_err());
    }

    #[test]
    fn network_state_defaults_need_no_tty() {
        let state = NetworkState::new();
        assert!(state.scan.is_empty());
        assert!(!state.scan_running);
        assert!(state.feedback.contains("Scan to select"));
        assert_eq!(state.wifi_status, None);
        assert_eq!(NetEntry::Result(3), NetEntry::Result(3));
    }
}
