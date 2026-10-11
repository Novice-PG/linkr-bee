//! Diagnostics view: `@i?` collection and the same grid the web client
//! renders from `@info` lines (WEB_UX_SPEC section 5.2).

use std::collections::BTreeMap;

use unicode_width::UnicodeWidthStr;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use tokio::sync::oneshot;

use super::i18n::{strings, t, tr, Lang};
use super::state::App;
use crate::protocol::MgmtReply;
use crate::session::SessionHandle;

strings! {
    DIAG_HEADER => "Diagnostics", "诊断";
    DIAG_UPDATED => "@i? · updated {}", "@i? · 已更新 {}";
    DIAG_NOT_READ => "@i? not read yet", "@i? 尚未读取";
    DIAG_NO_BLE => "Connect over BLE to read diagnostics (management commands are BLE-only).",
        "通过 BLE 连接后才能读取诊断（管理命令仅限 BLE）。";
    DIAG_LOADING => "reading @i?…", "正在读取 @i?…";
    DIAG_EMPTY => "No device information yet.", "暂无设备信息。";
    DIAG_KEYS_HINT => "r refresh · PgUp/PgDn scroll · F2 terminal",
        "r 刷新 · PgUp/PgDn 滚动 · F2 终端";
    DIAG_POLL_CLOSED => "disconnected before diagnostics arrived",
        "诊断数据到达前连接已断开";
    DIAG_LABEL_FIRMWARE => "Firmware", "固件";
    DIAG_LABEL_UPTIME => "Uptime", "运行时间";
    DIAG_LABEL_BLE_ACCESS => "BLE access", "BLE 访问";
    DIAG_LABEL_UART_BUFFER => "UART Buffer", "UART 缓冲";
    DIAG_LABEL_UPLOAD_QUEUE => "Upload Queue", "上传队列";
    DIAG_ACCESS_OPEN => "open", "开放";
    DIAG_ACCESS_SCOPED => "scoped", "受限";
    DIAG_LINK_LEVEL => "link L{}", "链路 L{}";
}

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

