//! The serial pane: the device's screen, emulated.
//!
//! This is the one view that is not a document. There is no model to lay out
//! and no fields to place: the content is whatever the device has *painted*, and
//! a device paints with escape sequences — it moves the cursor, colours the next
//! glyph, erases a line, and repaints one cell in place for a spinner. Showing
//! those bytes as text would print the sequences; showing them as lines would
//! stack a spinner's four hundred frames up the screen. So the pane emulates:
//! [`crate::vt`] holds the screen, and this module is the wiring around it.
//!
//! Two details are the whole job, and both predate the emulation because both
//! are about the *wire* rather than the screen:
//!
//! - **Decoding is streaming.** A UTF-8 character can be split across two
//!   reads — a BLE notification carries 244 bytes and a Chinese character is
//!   three of them, so the split is not a rare edge, it is Tuesday. Decoding
//!   each chunk on its own would replace the halves with `U+FFFD` and corrupt
//!   exactly the text the device sent. [`Utf8Stream`] holds the incomplete tail
//!   until its bytes arrive.
//! - **Parsing is streaming too, for the same reason.** An escape sequence can
//!   be split the same way, so the [`Parser`] is kept rather than rebuilt per
//!   chunk: a notification boundary in the middle of `ESC [ 3 1 m` must not
//!   spray `31m` across the screen.
//!
//! The pane is also where the device's few *questions* are answered: a program
//! that asks for the cursor position (`ESC [ 6 n`) blocks until it hears back,
//! so the replies the emulator generates are held here and drained by the pump.

use std::io;
use std::path::Path;

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::app::{App, Focus};
use crate::text;
use crate::theme::Theme;
use crate::vt::{Ansi, Cell, CellStyle, Parser, Vt};

/// A streaming UTF-8 decoder: text now, plus the tail of a character that has
/// not finished arriving.
#[derive(Debug, Clone, Default)]
pub struct Utf8Stream {
    /// Bytes that did not form a complete character yet.
    pending: Vec<u8>,
}

impl Utf8Stream {
    /// Feed bytes, returning the text that is complete so far. An invalid byte
    /// is replaced with `U+FFFD` — the wire is untrusted, and one bad byte must
    /// not swallow the rest of a burst.
    pub fn push(&mut self, bytes: &[u8]) -> String {
        self.pending.extend_from_slice(bytes);
        let mut out = String::new();
        loop {
            match std::str::from_utf8(&self.pending) {
                Ok(text) => {
                    out.push_str(text);
                    self.pending.clear();
                    return out;
                }
                Err(err) => {
                    let valid = err.valid_up_to();
                    if valid > 0 {
                        // `valid_up_to` guarantees this slice is UTF-8.
                        out.push_str(std::str::from_utf8(&self.pending[..valid]).expect("valid"));
                    }
                    match err.error_len() {
                        // The tail is an unfinished character: keep it.
                        None => {
                            self.pending.drain(..valid);
                            return out;
                        }
                        // Genuinely invalid bytes: report them and move on.
                        Some(len) => {
                            out.push('\u{fffd}');
                            self.pending.drain(..valid + len);
                        }
                    }
                }
            }
        }
    }

    /// Whether a partial character is still waiting for its bytes.
    pub fn is_pending(&self) -> bool {
        !self.pending.is_empty()
    }
}

/// What the terminal pane holds: the device's screen, and how it is shown.
#[derive(Debug, Clone)]
pub struct TerminalPane {
    vt: Vt,
    /// Kept across calls so a sequence split by a read boundary still parses.
    parser: Parser,
    decoder: Utf8Stream,
    /// Stick to the newest line. When off, `scroll` says how far back the user
    /// has walked.
    pub autoscroll: bool,
    /// Lines scrolled up from the bottom. Only read while `autoscroll` is off.
    pub scroll: usize,
    /// Echo the keystrokes locally, for a device that does not echo them back.
    pub echo: bool,
}

impl Default for TerminalPane {
    fn default() -> TerminalPane {
        TerminalPane::new()
    }
}

