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
use crate::text;
use crate::theme::Theme;

/// Everything the network view knows. Filled by the session layer; read here
/// as plain data.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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
}

impl NetworkState {
    /// Whether the WiFi section shows a live connection.
    pub fn wifi_connected(&self) -> bool {
        self.wifi.as_ref().is_some_and(|w| w.state == "connected")
    }
}

/// The actions of the WiFi block, in the design's order.
const WIFI_ACTIONS: [&str; 4] = ["scan", "connect", "off", "status"];

/// The actions of the WebDAV block, in the design's order.
const WEBDAV_ACTIONS: [&str; 3] = ["set", "disable", "status"];

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
fn action_row(action: &str, ble_only: &str, width: u16, theme: &Theme) -> Line<'static> {
    let row = format!("  {action}{ble_only}");
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
        !connected,
        width,
        theme,
    ));
    if !connected {
        lines.push(field_row(
            &format!("{}{}", s.net_password_label, s.label_separator),
            s.net_empty_value,
            false,
            width,
            theme,
        ));
    }
    for (action, label) in [
        (WIFI_ACTIONS[0], s.net_scan),
        (WIFI_ACTIONS[1], s.net_connect),
        (WIFI_ACTIONS[2], s.net_wifi_off),
        (WIFI_ACTIONS[3], s.net_status),
    ] {
        let _ = action;
        lines.push(action_row(label, s.net_ble_only, width, theme));
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
        false,
        width,
        theme,
    ));
    for label in WEBDAV_ACTIONS {
        let word = match label {
            "set" => s.net_set,
            "disable" => s.net_disable,
            _ => s.net_status,
        };
        lines.push(action_row(word, s.net_ble_only, width, theme));
    }
    if let Some(dav) = &state.webdav {
        let state_word = if dav.state == "off" {
            s.net_state_off
        } else {
            &dav.state
        };
        lines.push(note(&format!("WebDAV: {state_word}"), width, theme));
    }

    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Transport;
    use crate::event::ConnectionState;
    use crate::i18n::Lang;

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
