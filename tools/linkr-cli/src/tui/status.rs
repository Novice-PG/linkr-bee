//! Top status bar and bottom status line formatting.
//!
//! Pure functions so the exact strings can be unit-tested without a TTY. The
//! web counterparts are the topbar (`.status-dot`, `#statusText`,
//! `#deviceName`) and the terminal `.statusbar` (`#rxCount`, `#txCount`,
//! `#baudLabel`, `#connStateText`).

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::i18n::{
    strings, t, Lang, CONNECTED, CONNECTING, DISCONNECTED, MODE_AUTO, MODE_FULL_AUTO, MODE_MANUAL,
};
use crate::agent::ExecMode;
use crate::event::ConnectionState;
use crate::transport::TransportKind;

strings! {
    ST_FAILED => "Failed", "连接失败";
    ST_HINTS => "Ctrl+P palette · F1 help · F2-F6 views · Ctrl+Q quit",
        "Ctrl+P 面板 · F1 帮助 · F2-F6 视图 · Ctrl+Q 退出";
}

/// Everything the status bar renders, already detached from the session so the
/// formatter stays testable.
#[derive(Debug, Clone)]
pub struct StatusModel {
    pub state: ConnectionState,
    pub transport: Option<TransportKind>,
    pub label: String,
    pub device_id: Option<String>,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub baud: u64,
    pub mode: ExecMode,
    /// `%H:%M:%S` local time, formatted by the caller.
    pub clock: String,
    /// Last connection detail (right side of the bottom line).
    pub detail: String,
    /// `linkr-lang` for this frame.
    pub lang: Lang,
}

impl Default for StatusModel {
    fn default() -> Self {
        Self {
            state: ConnectionState::Disconnected,
            transport: None,
            label: String::new(),
            device_id: None,
            rx_bytes: 0,
            tx_bytes: 0,
            baud: 0,
            mode: ExecMode::Auto,
            clock: "00:00:00".to_string(),
            detail: String::new(),
            lang: Lang::En,
        }
    }
}

/// Group separators exactly like the web client's `toLocaleString("en-US")`.
pub fn format_count(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Status dot glyph, same colors the web `.status-dot` uses.
pub fn state_dot(state: ConnectionState) -> char {
    match state {
        ConnectionState::Connected => '●',
        ConnectionState::Connecting => '◐',
        ConnectionState::Failed => '◆',
        ConnectionState::Disconnected => '○',
    }
}

pub fn state_dot_style(state: ConnectionState) -> Style {
    match state {
        ConnectionState::Connected => Style::default().fg(Color::Green),
        ConnectionState::Connecting => Style::default().fg(Color::Yellow),
        ConnectionState::Failed => Style::default().fg(Color::Red),
        ConnectionState::Disconnected => Style::default().fg(Color::DarkGray),
    }
}

pub fn state_text(state: ConnectionState, lang: Lang) -> &'static str {
    match state {
        ConnectionState::Connected => t(CONNECTED, lang),
        ConnectionState::Connecting => t(CONNECTING, lang),
        ConnectionState::Failed => t(ST_FAILED, lang),
        ConnectionState::Disconnected => t(DISCONNECTED, lang),
    }
}

/// Transport names are identifiers (the web keeps them verbatim), so only the
/// "no transport" placeholder differs.
pub fn transport_text(kind: Option<TransportKind>, _lang: Lang) -> &'static str {
    match kind {
        Some(TransportKind::Ble) => "BLE",
        Some(TransportKind::Lan) => "LAN",
        None => "—",
    }
}

pub fn mode_text(mode: ExecMode, lang: Lang) -> &'static str {
    match mode {
        ExecMode::Manual => t(MODE_MANUAL, lang),
        ExecMode::Auto => t(MODE_AUTO, lang),
        ExecMode::FullAuto => t(MODE_FULL_AUTO, lang),
    }
}