impl TerminalPane {
    /// An empty pane, scrolling, without local echo — a serial console already
    /// echoes what you type, and echoing twice is the classic double-paste bug.
    pub fn new() -> TerminalPane {
        TerminalPane {
            vt: Vt::default(),
            parser: Parser::new(),
            decoder: Utf8Stream::default(),
            autoscroll: true,
            scroll: 0,
            echo: false,
        }
    }

    /// Take a chunk of the device's output.
    pub fn push(&mut self, bytes: &[u8]) {
        let text = self.decoder.push(bytes);
        self.feed(&text);
    }

    /// Feed text without a decoder — how a sample frame is filled.
    pub fn push_text(&mut self, text: &str) {
        self.feed(text);
    }

    fn feed(&mut self, text: &str) {
        if !text.is_empty() {
            self.parser.feed_str(text, &mut self.vt);
        }
    }

    /// Tell the emulator how big the pane is.
    ///
    /// Called from the event loop, never from a paint: a frame is a pure
    /// function of the state, so it may not reshape what it is drawing.
    pub fn set_size(&mut self, cols: u16, rows: u16) {
        self.vt.resize(cols, rows);
    }

    pub fn size(&self) -> (u16, u16) {
        (
            self.vt.screen().cols() as u16,
            self.vt.screen().rows() as u16,
        )
    }

    /// Whether the device has written nothing at all.
    pub fn is_empty(&self) -> bool {
        let screen = self.vt.screen();
        screen.scrollback_len() == 0 && screen.is_blank() && self.vt.title().is_empty()
    }

    /// The whole retained output as plain text, escape sequences resolved.
    pub fn visible_text(&self) -> String {
        self.vt.dump()
    }

    /// Throw the history away. The device is not told; this is a screen, not a
    /// session.
    pub fn clear(&mut self) {
        let (cols, rows) = self.size();
        self.vt = Vt::new(cols, rows);
        self.parser = Parser::new();
        self.decoder = Utf8Stream::default();
        self.scroll = 0;
    }

    /// Write the retained output to `path`, returning the bytes written.
    pub fn save_log(&self, path: &Path) -> io::Result<usize> {
        let text = self.visible_text();
        std::fs::write(path, &text)?;
        Ok(text.len())
    }

    /// The window title, when the device has set one.
    pub fn title(&self) -> &str {
        self.vt.title()
    }

    /// The emulator, for a reader that needs the cursor or the screen.
    pub fn vt(&self) -> &Vt {
        &self.vt
    }

    /// Take the replies owed to the device — cursor-position reports and device
    /// attributes. Whatever calls this is responsible for putting them on the
    /// wire.
    pub fn take_replies(&mut self) -> Vec<Vec<u8>> {
        self.vt.take_replies()
    }
}

/// Fold `text` into rows of at most `width` columns, the way a terminal does.
///
/// This is the *document* wrapper, kept for text that is not a screen — the
/// help dialog's body. The serial pane no longer needs it: an emulated screen
/// wraps as it draws, and re-folding its output would undo the cursor.
///
/// Tabs advance to the next multiple of eight, and a wide glyph that would
/// straddle the edge starts the next row rather than being split — a split CJK
/// glyph is the one thing that cannot be drawn.
pub fn wrap_rows(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut rows = Vec::new();
    let mut row = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        match ch {
            '\n' => {
                rows.push(std::mem::take(&mut row));
                used = 0;
            }
            '\r' => {}
            '\t' => {
                // The next eight-column stop, or the row's edge when the stop
                // is past it — a tab moves the cursor along the line, it never
                // scrolls the screen.
                let stop = (((used / 8) + 1) * 8).min(width);
                while used < stop {
                    row.push(' ');
                    used += 1;
                }
            }
            other => {
                let glyph = text::columns(&other.to_string());
                if used + glyph > width {
                    rows.push(std::mem::take(&mut row));
                    used = 0;
                }
                row.push(other);
                used += glyph;
            }
        }
    }
    rows.push(row);
    rows
}