/// `open`/`scoped` + ` · link L<level>` (the web words both through its own
/// i18n table, so these two are interface text, not wire tokens).
pub fn fmt_ble_access(groups: &InfoGroups, lang: Lang) -> String {
    let owner = match get(groups, "sys", "owner") {
        Some("0") => t(DIAG_ACCESS_OPEN, lang),
        Some(_) => t(DIAG_ACCESS_SCOPED, lang),
        None => return "–".to_string(),
    };
    match get(groups, "sys", "security") {
        Some(level) => format!("{owner} · {}", tr!(t(DIAG_LINK_LEVEL, lang), level)),
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
pub fn value_rows(groups: &InfoGroups, lang: Lang) -> [(&'static str, String); 6] {
    [
        (t(DIAG_LABEL_FIRMWARE, lang), fmt_firmware(groups)),
        (t(DIAG_LABEL_UPTIME, lang), fmt_uptime(groups)),
        (t(DIAG_LABEL_BLE_ACCESS, lang), fmt_ble_access(groups, lang)),
        (t(DIAG_LABEL_UART_BUFFER, lang), fmt_uart_buffer(groups)),
        // "WiFi" reads the same in both languages (web `diagWifi`), so it is
        // not a bilingual entry — a copied translation would fail the check.
        ("WiFi", fmt_wifi(groups)),
        (t(DIAG_LABEL_UPLOAD_QUEUE, lang), fmt_upload(groups)),
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

    /// Drain a completed request without blocking the render loop. `lang` only
    /// words the local "never arrived" failure; device errors pass through.
    pub fn poll(&mut self, lang: Lang) {
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
                self.error = Some(t(DIAG_POLL_CLOSED, lang).to_string());
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
    // Padded to `label_width` *columns*, not characters: a CJK label is two
    // columns per glyph, so the old `{:<14}` (which counts characters) left
    // the `│ ` separator four columns right of the English rows and pushed
    // `value` past the pane.
    let label = pad_columns(label, label_width);
    // 2 columns for the `│ ` separator.
    let value_width = (width as usize).saturating_sub(label_width + 2);
    let value = super::dialogs::clip_columns(value, value_width);
    Line::from(vec![
        Span::styled(label, Style::default().fg(Color::Cyan)),
        Span::styled("│ ", Style::default().fg(Color::DarkGray)),
        Span::styled(value, Style::default().fg(Color::White)),
    ])
}

/// Left-align `text` in a `width`-column field, measured in terminal columns
/// (a CJK glyph is two). Text already at or past `width` is left untouched —
/// same as `{:<width$}` did for characters, only now the unit is columns.
fn pad_columns(text: &str, width: usize) -> String {
    let used = UnicodeWidthStr::width(text);
    let mut out = String::with_capacity(text.len() + width.saturating_sub(used));
    out.push_str(text);
    for _ in used..width {
        out.push(' ');
    }
    out
}

/// Header + the six-row grid + status, mirroring WEB_UX_SPEC section 5.2.
pub fn render_lines(app: &App, width: u16) -> Vec<Line<'static>> {
    let lang = app.lang();
    let state = &app.diagnostics;
    let mut lines = vec![
        Line::from(Span::styled(
            t(DIAG_HEADER, lang),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            match &state.updated {
                Some(at) => tr!(t(DIAG_UPDATED, lang), at),
                None => t(DIAG_NOT_READ, lang).to_string(),
            },
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(""),
    ];

    if !app.ble_connected() {
        lines.push(Line::from(Span::styled(
            t(DIAG_NO_BLE, lang),
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
        for (label, value) in value_rows(&state.groups, lang) {
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
            t(DIAG_LOADING, lang),
            Style::default().fg(Color::Gray),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            t(DIAG_EMPTY, lang),
            Style::default().fg(Color::DarkGray),
        )));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        t(DIAG_KEYS_HINT, lang),
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
        assert_eq!(fmt_ble_access(&groups, Lang::En), "open · link L1");
        assert_eq!(fmt_uart_buffer(&groups), "0/16384 · drop 0");
        assert_eq!(fmt_wifi(&groups), "connected · IP ready · err 0");
        assert_eq!(fmt_upload(&groups), "0 B · HTTP 201 · fail 0");
        let rows = value_rows(&groups, Lang::En);
        assert_eq!(rows[0].0, "Firmware");
        assert_eq!(rows[5].0, "Upload Queue");

        // The Chinese grid labels reach the render; the values stay the
        // byte-for-byte web formatters.
        let zh = value_rows(&groups, Lang::Zh);
        assert_eq!(zh[0].0, "固件");
        assert_eq!(zh[1].0, "运行时间");
        assert_eq!(zh[2].0, "BLE 访问");
        assert_eq!(zh[2].1, "开放 · 链路 L1");
        assert_eq!(zh[5].0, "上传队列");
    }

    /// The grid's label field is 14 *columns*. A CJK label is two columns per
    /// glyph, so measuring characters ("运行时间" counts 4) padded it to 18
    /// columns and shoved `│ ` four columns right of the English rows.
    #[test]
    fn the_label_field_is_measured_in_columns_not_characters() {
        for label in ["Firmware", "运行时间", "BLE 访问", "上传队列"] {
            let line = row_line(label, "value", 40);
            let width = unicode_width::UnicodeWidthStr::width(line.spans[0].content.as_ref());
            assert_eq!(width, 14, "label {label:?} occupies {width} columns");
        }
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
        assert_eq!(fmt_ble_access(&groups, Lang::En), "–");
    }

    #[test]
    fn ble_access_scoped_when_owner_is_not_open() {
        let (groups, _) = parse_info_lines(["@info sys uptime_ms=1 owner=1 security=2"]);
        assert_eq!(fmt_ble_access(&groups, Lang::En), "scoped · link L2");
        assert_eq!(fmt_ble_access(&groups, Lang::Zh), "受限 · 链路 L2");
        let (groups, _) = parse_info_lines(["@info sys uptime_ms=1 owner=1"]);
        assert_eq!(fmt_ble_access(&groups, Lang::En), "scoped");
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
        state.poll(Lang::En);
        assert!(state.error.is_none());
    }

    #[test]
    fn every_diagnostics_message_is_translated() {
        super::super::i18n::assert_bilingual(ALL);
        assert!(ALL.len() >= 16, "the diagnostics view carries 16 messages");
    }
}