fn device_text(label: &str, id: Option<&str>) -> String {
    let label = label.trim();
    match id {
        Some(id) if !id.is_empty() => {
            let short: String = id.chars().take(8).collect();
            if label.is_empty() {
                short
            } else if label == id {
                format!("{label} ({short})")
            } else {
                format!("{label} #{short}")
            }
        }
        _ if label.is_empty() => "—".to_string(),
        _ => label.to_string(),
    }
}

/// Left cluster: dot, state, transport, device label/id.
/// Right cluster: counters, baud, execution mode, clock.
pub fn status_right(m: &StatusModel) -> String {
    let baud = if m.baud > 0 {
        m.baud.to_string()
    } else {
        "–".to_string()
    };
    format!(
        "RX {} · TX {} · {} · {} · {}",
        format_count(m.rx_bytes),
        format_count(m.tx_bytes),
        baud,
        mode_text(m.mode, m.lang),
        m.clock,
    )
}

/// Left cluster styled as spans (dot colored by connection state).
pub fn status_left_spans(m: &StatusModel) -> Vec<Span<'static>> {
    vec![
        Span::styled(
            state_dot(m.state).to_string(),
            state_dot_style(m.state).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" {}", state_text(m.state, m.lang)),
            match m.state {
                ConnectionState::Connected => Style::default().fg(Color::Green),
                ConnectionState::Connecting => Style::default().fg(Color::Yellow),
                ConnectionState::Failed => Style::default().fg(Color::Red),
                ConnectionState::Disconnected => Style::default().fg(Color::Gray),
            },
        ),
        Span::styled(
            format!(" · {} · ", transport_text(m.transport, m.lang)),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            device_text(&m.label, m.device_id.as_deref()),
            Style::default().fg(Color::Cyan),
        ),
    ]
}

pub fn status_right_spans(m: &StatusModel) -> Vec<Span<'static>> {
    vec![Span::styled(
        status_right(m),
        Style::default().fg(Color::Gray),
    )]
}

/// The rendered width of `s` in console columns: what the terminal counts, and
/// what decides whether a bar fits. A CJK glyph is two columns wide, so
/// counting characters instead lets a Chinese bar overflow the screen and lose
/// the tail of its right cluster.
fn cols(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// As much of `text` as fits in `width` columns, with nothing added. A wide
/// glyph is never split: the column it cannot have is left empty, so the
/// result never exceeds `width`.
fn cut_columns(text: &str, width: usize) -> String {
    let mut out = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let w = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + w > width {
            break;
        }
        out.push(ch);
        used += w;
    }
    out
}

/// [`cut_columns`] with an ellipsis that lives inside the budget, so a cut
/// stays at most `width` columns wide.
fn clip(text: &str, width: usize) -> String {
    if cols(text) <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut out = cut_columns(text, width - 1);
    out.push('…');
    out
}

/// Bottom status line: focus + view on the left, key hints on the right.
///
/// The status cluster wins: when the hints do not fit they are truncated with
/// an ellipsis instead of the focus/view/detail text.
pub fn bottom_line(focus: &str, view: &str, detail: &str, width: u16, lang: Lang) -> String {
    let left = format!(
        "[{focus}] {view}{}",
        if detail.is_empty() {
            String::new()
        } else {
            format!(" · {detail}")
        }
    );
    let right = t(ST_HINTS, lang);
    let width = width as usize;
    if width == 0 {
        return String::new();
    }
    let left_len = cols(&left);
    if left_len >= width {
        return clip(&left, width);
    }
    let room = width - left_len;
    let right_len = cols(right);
    let (tail, gap) = if right_len < room {
        (right.to_string(), room - right_len)
    } else if room >= 2 {
        let cut = clip(right, room - 1);
        let gap = room - cols(&cut);
        (cut, gap)
    } else {
        (String::new(), room)
    };
    format!("{}{}{}", left, " ".repeat(gap), tail)
}