/// Which lines the pane shows, as absolute indices into the emulator's line
/// space.
///
/// The window ends at the newest line unless the user has scrolled back, so the
/// cursor stays in view; scrolling past the top of the history lands on the top
/// of the history rather than on blank space, because a scroll that can run off
/// the end leaves the user staring at nothing with no way to tell why.
///
/// `None` is a filler row *below* the newest line. A screen has a fixed number
/// of rows, so the only time there is filler at all is a pane taller than the
/// emulator was last told it is — a resize the event loop has not caught up
/// with. Padding below keeps the screen anchored where it has always been, at
/// the top; padding above would shift the whole terminal down a row and make the
/// device's first line look like its second.
pub fn visible_lines(pane: &TerminalPane, height: usize) -> Vec<Option<usize>> {
    if height == 0 {
        return Vec::new();
    }
    let total = pane.vt.line_count();
    let back = if pane.autoscroll { 0 } else { pane.scroll };
    let end = total.saturating_sub(back).max(height.min(total));
    let start = end.saturating_sub(height);
    let mut window: Vec<Option<usize>> = (start..end).map(Some).collect();
    while window.len() < height {
        window.push(None);
    }
    window
}

/// Where the cursor should be drawn, as `(absolute line, column)`, or `None`
/// when it should not be: another pane has the keyboard, or the device hid it,
/// or the user has scrolled back to read something the cursor is not on.
pub fn cursor_of(app: &App, pane: &TerminalPane) -> Option<(usize, usize)> {
    if app.focus != Focus::Terminal || !pane.autoscroll {
        return None;
    }
    if !pane.vt.screen().cursor_visible {
        return None;
    }
    Some((pane.vt.cursor_line(), pane.vt.screen().cursor().1))
}

/// A device-named colour as a value, with the terminal's own defaults standing in
/// for `Default` — its ink for a foreground, its ground for a fill.
fn resolve(theme: &Theme, colour: Ansi, foreground: bool) -> Color {
    match colour {
        Ansi::Default if foreground => theme.terminal_fg,
        Ansi::Default => theme.terminal_bg,
        Ansi::Indexed(index) => theme.indexed(index),
        Ansi::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

/// The style for one cell: the emulator's attributes resolved against the
/// theme.
///
/// Reverse video is applied by swapping the *resolved* colours, so a reversed
/// default cell is still the terminal's own foreground on its own background —
/// which is what `\e[7m` means, and what a status bar relies on.
fn cell_style(theme: &Theme, style: CellStyle) -> Style {
    let mut fg = resolve(theme, style.fg, true);
    let mut bg = resolve(theme, style.bg, false);
    if style.reverse {
        std::mem::swap(&mut fg, &mut bg);
    }
    let mut out = Style::default().fg(fg).bg(bg);
    if style.bold {
        out = out.add_modifier(Modifier::BOLD);
    }
    if style.dim {
        out = out.add_modifier(Modifier::DIM);
    }
    if style.italic {
        out = out.add_modifier(Modifier::ITALIC);
    }
    if style.underline {
        out = out.add_modifier(Modifier::UNDERLINED);
    }
    if style.blink {
        out = out.add_modifier(Modifier::SLOW_BLINK);
    }
    if style.hidden {
        out = out.add_modifier(Modifier::HIDDEN);
    }
    if style.strike {
        out = out.add_modifier(Modifier::CROSSED_OUT);
    }
    out
}

/// One row of the screen as spans, clipped to `width`, with the cursor cell
/// marked when it falls on this row.
///
/// Spacers are skipped rather than emitted: a wide glyph already claims two
/// columns, and writing anything for its trailing half would push the rest of
/// the row one column right.
fn row_line(
    theme: &Theme,
    cells: &[Cell],
    width: usize,
    cursor_col: Option<usize>,
) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut run = String::new();
    let mut run_style = Style::default();
    let mut used = 0usize;
    for (col, cell) in cells.iter().enumerate() {
        if cell.spacer {
            continue;
        }
        let glyph = cell.glyph();
        let glyph_width = text::columns(&glyph);
        if used + glyph_width > width {
            break;
        }
        let mut style = cell_style(theme, cell.style);
        if cursor_col == Some(col) {
            style = style.add_modifier(Modifier::REVERSED);
        }
        if style != run_style {
            if !run.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut run), run_style));
            }
            run_style = style;
        }
        run.push_str(&glyph);
        used += glyph_width;
    }
    if !run.is_empty() {
        spans.push(Span::styled(run, run_style));
    }
    Line::from(spans)
}

