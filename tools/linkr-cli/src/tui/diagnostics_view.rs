//! Diagnostics view: `@i?` collection and the same grid the web client
//! renders from `@info` lines (WEB_UX_SPEC section 5.2).

use std::collections::BTreeMap;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use tokio::sync::oneshot;

use super::state::App;
use crate::protocol::MgmtReply;
use crate::session::SessionHandle;

/// `state.diagnostics[group][key] = value`.
pub type InfoGroups = BTreeMap<String, BTreeMap<String, String>>;

/// Parse one `@info <group> key=value …` line. Returns `true` when the line
/// belonged to the protocol (the caller tracks `@info done` separately via
/// [`parse_info_lines`]).
pub fn parse_info_line(line: &str, groups: &mut InfoGroups) -> bool {
    let rest = match line.trim_end().strip_prefix("@info ") {
        Some(rest) => rest,
        None => return false,
    };
    let mut fields = rest.split_whitespace();
    let group = match fields.next() {
        Some(group) if group != "done" => group.to_string(),
        _ => return true, // `@info done` carries no data
    };
    let entry = groups.entry(group).or_default();
    for field in fields {
        if let Some((key, value)) = field.split_once('=') {
            entry.insert(key.to_string(), value.to_string());
        }
    }
    true
}

/// Parse a whole reply: merged groups plus whether `@info done` arrived.
pub fn parse_info_lines<'a, I>(lines: I) -> (InfoGroups, bool)
where
    I: IntoIterator<Item = &'a str>,
{
    let mut groups = InfoGroups::new();
    let mut done = false;
    for line in lines {
        if line.trim_end() == "@info done" {
            done = true;
            continue;
        }
        parse_info_line(line, &mut groups);
    }
    (groups, done)
}

fn get<'a>(groups: &'a InfoGroups, group: &str, key: &str) -> Option<&'a str> {
    groups
        .get(group)
        .and_then(|g| g.get(key))
        .map(String::as_str)
}

/// `<version> · Z<zephyr>` or `—`.
pub fn fmt_firmware(groups: &InfoGroups) -> String {
    match get(groups, "fw", "version") {
        Some(version) => match get(groups, "fw", "zephyr") {
            Some(zephyr) => format!("{version} · Z{zephyr}"),
            None => version.to_string(),
        },
        None => "—".to_string(),
    }
}

/// `1d 2h` | `2h 30m` | `12m 5s` | `5s` — the web uptime formatter.
pub fn fmt_uptime_ms(uptime_ms: u64) -> String {
    let total_secs = uptime_ms / 1000;
    let days = total_secs / 86_400;
    let hours = (total_secs % 86_400) / 3_600;
    let minutes = (total_secs % 3_600) / 60;
    let secs = total_secs % 60;
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m {secs}s")
    } else {
        format!("{secs}s")
    }
}

pub fn fmt_uptime(groups: &InfoGroups) -> String {
    match get(groups, "sys", "uptime_ms").and_then(|v| v.parse::<u64>().ok()) {
        Some(ms) => fmt_uptime_ms(ms),
        None => "–".to_string(),
    }
}

/// `open`/`scoped` + ` · link L<level>`.
pub fn fmt_ble_access(groups: &InfoGroups) -> String {
    let owner = match get(groups, "sys", "owner") {
        Some("0") => "open",
        Some(_) => "scoped",
        None => return "–".to_string(),
    };
    match get(groups, "sys", "security") {
        Some(level) => format!("{owner} · link L{level}"),
        None => owner.to_string(),
    }
}

/// `<buffer> · drop <n>`.
pub fn fmt_uart_buffer(groups: &InfoGroups) -> String {
    let buffer = get(groups, "uart", "buffer").unwrap_or("–");
    let dropped = get(groups, "uart", "dropped").unwrap_or("0");
    format!("{buffer} · drop {dropped}")
}

/// `<state> · IP <ip> · err <n>`.
pub fn fmt_wifi(groups: &InfoGroups) -> String {
    let state = get(groups, "wifi", "state").unwrap_or("–");
    let ip = get(groups, "wifi", "ip").unwrap_or("-");
    let err = get(groups, "wifi", "error").unwrap_or("0");
    format!("{state} · IP {ip} · err {err}")
}