/// Render the full top bar as one styled line, never wider than `width`.
pub fn status_line(model: &StatusModel, width: u16) -> Line<'static> {
    let width = width as usize;
    if width == 0 {
        return Line::from("");
    }
    // Measure the spans, not a second copy of them: this ran once per frame
    // and used to format both clusters as strings *and* as spans before
    // throwing the strings away (except in the cut branch, where the left
    // cluster is flattened exactly once).
    let right_spans = status_right_spans(model);
    let right_len = spans_cols(&right_spans);
    if right_len >= width {
        // Not even the right cluster fits: show it truncated, alone.
        return Line::from(truncate_spans(right_spans, width));
    }
    let room = width - right_len;
    let left_spans = status_left_spans(model);
    let left_len = spans_cols(&left_spans);
    let mut spans = if left_len < room {
        let gap = room - left_len;
        let mut spans = left_spans;
        if gap > 0 {
            spans.push(Span::styled(" ".repeat(gap), Style::default()));
        }
        spans
    } else {
        // Ellipsis truncation: the cut keeps one column for the gap.
        let cut = clip(&spans_text(&left_spans), room.saturating_sub(1));
        let gap = room - cols(&cut);
        let mut spans = vec![Span::styled(cut, state_dot_style(model.state))];
        if gap > 0 {
            spans.push(Span::styled(" ".repeat(gap), Style::default()));
        }
        spans
    };
    spans.extend(right_spans);
    Line::from(spans)
}