/// The terminal pane's body, sized to the whole centre area.
pub fn lines(app: &App, theme: &Theme, area: Rect) -> Vec<Line<'static>> {
    if area.width == 0 || area.height == 0 {
        return Vec::new();
    }
    let pane = &app.terminal;
    let width = area.width as usize;
    let height = area.height as usize;
    let cursor = cursor_of(app, pane);
    visible_lines(pane, height)
        .into_iter()
        .map(|line| match line {
            Some(index) => {
                let cells = pane.vt.line(index).unwrap_or(&[]);
                let cursor_col = match cursor {
                    Some((cursor_line, col)) if cursor_line == index => Some(col),
                    _ => None,
                };
                row_line(theme, cells, width, cursor_col)
            }
            None => Line::default(),
        })
        .collect()
}

/// The rows the pane shows as plain text — what the tests assert on, without a
/// frame or a theme.
pub fn rows(pane: &TerminalPane, width: usize, height: usize) -> Vec<String> {
    let theme = Theme::DARK;
    visible_lines(pane, height)
        .into_iter()
        .map(|line| match line {
            Some(index) => {
                let cells = pane.vt.line(index).unwrap_or(&[]);
                let text = row_line(&theme, cells, width, None)
                    .spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>();
                text.trim_end().to_string()
            }
            None => String::new(),
        })
        .collect()
}

/// The sample transcript `--sample` loads. It is the loopback device's session,
/// written out, so a frame can be looked at with a populated pane without
/// running a bridge.
pub const SAMPLE: &str = concat!(
    "Linkr Bee loopback device (no radio attached)\r\n",
    "Type a command; try: help, version, uname -a, df -h\r\n",
    "linkr@bee:~$ uname -a\r\n",
    "Linux bee 6.6.0-linkr #1 SMP aarch64 GNU/Linux\r\n",
    "linkr@bee:~$ df -h\r\n",
    "Filesystem      Size  Used Avail Use% Mounted on\r\n",
    "/dev/root        28G  7.4G   19G  29% /\r\n",
    "tmpfs           196M     0  196M   0% /run\r\n",
    "linkr@bee:~$ ",
);