/// `<queue> B · HTTP <n> · fail <n>`.
pub fn fmt_upload(groups: &InfoGroups) -> String {
    let queue = get(groups, "upload", "queue").unwrap_or("0");
    let http = get(groups, "upload", "http").unwrap_or("-");
    let failures = get(groups, "upload", "failures").unwrap_or("0");
    format!("{queue} B · HTTP {http} · fail {failures}")
}

/// The six rows of the web `#diagnosticsGrid`, in order.
pub fn value_rows(groups: &InfoGroups) -> [(&'static str, String); 6] {
    [
        ("Firmware", fmt_firmware(groups)),
        ("Uptime", fmt_uptime(groups)),
        ("BLE access", fmt_ble_access(groups)),
        ("UART Buffer", fmt_uart_buffer(groups)),
        ("WiFi", fmt_wifi(groups)),
        ("Upload Queue", fmt_upload(groups)),
    ]
}

/// `wifi.state` side effect: the web client updates its WiFi summary from the
/// diagnostics stream. Returns `(connected, ip)` when present.
pub fn wifi_from_info(groups: &InfoGroups) -> Option<(bool, String)> {
    let state = get(groups, "wifi", "state")?;
    let ip = get(groups, "wifi", "ip").unwrap_or("").to_string();
    Some((state == "connected", ip))
}

/// Heuristic used to pre-fill the LAN host field (web: `wifi.ip` looks like an
/// IPv4 address).
pub fn looks_like_ipv4(value: &str) -> bool {
    let mut parts = 0;
    for part in value.split('.') {
        if parts == 4 {
            return false;
        }
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) || part.len() > 3 {
            return false;
        }
        if part.parse::<u16>().map(|v| v > 255).unwrap_or(true) {
            return false;
        }
        parts += 1;
    }
    parts == 4
}

/// Diagnostics view state machine: request, collect, render.
#[derive(Default)]
pub struct DiagnosticsState {
    pub groups: InfoGroups,
    pub raw: Vec<String>,
    pub done: bool,
    pub loading: bool,
    pub error: Option<String>,
    /// `%H:%M:%S` of the last successful refresh.
    pub updated: Option<String>,
    pending: Option<oneshot::Receiver<Result<MgmtReply, String>>>,
}

impl DiagnosticsState {
    /// Fire `@i?` (BLE + management ready only — the caller gates).
    pub fn refresh(&mut self, session: &SessionHandle) {
        if self.pending.is_some() {
            return;
        }
        self.loading = true;
        self.error = None;
        self.pending = Some(session.request_mgmt("@i?".to_string(), None));
    }

    /// Drain a completed request without blocking the render loop.
    pub fn poll(&mut self) {
        let Some(rx) = &mut self.pending else { return };
        match rx.try_recv() {
            Ok(Ok(reply)) => {
                self.loading = false;
                self.pending = None;
                self.updated = Some(chrono::Local::now().format("%H:%M:%S").to_string());
                self.raw.clear();
                for line in reply.lines.iter().chain(reply.events.iter()) {
                    self.raw.push(line.clone());
                }
                let borrowed: Vec<&str> = self.raw.iter().map(String::as_str).collect();
                let (groups, done) = parse_info_lines(borrowed);
                if !groups.is_empty() {
                    self.groups = groups;
                }
                self.done = done;
            }
            Ok(Err(err)) => {
                self.loading = false;
                self.pending = None;
                self.error = Some(err);
            }
            Err(oneshot::error::TryRecvError::Empty) => {}
            Err(oneshot::error::TryRecvError::Closed) => {
                self.loading = false;
                self.pending = None;
                self.error = Some("disconnected before diagnostics arrived".to_string());
            }
        }
    }

    pub fn has_data(&self) -> bool {
        !self.groups.is_empty()
    }
}

// --- rendering ---------------------------------------------------------------