/// Display columns a span list occupies — what the bar is measured in.
fn spans_cols(spans: &[Span<'static>]) -> usize {
    spans.iter().map(|span| cols(&span.content)).sum()
}

/// What a span list reads as: the text the row shows, one string.
fn spans_text(spans: &[Span<'static>]) -> String {
    spans.iter().map(|span| span.content.as_ref()).collect()
}

/// Cut a span list down to `width` columns (used when the bar is narrow).
fn truncate_spans(spans: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    let mut out = Vec::with_capacity(spans.len());
    let mut budget = width;
    for span in spans {
        if budget == 0 {
            break;
        }
        let len = cols(&span.content);
        if len <= budget {
            out.push(span);
            budget -= len;
        } else {
            let text = cut_columns(&span.content, budget);
            out.push(Span::styled(text, span.style));
            budget = 0;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> StatusModel {
        StatusModel {
            state: ConnectionState::Connected,
            transport: Some(TransportKind::Ble),
            label: "Linkr BLE UART".to_string(),
            device_id: Some("aabbccddeeff0011".to_string()),
            rx_bytes: 1_234_567,
            tx_bytes: 890,
            baud: 115200,
            mode: ExecMode::Auto,
            clock: "13:04:11".to_string(),
            detail: "API v1.0 device=aabbccddeeff0011".to_string(),
            lang: Lang::En,
        }
    }

    #[test]
    fn counts_use_en_us_grouping() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(1000), "1,000");
        assert_eq!(format_count(1_234_567), "1,234,567");
    }

    #[test]
    fn left_cluster_is_exact() {
        assert_eq!(
            spans_text(&status_left_spans(&model())),
            "● Connected · BLE · Linkr BLE UART #aabbccdd"
        );
    }

    #[test]
    fn right_cluster_is_exact() {
        assert_eq!(
            status_right(&model()),
            "RX 1,234,567 · TX 890 · 115200 · Auto · 13:04:11"
        );
    }

    #[test]
    fn disconnected_and_zero_baud_render_placeholders() {
        let m = StatusModel::default();
        assert_eq!(spans_text(&status_left_spans(&m)), "○ Disconnected · — · —");
        assert_eq!(status_right(&m), "RX 0 · TX 0 · – · Auto · 00:00:00");
        assert_eq!(state_dot(ConnectionState::Connecting), '◐');
        assert_eq!(state_dot(ConnectionState::Failed), '◆');
    }

    #[test]
    fn label_that_is_the_id_is_not_duplicated() {
        let mut m = model();
        m.label = "aabbccddeeff0011".to_string();
        assert_eq!(
            spans_text(&status_left_spans(&m)),
            "● Connected · BLE · aabbccddeeff0011 (aabbccdd)"
        );
    }

    #[test]
    fn mode_labels_match_the_web_picker() {
        assert_eq!(mode_text(ExecMode::Manual, Lang::En), "Manual");
        assert_eq!(mode_text(ExecMode::Auto, Lang::En), "Auto");
        assert_eq!(mode_text(ExecMode::FullAuto, Lang::En), "Full Auto");
    }

    #[test]
    fn bottom_line_contains_the_documented_hints() {
        let line = bottom_line("Terminal", "Serial Terminal", "detail", 100, Lang::En);
        assert!(line.starts_with("[Terminal] Serial Terminal · detail"));
        assert!(line.contains("Ctrl+P palette"));
        assert!(line.contains("F1 help"));
        assert!(line.contains("Ctrl+Q quit"));
        assert_eq!(line.chars().count(), 100);

        // At 80 columns the hint cluster is truncated; the status wins.
        let line = bottom_line("Terminal", "Serial Terminal", "detail", 80, Lang::En);
        assert!(line.starts_with("[Terminal] Serial Terminal · detail"));
        assert!(line.contains("Ctrl+P palette"));
        assert!(line.contains('…'));
        assert_eq!(line.chars().count(), 80);

        // A tiny bar keeps the status and stays inside the width.
        let line = bottom_line("Terminal", "Serial Terminal", "detail", 20, Lang::En);
        assert_eq!(line.chars().count(), 20);
        assert!(line.starts_with("[Terminal] Serial"));
    }

    /// Both languages of every status message carry text and differ.
    #[test]
    fn every_status_message_is_translated() {
        super::super::i18n::assert_bilingual(ALL);
        assert!(ALL.len() >= 2, "the status bar adds at least two messages");
    }

    /// Chinese must reach the rendered bar, not just the table.
    #[test]
    fn the_bottom_line_follows_the_language() {
        let zh = bottom_line("终端", "串口终端", "详情", 80, Lang::Zh);
        assert!(zh.contains("Ctrl+P 面板"), "{zh}");
        assert!(zh.contains("Ctrl+Q 退出"), "{zh}");
        assert!(!zh.contains("Ctrl+P palette"), "{zh}");
        assert_eq!(state_text(ConnectionState::Failed, Lang::Zh), "连接失败");
        // The bar shows the panel's caption, and the web panel prints `Auto`
        // and `Full Auto` untranslated in both languages.
        assert_eq!(mode_text(ExecMode::Auto, Lang::Zh), "Auto");
        assert_eq!(mode_text(ExecMode::FullAuto, Lang::Zh), "Full Auto");
        assert_eq!(mode_text(ExecMode::Manual, Lang::Zh), "手动");
    }

    #[test]
    fn status_line_keeps_the_bar_within_width() {
        let m = model();
        for width in [10u16, 40, 80, 200] {
            let line = status_line(&m, width);
            let len: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
            assert!(len as u16 <= width, "width {width}: rendered {len}");
        }
    }

    /// A CJK glyph is two columns wide. Measuring the bar in characters made
    /// the Chinese UI render past the right edge of the console, where the
    /// terminal drops the tail of the RX/TX cluster.
    #[test]
    fn the_bar_is_measured_in_columns_not_characters() {
        let mut m = model();
        m.lang = Lang::Zh;
        for width in [10u16, 40, 80, 200] {
            let line = status_line(&m, width);
            let spanned: usize = line.spans.iter().map(|s| cols(s.content.as_ref())).sum();
            assert!(
                spanned as u16 <= width,
                "width {width}: the Chinese bar renders {spanned} columns"
            );
        }
        for width in [20u16, 80] {
            let zh = bottom_line("终端", "串口终端", "详情", width, Lang::Zh);
            assert!(
                cols(&zh) as u16 <= width,
                "width {width}: the Chinese bottom line renders {} columns: {zh}",
                cols(&zh)
            );
        }
        // A two-column glyph is never split across the cut: the cut stays
        // inside its budget rather than spilling one column over it.
        assert_eq!(cols(&clip("中文测试", 5)), 5);
        assert!(clip("中文测试", 5).chars().count() <= 5);
    }
}