/// A transcript that exercises the emulation rather than the layout: colour,
/// bold, a reverse-video status bar drawn with a scroll region, and a red
/// failure. `--vt-sample` loads it, so the emulator can be looked at on a
/// machine with no device attached.
pub const SAMPLE_COLOUR: &str = concat!(
    "\x1b[1;32mroot@kickpi\x1b[0m:\x1b[1;34m~\x1b[0m$ uname -a\r\n",
    "Linux kickpi-k2b 6.18.53-current-sunxi64 #14 SMP aarch64 GNU/Linux\r\n",
    "\x1b[1;32mroot@kickpi\x1b[0m:\x1b[1;34m~\x1b[0m$ linkr --transport ble --connect\r\n",
    "\x1b[36m→\x1b[0m linkr-bee \x1b[1mEE:C7:42:34:48:CF\x1b[0m rssi=\x1b[32m-34\x1b[0m\r\n",
    "\x1b[32m✓\x1b[0m protocol \x1b[1m1.0\x1b[0m max_payload=\x1b[33m512\x1b[0m\r\n",
    "\x1b[31mfailed\x1b[0m to open ws://192.168.0.100/ws: \x1b[31mconnection refused\x1b[0m\r\n",
    "\x1b[1;32mroot@kickpi\x1b[0m:\x1b[1;34m~\x1b[0m$ ",
    // A status bar, the way a full-screen program draws one: an inverse row
    // across the bottom of the screen, positioned with the cursor.
    "\x1b[4;1H\x1b[7m bridge up  \x1b[0m\x1b[7m uart 115200 8N1 ",
    "\x1b[0m\x1b[4;24H\x1b[7m rx 1284  tx 27 \x1b[0m",
);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Lang;

    /// A pane with a known size, so wrapping and scrolling are deterministic.
    fn sized(cols: u16, rows: u16) -> TerminalPane {
        let mut pane = TerminalPane::new();
        pane.set_size(cols, rows);
        pane
    }

    /// A character split across two reads is held until its bytes arrive: the
    /// failure this decoder exists to prevent, and the one a BLE notification
    /// boundary produces every time.
    #[test]
    fn a_character_split_across_reads_is_not_corrupted() {
        let mut stream = Utf8Stream::default();
        let text = "中文链路";
        let bytes = text.as_bytes();
        // Split inside the second character's three bytes.
        let (first, rest) = bytes.split_at(4);
        let head = stream.push(first);
        assert_eq!(head, "中");
        assert!(stream.is_pending(), "the half character is held");
        let tail = stream.push(rest);
        assert_eq!(tail, "文链路");
        assert!(!stream.is_pending());
        assert_eq!(format!("{head}{tail}"), text);
    }

    /// An invalid byte is reported once and does not swallow the rest of the
    /// burst — the wire is untrusted, but the text after a glitch is still the
    /// device's.
    #[test]
    fn an_invalid_byte_does_not_eat_the_burst() {
        let mut stream = Utf8Stream::default();
        let text = stream.push(b"ok\xffmore");
        assert_eq!(text, "ok\u{fffd}more");
        assert!(!stream.is_pending());
    }

    /// Decoding byte by byte gives the same text as decoding it all at once:
    /// the streaming path is not a second, subtly different, decoder.
    #[test]
    fn byte_by_byte_matches_one_shot() {
        let text = "a中b文c🚀d";
        let mut stream = Utf8Stream::default();
        let mut out = String::new();
        for byte in text.as_bytes() {
            out.push_str(&stream.push(&[*byte]));
        }
        assert_eq!(out, text);
        assert!(!stream.is_pending());
    }

    /// An escape sequence split across two reads is still one sequence. This is
    /// the parser equivalent of the UTF-8 split above, and the reason the pane
    /// keeps a parser instead of building one per chunk.
    #[test]
    fn an_escape_sequence_split_across_reads_is_not_sprayed() {
        let mut pane = sized(20, 4);
        pane.push(b"a\x1b[3");
        pane.push(b"1mred");
        let shown = rows(&pane, 20, 4);
        assert_eq!(shown[0], "ared", "{shown:?}");
        // The parameters did not become text.
        assert!(!shown[0].contains('3') || shown[0].starts_with("ared"));
        assert!(!shown[0].contains("1m"), "{shown:?}");
    }

    /// Cursor addressing draws where it is told. The transcript's rows are not
    /// a stack: the status bar sits on row four because the device said so.
    #[test]
    fn cursor_addressing_places_text_on_the_row_it_names() {
        let mut pane = sized(24, 5);
        pane.push_text("one\r\ntwo\r\n");
        pane.push_text("\x1b[4;1Hstatus");
        let shown = rows(&pane, 24, 5);
        assert_eq!(shown[0], "one");
        assert_eq!(shown[1], "two");
        assert_eq!(shown[2], "");
        assert_eq!(shown[3], "status");
    }

    /// A spinner repaints one cell instead of stacking frames: carriage return
    /// puts the cursor back at the start of the row.
    #[test]
    fn a_spinner_repaints_in_place() {
        let mut pane = sized(12, 3);
        pane.push_text("working |");
        pane.push_text("\rworking /");
        pane.push_text("\rworking -");
        let shown = rows(&pane, 12, 3);
        assert_eq!(shown[0], "working -", "{shown:?}");
        assert_eq!(shown[1], "", "no second frame: {shown:?}");
    }

    /// Colour reaches the cells and survives into the painted spans.
    #[test]
    fn colour_reaches_the_spans() {
        let mut app = App {
            lang: Lang::En,
            ..App::default()
        };
        app.terminal.set_size(20, 2);
        app.terminal.push_text("\x1b[31merr\x1b[0m ok");
        let painted = lines(&app, &Theme::DARK, Rect::new(0, 0, 20, 2));
        let first = &painted[0];
        assert_eq!(first.spans[0].content.as_ref(), "err");
        assert_eq!(
            first.spans[0].style.fg,
            Some(Theme::DARK.indexed(1)),
            "ANSI red is the palette's red"
        );
        assert_eq!(
            first.spans[1].style.fg,
            Some(Theme::DARK.terminal_fg),
            "the reset restores the pane's own ink"
        );
    }

    /// The pane paints exactly the rows it was given room for, and no row is
    /// wider than its area.
    #[test]
    fn the_pane_fills_its_area_without_overflowing() {
        let mut app = App::default();
        app.terminal.set_size(60, 8);
        app.terminal
            .push_text("uname -a\r\nLinux bee 6.6.0-linkr aarch64\r\n");
        for width in [12u16, 40, 60] {
            let area = Rect::new(0, 0, width, 4);
            let painted = lines(&app, &app.theme(), area);
            assert_eq!(painted.len(), 4);
            for line in painted {
                let joined: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                assert!(
                    text::columns(&joined) <= width as usize,
                    "{width}: {joined:?}"
                );
            }
        }
    }

    /// An empty pane is blank rows, not no rows: the design's first frame shows
    /// a pane with its ground and nothing in it.
    #[test]
    fn an_empty_pane_is_blank_rows() {
        let pane = sized(20, 3);
        let shown = rows(&pane, 20, 3);
        assert_eq!(shown, vec![String::new(), String::new(), String::new()]);
        assert!(pane.is_empty());
        // A zero-height pane paints nothing at all, which is the resize storm.
        assert!(lines(&App::default(), &Theme::DARK, Rect::new(0, 0, 20, 0)).is_empty());
    }

    /// A screen on a taller area is anchored at the top, the way a terminal is:
    /// the first line the device printed is the first line of the pane, and the
    /// rows it has not reached yet are blank below.
    #[test]
    fn a_short_screen_is_anchored_at_the_top() {
        let mut pane = sized(20, 5);
        pane.push_text("one\r\ntwo\r\nthree");
        assert_eq!(
            rows(&pane, 20, 5),
            vec!["one", "two", "three", "", ""],
            "the text starts where the device put it"
        );
        // A pane Rect taller than the emulator's rows — a resize the event loop
        // has not caught up with — pads below rather than shifting the screen
        // down a row.
        let taller = rows(&pane, 20, 7);
        assert_eq!(taller[..5].to_vec(), rows(&pane, 20, 5));
        assert_eq!(taller[5..].to_vec(), vec![String::new(), String::new()]);
    }

    /// Scrolling back holds the view still; autoscroll pulls it to the newest
    /// row again.
    #[test]
    fn scrolling_back_holds_the_view_still() {
        let mut pane = sized(20, 2);
        pane.push_text("one\r\ntwo\r\nthree\r\nfour\r\n");
        pane.autoscroll = false;
        pane.scroll = 1;
        assert_eq!(rows(&pane, 20, 2), vec!["three", "four"]);
        pane.scroll = 3;
        assert_eq!(rows(&pane, 20, 2), vec!["one", "two"]);
        // Past the top: the pane shows what there is, not blank space.
        pane.scroll = 99;
        assert_eq!(rows(&pane, 20, 5)[0], "one");
        pane.autoscroll = true;
        assert_eq!(rows(&pane, 20, 2), vec!["four", ""]);
    }

    /// A cleared pane is empty again, and the decoder and the parser are reset
    /// with it, so a half character or half sequence cannot appear in the new
    /// session's first line.
    #[test]
    fn clearing_the_pane_resets_the_decoders_too() {
        let mut pane = sized(20, 3);
        // A UTF-8 character cut in half by a read boundary, and an escape
        // sequence cut in half the same way.
        pane.push(&"中".as_bytes()[..1]);
        pane.push(b"\x1b[3");
        pane.clear();
        assert!(pane.is_empty());
        // The lead byte is gone, so the next character decodes whole.
        pane.push("文".as_bytes());
        assert_eq!(rows(&pane, 20, 3)[0], "文");
        // And the `\x1b[3` is gone with it: these bytes are text, not the tail
        // of a colour sequence that would have swallowed them.
        pane.push(b"1mx");
        assert_eq!(rows(&pane, 20, 3)[0], "文1mx", "the stale tail is gone");
    }

    /// The pane does not translate the device: a Chinese line stays Chinese in
    /// either interface language, because the pane is not the interface.
    #[test]
    fn the_pane_does_not_translate_the_device() {
        let mut app = App {
            lang: Lang::En,
            ..App::default()
        };
        app.terminal.set_size(20, 4);
        app.terminal.push_text("链路已连接\r\n");
        let shown = rows(&app.terminal, 20, 4);
        assert!(shown.iter().any(|r| r.contains("链路已连接")), "{shown:?}");
    }

    /// The cursor is drawn only when this pane has the keyboard and the device
    /// has not hidden it — and never while the user is reading history.
    #[test]
    fn the_cursor_is_drawn_only_when_it_belongs() {
        let mut app = App::default();
        app.terminal.set_size(20, 3);
        app.terminal.push_text("prompt ");
        assert_eq!(cursor_of(&app, &app.terminal), Some((0, 7)));

        app.focus = Focus::Sidebar;
        assert_eq!(cursor_of(&app, &app.terminal), None);
        app.focus = Focus::Terminal;

        app.terminal.push_text("\x1b[?25l");
        assert_eq!(cursor_of(&app, &app.terminal), None, "the device hid it");
        app.terminal.push_text("\x1b[?25h");

        app.terminal.autoscroll = false;
        assert_eq!(cursor_of(&app, &app.terminal), None, "reading history");
    }

    /// The cursor marks its cell in the painted row, so a shell prompt shows
    /// where the next character will land.
    #[test]
    fn the_cursor_marks_its_cell() {
        let mut app = App::default();
        app.terminal.set_size(20, 3);
        app.terminal.push_text("ab");
        let painted = lines(&app, &Theme::DARK, Rect::new(0, 0, 20, 3));
        let spans = &painted[0].spans;
        let marked = spans
            .iter()
            .find(|span| span.style.add_modifier.contains(Modifier::REVERSED))
            .expect("a cursor cell");
        assert_eq!(marked.content.as_ref(), " ", "{spans:?}");
    }

    /// The pane answers the device's questions, and hands the replies over once.
    #[test]
    fn the_pane_hands_over_the_devices_replies() {
        let mut pane = sized(20, 3);
        pane.push_text("\x1b[2;5H\x1b[6n");
        assert_eq!(pane.take_replies(), vec![b"\x1b[2;5R".to_vec()]);
        assert!(pane.take_replies().is_empty());
    }

    /// The window title reaches the pane, ready for a caller that wants to show
    /// it.
    #[test]
    fn the_title_is_available() {
        let mut pane = sized(20, 3);
        pane.push_text("\x1b]0;kickpi: ~\x07");
        assert_eq!(pane.title(), "kickpi: ~");
    }

    /// The saved log is plain text: a device's escape sequences must not end up
    /// in the file, or every line of it would come back coloured in an editor.
    #[test]
    fn the_saved_log_is_plain_text() {
        let mut pane = sized(20, 3);
        pane.push_text("\x1b[1;31mred\x1b[0m\r\nplain\r\n");
        let text = pane.visible_text();
        assert!(text.contains("red") && text.contains("plain"), "{text:?}");
        assert!(!text.contains('\u{1b}'), "{text:?}");
    }

    /// `wrap_rows` still folds a document for the help dialog, and a wide glyph
    /// is never split.
    #[test]
    fn wrap_rows_still_folds_documents() {
        assert_eq!(wrap_rows("abcdef", 4), vec!["abcd", "ef"]);
        assert_eq!(wrap_rows("中文中文", 4), vec!["中文", "中文"]);
        assert_eq!(wrap_rows("中中中", 4), vec!["中中", "中"]);
        assert_eq!(wrap_rows("a\tb", 16), vec!["a       b"]);
        assert_eq!(wrap_rows("a\tb", 4), vec!["a   ", "b"]);
        assert_eq!(wrap_rows("", 4), vec![""]);
    }

    /// The colour sample is a *screen*: its status bar is one row of inverse
    /// video, not three rows of stacked text.
    #[test]
    fn the_colour_sample_draws_one_status_row() {
        let mut pane = sized(24, 4);
        pane.push_text(SAMPLE_COLOUR);
        let text = pane.visible_text();
        assert!(text.contains("bridge up"), "{text:?}");
        assert_eq!(
            text.matches("bridge up").count(),
            1,
            "the status bar was drawn more than once"
        );
    }
}