/// One `label ┆ value` row of the web `#diagnosticsGrid`.
fn row_line(label: &str, value: &str, width: u16) -> Line<'static> {
    let label_width = 14usize;
    let label = format!("{label:<label_width$}");
    // 2 columns for the `│ ` separator.
    let value_width = (width as usize).saturating_sub(label_width + 2);
    let value = clip(value, value_width);
    Line::from(vec![
        Span::styled(label, Style::default().fg(Color::Cyan)),
        Span::styled("│ ", Style::default().fg(Color::DarkGray)),
        Span::styled(value, Style::default().fg(Color::White)),
    ])
}

fn clip(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1);
        if used + w > width.saturating_sub(1) {
            out.push('…');
            return out;
        }
        out.push(ch);
        used += w;
    }
    out
}

/// Header + the six-row grid + status, mirroring WEB_UX_SPEC section 5.2.
pub fn render_lines(app: &App, width: u16) -> Vec<Line<'static>> {
    let state = &app.diagnostics;
    let mut lines = vec![
        Line::from(Span::styled(
            "Diagnostics",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            match &state.updated {
                Some(at) => format!("@i? · updated {at}"),
                None => "@i? not read yet".to_string(),
            },
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(""),
    ];

    if !app.ble_connected() {
        lines.push(Line::from(Span::styled(
            "Connect over BLE to read diagnostics (management commands are BLE-only).",
            Style::default().fg(Color::LightYellow),
        )));
        lines.push(Line::from(""));
    }
    if let Some(error) = &state.error {
        lines.push(Line::from(Span::styled(
            error.clone(),
            Style::default().fg(Color::LightRed),
        )));
        lines.push(Line::from(""));
    }

    if state.has_data() {
        for (label, value) in value_rows(&state.groups) {
            lines.push(row_line(label, &value, width));
        }
        // Extra `@info` groups beyond the six web rows stay visible so the
        // grid never hides data the firmware reports.
        for (group, values) in &state.groups {
            if matches!(group.as_str(), "fw" | "sys" | "uart" | "wifi" | "upload") {
                continue;
            }
            for (key, value) in values {
                lines.push(row_line(&format!("{group}.{key}"), value, width));
            }
        }
    } else if state.loading {
        lines.push(Line::from(Span::styled(
            "reading @i?…",
            Style::default().fg(Color::Gray),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            "No device information yet.",
            Style::default().fg(Color::DarkGray),
        )));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "r refresh · PgUp/PgDn scroll · F2 terminal",
        Style::default().fg(Color::DarkGray),
    )));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact sample from docs/DEVELOPMENT.md.
    const SAMPLE: &[&str] = &[
        "@info fw version=0.2.0 zephyr=4.4.1",
        "@info sys uptime_ms=123456 owner=0 security=1",
        "@info uart dropped=0 buffer=0/16384",
        "@info wifi state=connected ip=ready error=0",
        "@info upload state=on queue=0 dropped=0 http=201 failures=0 successes=4",
        "@info done",
    ];

    #[test]
    fn parses_the_documented_sample() {
        let (groups, done) = parse_info_lines(SAMPLE.iter().copied());
        assert!(done);
        assert_eq!(groups.len(), 5);
        assert_eq!(groups["fw"]["version"], "0.2.0");
        assert_eq!(groups["fw"]["zephyr"], "4.4.1");
        assert_eq!(groups["sys"]["uptime_ms"], "123456");
        assert_eq!(groups["sys"]["owner"], "0");
        assert_eq!(groups["sys"]["security"], "1");
        assert_eq!(groups["uart"]["dropped"], "0");
        assert_eq!(groups["uart"]["buffer"], "0/16384");
        assert_eq!(groups["wifi"]["state"], "connected");
        assert_eq!(groups["wifi"]["ip"], "ready");
        assert_eq!(groups["upload"]["http"], "201");
        assert_eq!(groups["upload"]["failures"], "0");
        assert_eq!(groups["upload"]["successes"], "4");
    }

    #[test]
    fn splits_each_key_at_the_first_equals() {
        let mut groups = InfoGroups::new();
        assert!(parse_info_line("@info net path=/a=b=c", &mut groups));
        assert_eq!(groups["net"]["path"], "/a=b=c");
        // Unknown shapes are ignored without corrupting the map.
        assert!(!parse_info_line("hello", &mut groups));
        assert!(!parse_info_line("@infodone", &mut groups));
        // A field without '=' is skipped.
        assert!(parse_info_line("@info net lone", &mut groups));
        assert!(groups["net"].is_empty() || !groups["net"].contains_key("lone"));
        // `@info done` is recognized but stores nothing.
        assert!(parse_info_line("@info done", &mut groups));
    }

    #[test]
    fn done_is_only_set_by_the_done_line() {
        let (_, done) = parse_info_lines(["@info fw version=1"]);
        assert!(!done);
        let (_, done) = parse_info_lines(["@info done"]);
        assert!(done);
        let (groups, done) = parse_info_lines(Vec::<&str>::new());
        assert!(groups.is_empty());
        assert!(!done);
    }

    #[test]
    fn renders_the_web_grid_values_exactly() {
        let (groups, _) = parse_info_lines(SAMPLE.iter().copied());
        assert_eq!(fmt_firmware(&groups), "0.2.0 · Z4.4.1");
        assert_eq!(fmt_uptime(&groups), "2m 3s");
        assert_eq!(fmt_ble_access(&groups), "open · link L1");
        assert_eq!(fmt_uart_buffer(&groups), "0/16384 · drop 0");
        assert_eq!(fmt_wifi(&groups), "connected · IP ready · err 0");
        assert_eq!(fmt_upload(&groups), "0 B · HTTP 201 · fail 0");
        let rows = value_rows(&groups);
        assert_eq!(rows[0].0, "Firmware");
        assert_eq!(rows[5].0, "Upload Queue");
    }

    #[test]
    fn grid_rows_are_labelled_and_clipped_to_the_width() {
        let line = row_line("Firmware", "0.2.0 · Z4.4.1", 40);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.starts_with("Firmware"), "{text}");
        assert!(text.contains("0.2.0 · Z4.4.1"), "{text}");
        assert!(text.chars().count() <= 40, "{text}");

        let long = row_line("Firmware", &"x".repeat(200), 40);
        let text: String = long.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains('…'), "{text}");
        assert!(text.chars().count() <= 40, "{}", text.chars().count());
    }

    #[test]
    fn uptime_formatting_covers_every_bracket() {
        assert_eq!(fmt_uptime_ms(5_000), "5s");
        assert_eq!(fmt_uptime_ms(75_000), "1m 15s");
        assert_eq!(fmt_uptime_ms(9_000_000), "2h 30m");
        assert_eq!(fmt_uptime_ms(90_060_000), "1d 1h");
        assert_eq!(fmt_uptime_ms(0), "0s");
        let (groups, _) = parse_info_lines(Vec::<&str>::new());
        assert_eq!(fmt_uptime(&groups), "–");
        assert_eq!(fmt_firmware(&groups), "—");
        assert_eq!(fmt_ble_access(&groups), "–");
    }

    #[test]
    fn ble_access_scoped_when_owner_is_not_open() {
        let (groups, _) = parse_info_lines(["@info sys uptime_ms=1 owner=1 security=2"]);
        assert_eq!(fmt_ble_access(&groups), "scoped · link L2");
        let (groups, _) = parse_info_lines(["@info sys uptime_ms=1 owner=1"]);
        assert_eq!(fmt_ble_access(&groups), "scoped");
    }

    #[test]
    fn wifi_side_effect_and_ipv4_heuristic() {
        let (groups, _) = parse_info_lines(SAMPLE.iter().copied());
        assert_eq!(wifi_from_info(&groups), Some((true, "ready".to_string())));
        assert!(looks_like_ipv4("192.168.1.50"));
        assert!(looks_like_ipv4("127.0.0.1"));
        assert!(!looks_like_ipv4("ready"));
        assert!(!looks_like_ipv4("192.168.1"));
        assert!(!looks_like_ipv4("192.168.1.256"));
        assert!(!looks_like_ipv4("host.local"));
        assert_eq!(wifi_from_info(&InfoGroups::new()), None);
    }

    #[test]
    fn state_starts_empty_and_needs_no_tty() {
        let state = DiagnosticsState::default();
        assert!(!state.has_data());
        assert!(!state.loading);
        assert!(state.raw.is_empty());
        assert_eq!(state.updated, None);
        // poll() without a pending request is a no-op.
        let mut state = state;
        state.poll();
        assert!(state.error.is_none());
    }
}
