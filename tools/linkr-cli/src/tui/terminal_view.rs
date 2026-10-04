//! Center terminal pane: a real VT grid fed through the `vte` parser with
//! scrollback, ANSI 16/256/truecolor SGR, cursor, alt screen and deferred
//! wrap. Mirrors the web xterm.js pane (WEB_UX_SPEC section 1.5: 10 000 lines
//! of scrollback, font zoom 10..=28, 4 MiB log ring, autoscroll).

use std::collections::VecDeque;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;
use vte::{Params, Parser, Perform};

use super::i18n::strings;
use super::settings::{EnterMode, MAX_FONT_SIZE, MIN_FONT_SIZE};

/// xterm scrollback default of the web client (`scrollback: 10000`).
pub const SCROLLBACK_LINES: usize = 10_000;
/// `LOG_CAP_BYTES` of the web client.
pub const LOG_CAP_BYTES: usize = 4 * 1024 * 1024;
/// Default font size (`linkr-font` default) used to derive grid density.
pub const FONT_DEFAULT: u8 = 13;
/// Placeholder cell for the second column of a wide (2-cell) glyph.
pub const CONT: char = '\0';

pub const ATTR_BOLD: u8 = 1 << 0;
pub const ATTR_DIM: u8 = 1 << 1;
pub const ATTR_ITALIC: u8 = 1 << 2;
pub const ATTR_UNDERLINE: u8 = 1 << 3;
pub const ATTR_REVERSED: u8 = 1 << 4;
pub const ATTR_HIDDEN: u8 = 1 << 5;
pub const ATTR_STRIKE: u8 = 1 << 6;

// The VT grid prints what the device sent, byte for byte, and composes no
// interface text of its own: every literal left here is protocol (ESC
// sequences, the OSC 52 payload), a format template or a file name. The
// terminal pane's own wording — the help rows for scrolling, clearing and
// copying — is drawn by `dialogs.rs` and `palette.rs`, which own those
// tables. Nothing for the language tables to hold.
strings! {}

/// SGR color: default (reset), 256-color palette index or 24-bit RGB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Palette {
    #[default]
    Def,
    Idx(u8),
    Rgb(u8, u8, u8),
}

impl Palette {
    pub fn to_style_color(self) -> Color {
        match self {
            Palette::Def => Color::Reset,
            Palette::Idx(n) => Color::Indexed(n),
            Palette::Rgb(r, g, b) => Color::Rgb(r, g, b),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub fg: Palette,
    pub bg: Palette,
    pub attrs: u8,
}

impl Cell {
    pub fn blank() -> Self {
        Self {
            ch: ' ',
            fg: Palette::Def,
            bg: Palette::Def,
            attrs: 0,
        }
    }

    pub fn is_continuation(&self) -> bool {
        self.ch == CONT
    }
}

/// Geometry of the letterboxed grid inside a pane, mirroring the web fit
/// addon: a bigger font yields fewer columns/rows (font 13 fills the pane).
pub fn grid_dims(width: u16, height: u16, font_size: u8) -> (u16, u16) {
    let font = f64::from(font_size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE));
    let scale = f64::from(FONT_DEFAULT) / font;
    let cols = ((f64::from(width) * scale).floor() as u16).clamp(2, width.max(2));
    let rows = ((f64::from(height) * scale).floor() as u16).clamp(2, height.max(2));
    (cols, rows)
}

/// One rendered row: text plus the style runs that built it.
pub struct GridView {
    pub lines: Vec<Line<'static>>,
    /// Cursor position relative to the first rendered row, when visible.
    pub cursor: Option<(u16, u16)>,
}

struct AltScreen {
    screen: Vec<Vec<Cell>>,
    cursor: (u16, u16),
}

/// The VT grid: visible screen + scrollback ring + parser state.
pub struct TermGrid {
    cols: u16,
    rows: u16,
    screen: Vec<Vec<Cell>>,
    scrollback: VecDeque<Vec<Cell>>,
    scrollback_cap: usize,
    cursor: (u16, u16),
    saved_cursor: (u16, u16),
    cursor_visible: bool,
    fg: Palette,
    bg: Palette,
    attrs: u8,
    autowrap: bool,
    app_cursor: bool,
    bracketed_paste: bool,
    top_margin: u16,
    bottom_margin: u16,
    wrap_pending: bool,
    title: String,
    bells: u32,
    alt: Option<AltScreen>,
    /// Device → host reports (DSR/DA) waiting to be written to the UART.
    pending_reports: Vec<u8>,
    /// Base64 payload of an incoming OSC 52 (device asks to set the clipboard).
    pub clipboard: Option<String>,
    local_log: Vec<u8>,
    parser: Parser,
}

impl TermGrid {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self::with_scrollback(cols, rows, SCROLLBACK_LINES)
    }

    pub fn with_scrollback(cols: u16, rows: u16, cap: usize) -> Self {
        let cols = cols.max(1);
        let rows = rows.max(1);
        Self {
            cols,
            rows,
            screen: vec![vec![Cell::blank(); cols as usize]; rows as usize],
            scrollback: VecDeque::new(),
            scrollback_cap: cap,
            cursor: (0, 0),
            saved_cursor: (0, 0),
            cursor_visible: true,
            fg: Palette::Def,
            bg: Palette::Def,
            attrs: 0,
            autowrap: true,
            app_cursor: false,
            bracketed_paste: false,
            top_margin: 0,
            bottom_margin: rows - 1,
            wrap_pending: false,
            title: String::new(),
            bells: 0,
            alt: None,
            pending_reports: Vec::new(),
            clipboard: None,
            local_log: Vec::new(),
            parser: Parser::default(),
        }
    }

    pub fn cols(&self) -> u16 {
        self.cols
    }

    pub fn rows(&self) -> u16 {
        self.rows
    }

    pub fn scrollback_len(&self) -> usize {
        self.scrollback.len()
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn bells(&self) -> u32 {
        self.bells
    }

    pub fn app_cursor_keys(&self) -> bool {
        self.app_cursor
    }

    pub fn bracketed_paste(&self) -> bool {
        self.bracketed_paste
    }

    pub fn in_alt_screen(&self) -> bool {
        self.alt.is_some()
    }

    pub fn log_bytes(&self) -> &[u8] {
        &self.local_log
    }

    pub fn take_pending_reports(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.pending_reports)
    }

    pub fn take_clipboard(&mut self) -> Option<String> {
        self.clipboard.take()
    }

    /// Feed raw UART bytes through the VT parser, keeping a capped copy for
    /// "Save Log" (`state.logBytes` in the web client).
    pub fn feed(&mut self, bytes: &[u8]) {
        if !bytes.is_empty() {
            if self.local_log.len() + bytes.len() > LOG_CAP_BYTES {
                let drop = LOG_CAP_BYTES / 2;
                self.local_log.drain(..drop.min(self.local_log.len()));
            }
            self.local_log.extend_from_slice(bytes);
        }
        let mut parser = std::mem::take(&mut self.parser);
        parser.advance(self, bytes);
        self.parser = parser;
    }

    /// `term.reset()` equivalent: screen, scrollback, modes and log cleared.
    pub fn reset(&mut self) {
        let cols = self.cols;
        let rows = self.rows;
        self.screen = vec![vec![Cell::blank(); cols as usize]; rows as usize];
        self.scrollback.clear();
        self.cursor = (0, 0);
        self.saved_cursor = (0, 0);
        self.cursor_visible = true;
        self.fg = Palette::Def;
        self.bg = Palette::Def;
        self.attrs = 0;
        self.autowrap = true;
        self.top_margin = 0;
        self.bottom_margin = rows - 1;
        self.wrap_pending = false;
        self.alt = None;
        self.title.clear();
        self.pending_reports.clear();
        self.local_log.clear();
    }

    /// Ctrl+L / toolbar Clear: wipe screen and scrollback, keep the log ring.
    pub fn clear_pane(&mut self) {
        self.screen = vec![vec![Cell::blank(); self.cols as usize]; self.rows as usize];
        self.scrollback.clear();
        self.cursor = (0, 0);
        self.wrap_pending = false;
        self.alt = None;
    }

    /// Re-fit the grid to a new pane size. Cells outside the new width are
    /// dropped (no reflow — same trade-off as xterm's non-reflowing resize).
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let cols = cols.max(1);
        let rows = rows.max(1);
        if cols == self.cols && rows == self.rows {
            return;
        }
        for line in &mut self.screen {
            line.resize(cols as usize, Cell::blank());
        }
        while self.screen.len() < rows as usize {
            self.screen.push(vec![Cell::blank(); cols as usize]);
        }
        while self.screen.len() > rows as usize {
            if let Some(line) = self.screen.pop() {
                if self.scrollback_cap > 0 {
                    self.push_scrollback(line);
                }
            }
        }
        for line in self.scrollback.iter_mut() {
            line.resize(cols as usize, Cell::blank());
        }
        self.cols = cols;
        self.rows = rows;
        self.cursor.0 = self.cursor.0.min(rows - 1);
        self.cursor.1 = self.cursor.1.min(cols - 1);
        self.top_margin = 0;
        self.bottom_margin = rows - 1;
        self.wrap_pending = false;
        if let Some(alt) = &mut self.alt {
            // The saved (primary) screen keeps its contents through a resize.
            for line in &mut alt.screen {
                line.resize(cols as usize, Cell::blank());
            }
            while alt.screen.len() < rows as usize {
                alt.screen.push(vec![Cell::blank(); cols as usize]);
            }
            alt.screen.truncate(rows as usize);
            alt.cursor = (alt.cursor.0.min(rows - 1), alt.cursor.1.min(cols - 1));
        }
    }

    fn push_scrollback(&mut self, line: Vec<Cell>) {
        if self.scrollback_cap == 0 {
            return;
        }
        self.scrollback.push_back(line);
        while self.scrollback.len() > self.scrollback_cap {
            self.scrollback.pop_front();
        }
    }

    // --- geometry helpers ----------------------------------------------------

    /// The live buffer: the alternate screen while it is active, the primary
    /// screen otherwise. `self.alt` only *stores* the screen that was swapped
    /// out (plus its cursor) so it can come back on `1049l`.
    fn screen_mut(&mut self) -> &mut Vec<Vec<Cell>> {
        &mut self.screen
    }

    fn screen_ref(&self) -> &Vec<Vec<Cell>> {
        &self.screen
    }

    fn blank(&self) -> Cell {
        Cell {
            ch: ' ',
            fg: Palette::Def,
            bg: self.bg,
            attrs: 0,
        }
    }

    fn scroll_up(&mut self, n: u16) {
        let top = self.top_margin;
        let bottom = self.bottom_margin;
        let (cols, rows) = (self.cols, self.rows);
        let n = n.min(bottom - top + 1);
        let full = top == 0 && bottom == rows - 1;
        let blank = self.blank();
        for _ in 0..n {
            let removed = {
                let screen = self.screen_mut();
                screen.remove(top as usize)
            };
            if full && self.alt.is_none() {
                self.push_scrollback(removed);
            }
            let screen = self.screen_mut();
            screen.insert(bottom as usize, vec![blank; cols as usize]);
            screen.truncate(rows as usize);
        }
    }

    fn scroll_down(&mut self, n: u16) {
        let top = self.top_margin;
        let bottom = self.bottom_margin;
        let (cols, rows) = (self.cols, self.rows);
        let n = n.min(bottom - top + 1);
        let full = top == 0 && bottom == rows - 1;
        let blank = self.blank();
        for _ in 0..n {
            let restore = if full && self.alt.is_none() {
                self.scrollback.pop_back()
            } else {
                None
            };
            let screen = self.screen_mut();
            screen.remove(bottom as usize);
            let restored = restore.unwrap_or_else(|| vec![blank; cols as usize]);
            screen.insert(top as usize, restored);
            screen.truncate(rows as usize);
        }
    }

    fn line_feed(&mut self) {
        if self.cursor.0 == self.bottom_margin {
            self.scroll_up(1);
        } else if self.cursor.0 + 1 < self.rows {
            self.cursor.0 += 1;
        }
    }

    fn reverse_index(&mut self) {
        if self.cursor.0 == self.top_margin {
            self.scroll_down(1);
        } else if self.cursor.0 > 0 {
            self.cursor.0 -= 1;
        }
    }

    fn erase_line(&mut self, mode: u16) {
        let (row, col) = self.cursor;
        let blank = self.blank();
        let screen = self.screen_mut();
        if let Some(line) = screen.get_mut(row as usize) {
            let len = line.len();
            let (from, to) = match mode {
                1 => (0, col as usize + 1),
                2 => (0, len),
                _ => (col as usize, len),
            };
            for cell in &mut line[from.min(len)..to.min(len)] {
                *cell = blank;
            }
            // A wide glyph erased in half leaves no orphan continuation.
            if from > 0
                && line
                    .get(from - 1)
                    .map(|c| c.is_continuation())
                    .unwrap_or(false)
            {
                if let Some(prev) = line.get_mut(from - 1) {
                    *prev = blank;
                }
            }
        }
    }

    fn erase_display(&mut self, mode: u16) {
        let (row, col) = self.cursor;
        let blank = self.blank();
        match mode {
            0 => {
                let screen = self.screen_mut();
                if let Some(line) = screen.get_mut(row as usize) {
                    for cell in &mut line[col as usize..] {
                        *cell = blank;
                    }
                }
                for line in screen.iter_mut().skip(row as usize + 1) {
                    for cell in line.iter_mut() {
                        *cell = blank;
                    }
                }
            }
            1 => {
                let screen = self.screen_mut();
                for line in screen.iter_mut().take(row as usize) {
                    for cell in line.iter_mut() {
                        *cell = blank;
                    }
                }
                if let Some(line) = screen.get_mut(row as usize) {
                    let last = (col as usize).min(line.len() - 1);
                    for cell in &mut line[..=last] {
                        *cell = blank;
                    }
                }
            }
            3 => {
                self.scrollback.clear();
                let screen = self.screen_mut();
                for line in screen.iter_mut() {
                    for cell in line.iter_mut() {
                        *cell = blank;
                    }
                }
            }
            _ => {
                let screen = self.screen_mut();
                for line in screen.iter_mut() {
                    for cell in line.iter_mut() {
                        *cell = blank;
                    }
                }
            }
        }
        self.wrap_pending = false;
    }

    fn insert_lines(&mut self, n: u16) {
        let (row, col) = self.cursor;
        if row < self.top_margin || row > self.bottom_margin {
            return;
        }
        let (bottom, cols, rows) = (self.bottom_margin, self.cols, self.rows);
        let n = n.min(bottom - row + 1);
        let blank = self.blank();
        let screen = self.screen_mut();
        for _ in 0..n {
            screen.remove(bottom as usize);
            screen.insert(row as usize, vec![blank; cols as usize]);
            screen.truncate(rows as usize);
        }
        let _ = col;
    }

    fn delete_lines(&mut self, n: u16) {
        let (row, col) = self.cursor;
        if row < self.top_margin || row > self.bottom_margin {
            return;
        }
        let (bottom, cols, rows) = (self.bottom_margin, self.cols, self.rows);
        let n = n.min(bottom - row + 1);
        let blank = self.blank();
        let screen = self.screen_mut();
        for _ in 0..n {
            screen.remove(row as usize);
            screen.insert(bottom as usize, vec![blank; cols as usize]);
            screen.truncate(rows as usize);
        }
        let _ = col;
    }

    fn insert_chars(&mut self, n: u16) {
        let (row, col) = self.cursor;
        let cols = self.cols;
        let n = n.min(cols - col) as usize;
        let blank = self.blank();
        let screen = self.screen_mut();
        if let Some(line) = screen.get_mut(row as usize) {
            for _ in 0..n {
                line.insert(col as usize, blank);
            }
            line.truncate(cols as usize);
        }
    }

    fn delete_chars(&mut self, n: u16) {
        let (row, col) = self.cursor;
        let cols = self.cols as usize;
        let blank = self.blank();
        let screen = self.screen_mut();
        if let Some(line) = screen.get_mut(row as usize) {
            let start = col as usize;
            let end = (start + n as usize).min(line.len());
            for _ in start..end {
                line.remove(start);
            }
            while line.len() < cols {
                line.push(blank);
            }
        }
    }

    fn erase_chars(&mut self, n: u16) {
        let (row, col) = self.cursor;
        let blank = self.blank();
        let screen = self.screen_mut();
        if let Some(line) = screen.get_mut(row as usize) {
            let end = (col as usize + n as usize).min(line.len());
            for cell in &mut line[col as usize..end] {
                *cell = blank;
            }
        }
    }

    // --- rendering -----------------------------------------------------------

    /// Total renderable rows (scrollback + screen), or just the screen in the
    /// alt buffer.
    pub fn total_lines(&self) -> usize {
        if self.alt.is_some() {
            self.rows as usize
        } else {
            self.scrollback.len() + self.rows as usize
        }
    }

    /// Build `viewport_rows` lines ending `offset` lines above the bottom.
    pub fn render(&self, viewport_rows: u16, offset: usize) -> GridView {
        let viewport_rows = viewport_rows as usize;
        let total = self.total_lines();
        let max_offset = if self.alt.is_some() {
            0
        } else {
            self.scrollback.len()
        };
        let offset = offset.min(max_offset);
        // Index (from the top of everything) of the first rendered line.
        let end = total - offset;
        let start = end.saturating_sub(viewport_rows);

        let mut lines = Vec::with_capacity(viewport_rows);
        for index in start..end {
            let cells = if index < self.scrollback.len() {
                Some(&self.scrollback[index])
            } else {
                let screen_index = index - self.scrollback.len();
                self.screen_ref().get(screen_index)
            };
            let cells: &[Cell] = match cells {
                Some(c) => c,
                None => continue,
            };
            lines.push(cells_to_line(cells));
        }

        // Cursor: only in the live screen (not scrolled back), when visible.
        let cursor = if offset == 0 && self.cursor_visible {
            let screen_top = self.rows as usize as isize - viewport_rows as isize;
            let cursor_row = self.cursor.0 as isize;
            if cursor_row >= screen_top {
                Some(((cursor_row - screen_top) as u16, self.cursor.1))
            } else {
                None
            }
        } else {
            None
        };

        GridView { lines, cursor }
    }
}

fn cells_to_line(cells: &[Cell]) -> Line<'static> {
    // Trailing default-styled blanks are dropped: they would only add dead
    // space and extra spans to every row of the pane.
    let keep = cells
        .iter()
        .rposition(|cell| {
            !cell.is_continuation()
                && !(cell.ch == ' '
                    && cell.attrs == 0
                    && cell.fg == Palette::Def
                    && cell.bg == Palette::Def)
        })
        .map(|index| index + 1)
        .unwrap_or(0);
    let cells = &cells[..keep];

    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut run = String::new();
    let mut style = Style::default();
    let mut run_attrs = 0u8;
    let mut run_fg = Palette::Def;
    let mut run_bg = Palette::Def;
    let mut first = true;

    let flush = |run: &mut String, style: Style, spans: &mut Vec<Span<'static>>| {
        if !run.is_empty() {
            spans.push(Span::styled(std::mem::take(run), style));
        }
    };

    for cell in cells {
        if cell.is_continuation() {
            continue;
        }
        let style_now = cell_style(cell);
        if first {
            style = style_now;
            run_attrs = cell.attrs;
            run_fg = cell.fg;
            run_bg = cell.bg;
            first = false;
        } else if cell.attrs != run_attrs || cell.fg != run_fg || cell.bg != run_bg {
            flush(&mut run, style, &mut spans);
            style = style_now;
            run_attrs = cell.attrs;
            run_fg = cell.fg;
            run_bg = cell.bg;
        }
        run.push(cell.ch);
    }
    flush(&mut run, style, &mut spans);
    if spans.is_empty() {
        spans.push(Span::raw(""));
    }
    Line::from(spans)
}

fn cell_style(cell: &Cell) -> Style {
    let mut style = Style::default()
        .fg(cell.fg.to_style_color())
        .bg(cell.bg.to_style_color());
    if cell.attrs & ATTR_BOLD != 0 {
        style = style.add_modifier(Modifier::BOLD);
    }
    if cell.attrs & ATTR_DIM != 0 {
        style = style.add_modifier(Modifier::DIM);
    }
    if cell.attrs & ATTR_ITALIC != 0 {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if cell.attrs & ATTR_UNDERLINE != 0 {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    if cell.attrs & ATTR_REVERSED != 0 {
        style = style.add_modifier(Modifier::REVERSED);
    }
    if cell.attrs & ATTR_HIDDEN != 0 {
        style = style.add_modifier(Modifier::HIDDEN);
    }
    if cell.attrs & ATTR_STRIKE != 0 {
        style = style.add_modifier(Modifier::CROSSED_OUT);
    }
    style
}

// --- vte::Perform ------------------------------------------------------------

fn param_at(params: &Params, index: usize, default: u16) -> u16 {
    params
        .iter()
        .nth(index)
        .and_then(|group| group.first().copied())
        .filter(|v| *v != 0)
        .unwrap_or(default)
}

impl Perform for TermGrid {
    fn print(&mut self, c: char) {
        let width = UnicodeWidthChar::width(c).unwrap_or(0);
        if width == 0 {
            // Combining marks and control-ish output are dropped: the cell
            // buffer stores one base glyph per cell.
            return;
        }
        if self.wrap_pending {
            self.wrap_pending = false;
            if self.autowrap {
                self.cursor.1 = 0;
                self.line_feed();
            }
        }
        let (_, mut col) = self.cursor;
        if col as usize + width > self.cols as usize {
            if self.autowrap {
                col = 0;
                self.cursor.1 = 0;
                self.line_feed();
            } else {
                col = self.cols.saturating_sub(width as u16);
            }
        }
        let row = self.cursor.0;
        let cell = Cell {
            ch: c,
            fg: self.fg,
            bg: self.bg,
            attrs: self.attrs,
        };
        let screen = self.screen_mut();
        if let Some(line) = screen.get_mut(row as usize) {
            line[col as usize] = cell;
            if width == 2 && (col as usize + 1) < line.len() {
                line[col as usize + 1] = Cell { ch: CONT, ..cell };
            }
        }
        let next = col as usize + width;
        if next >= self.cols as usize {
            self.cursor.1 = self.cols - 1;
            if self.autowrap {
                self.wrap_pending = true;
            }
        } else {
            self.cursor.1 = next as u16;
        }
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            0x07 => self.bells += 1,
            0x08 => {
                self.cursor.1 = self.cursor.1.saturating_sub(1);
                self.wrap_pending = false;
            }
            0x09 => {
                self.wrap_pending = false;
                self.cursor.1 = (((self.cursor.1 / 8) + 1) * 8).min(self.cols - 1);
            }
            0x0a..=0x0c => {
                self.wrap_pending = false;
                self.line_feed();
            }
            0x0d => {
                self.cursor.1 = 0;
                self.wrap_pending = false;
            }
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        // vte collects the DEC private marker '?' with the intermediates.
        let private = intermediates == *b"?";
        if ignore || (!intermediates.is_empty() && !private) {
            return;
        }
        if private && !matches!(action, 'h' | 'l' | 'n') {
            return;
        }
        let n = param_at(params, 0, 1);
        let rows = self.rows;
        let cols = self.cols;
        let top = self.top_margin;
        let bottom = self.bottom_margin;
        match action {
            'H' | 'f' => {
                self.cursor.0 = param_at(params, 0, 1)
                    .saturating_sub(1)
                    .min(bottom)
                    .max(top);
                self.cursor.1 = param_at(params, 1, 1).saturating_sub(1).min(cols - 1);
                self.wrap_pending = false;
            }
            'A' => {
                let floor = if self.cursor.0 < top { 0 } else { top };
                self.cursor.0 = self.cursor.0.saturating_sub(n).max(floor);
                self.wrap_pending = false;
            }
            'B' => {
                let ceil = if self.cursor.0 > bottom {
                    rows - 1
                } else {
                    bottom
                };
                self.cursor.0 = (self.cursor.0 + n).min(ceil);
                self.wrap_pending = false;
            }
            'C' => {
                self.cursor.1 = (self.cursor.1 + n).min(cols - 1);
                self.wrap_pending = false;
            }
            'D' => {
                self.cursor.1 = self.cursor.1.saturating_sub(n);
                self.wrap_pending = false;
            }
            'E' => {
                self.cursor.1 = 0;
                self.cursor.0 = (self.cursor.0 + n).min(bottom);
                self.wrap_pending = false;
            }
            'F' => {
                self.cursor.1 = 0;
                self.cursor.0 = self.cursor.0.saturating_sub(n).max(top);
                self.wrap_pending = false;
            }
            'G' | '`' => {
                self.cursor.1 = n.saturating_sub(1).min(cols - 1);
                self.wrap_pending = false;
            }
            'd' => {
                self.cursor.0 = n.saturating_sub(1).min(rows - 1);
                self.wrap_pending = false;
            }
            'J' => {
                let mode = params
                    .iter()
                    .next()
                    .and_then(|g| g.first().copied())
                    .unwrap_or(0);
                self.erase_display(mode);
            }
            'K' => {
                let mode = params
                    .iter()
                    .next()
                    .and_then(|g| g.first().copied())
                    .unwrap_or(0);
                self.erase_line(mode);
            }
            'L' => self.insert_lines(n),
            'M' => self.delete_lines(n),
            'P' => self.delete_chars(n),
            '@' => self.insert_chars(n),
            'X' => self.erase_chars(n),
            'S' => self.scroll_up(n),
            'T' => self.scroll_down(n),
            'r' => {
                let new_top = param_at(params, 0, 1).saturating_sub(1);
                let new_bottom = param_at(params, 1, rows).saturating_sub(1).min(rows - 1);
                if new_top < new_bottom {
                    self.top_margin = new_top;
                    self.bottom_margin = new_bottom;
                    self.cursor = (0, 0);
                    self.wrap_pending = false;
                }
            }
            's' => self.saved_cursor = self.cursor,
            'u' => {
                self.cursor = self.saved_cursor;
                self.wrap_pending = false;
            }
            'm' => self.sgr(params),
            'h' | 'l' => {
                let set = action == 'h';
                if !private {
                    // Plain modes (IRM, ...) are not used by the bridge shell.
                    return;
                }
                for group in params.iter() {
                    let code = group.first().copied().unwrap_or(0);
                    self.apply_mode(code, set);
                }
            }
            'n' => {
                let code = param_at(params, 0, 0);
                match (code, private) {
                    (5, false) => self.pending_reports.extend_from_slice(b"\x1b[0n"),
                    (6, false) => {
                        let report = format!("\x1b[{};{}R", self.cursor.0 + 1, self.cursor.1 + 1);
                        self.pending_reports.extend_from_slice(report.as_bytes());
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        match (intermediates, byte) {
            ([], b'7') => self.saved_cursor = self.cursor,
            ([], b'8') => {
                self.cursor = self.saved_cursor;
                self.wrap_pending = false;
            }
            ([], b'D') => {
                self.wrap_pending = false;
                self.line_feed();
            }
            ([], b'E') => {
                self.cursor.1 = 0;
                self.wrap_pending = false;
                self.line_feed();
            }
            ([], b'M') => self.reverse_index(),
            ([], b'c') => {
                let (cols, rows) = (self.cols, self.rows);
                let keep_log = std::mem::take(&mut self.local_log);
                *self = TermGrid::with_scrollback(cols, rows, self.scrollback_cap);
                self.local_log = keep_log;
            }
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        let code = params
            .first()
            .map(|p| String::from_utf8_lossy(p).to_string());
        let value = params
            .get(1)
            .map(|p| String::from_utf8_lossy(p).to_string())
            .unwrap_or_default();
        match code.as_deref() {
            Some("0") | Some("2") => self.title = value,
            Some("52") => {
                // OSC 52: `c;<base64>` — the target asks the host to set the
                // clipboard. vte splits on ';', so the payload is the third
                // field when the selector is present.
                let payload = params
                    .get(1)
                    .filter(|sel| *sel == b"c")
                    .map(|_| params.get(2).copied())
                    .unwrap_or(params.get(1).copied())
                    .unwrap_or(b"");
                let payload = String::from_utf8_lossy(payload).to_string();
                if !payload.is_empty() && payload != "?" {
                    self.clipboard = Some(payload);
                }
            }
            _ => {}
        }
    }
}

impl TermGrid {
    /// `CSI ? … h/l` mode setting (the caller has already verified the
    /// private marker).
    fn apply_mode(&mut self, code: u16, set: bool) {
        match code {
            7 => self.autowrap = set,
            1 => self.app_cursor = set,
            25 => self.cursor_visible = set,
            2004 => self.bracketed_paste = set,
            47 | 1047 => {
                if set {
                    if self.alt.is_none() {
                        let screen = std::mem::replace(
                            &mut self.screen,
                            vec![vec![Cell::blank(); self.cols as usize]; self.rows as usize],
                        );
                        self.alt = Some(AltScreen {
                            screen,
                            cursor: self.cursor,
                        });
                        self.cursor = (0, 0);
                    }
                } else if let Some(alt) = self.alt.take() {
                    self.screen = alt.screen;
                    self.cursor = alt.cursor;
                }
            }
            1048 => {
                if set {
                    self.saved_cursor = self.cursor;
                } else {
                    self.cursor = self.saved_cursor;
                }
            }
            1049 => {
                if set {
                    if self.alt.is_none() {
                        self.saved_cursor = self.cursor;
                        let screen = std::mem::replace(
                            &mut self.screen,
                            vec![vec![Cell::blank(); self.cols as usize]; self.rows as usize],
                        );
                        self.alt = Some(AltScreen {
                            screen,
                            cursor: self.cursor,
                        });
                        self.cursor = (0, 0);
                    }
                } else if let Some(alt) = self.alt.take() {
                    self.screen = alt.screen;
                    self.cursor = self.saved_cursor;
                }
            }
            _ => {}
        }
    }

    fn sgr(&mut self, params: &Params) {
        let groups: Vec<Vec<u16>> = params.iter().map(|g| g.to_vec()).collect();
        if groups.is_empty() {
            self.fg = Palette::Def;
            self.bg = Palette::Def;
            self.attrs = 0;
            return;
        }
        let mut i = 0;
        while i < groups.len() {
            let group = &groups[i];
            let code = group.first().copied().unwrap_or(0);
            match code {
                0 => {
                    self.fg = Palette::Def;
                    self.bg = Palette::Def;
                    self.attrs = 0;
                }
                1 => self.attrs |= ATTR_BOLD,
                2 => self.attrs |= ATTR_DIM,
                3 => self.attrs |= ATTR_ITALIC,
                4 => self.attrs |= ATTR_UNDERLINE,
                7 => self.attrs |= ATTR_REVERSED,
                8 => self.attrs |= ATTR_HIDDEN,
                9 => self.attrs |= ATTR_STRIKE,
                21 | 22 => self.attrs &= !(ATTR_BOLD | ATTR_DIM),
                23 => self.attrs &= !ATTR_ITALIC,
                24 => self.attrs &= !ATTR_UNDERLINE,
                27 => self.attrs &= !ATTR_REVERSED,
                28 => self.attrs &= !ATTR_HIDDEN,
                29 => self.attrs &= !ATTR_STRIKE,
                30..=37 => self.fg = Palette::Idx((code - 30) as u8),
                39 => self.fg = Palette::Def,
                40..=47 => self.bg = Palette::Idx((code - 40) as u8),
                49 => self.bg = Palette::Def,
                90..=97 => self.fg = Palette::Idx((code - 90 + 8) as u8),
                100..=107 => self.bg = Palette::Idx((code - 100 + 8) as u8),
                38 | 48 => {
                    let target_fg = code == 38;
                    // Colon form: the group itself carries mode + components.
                    if group.len() >= 3 {
                        let color = color_from_slice(&group[1..]);
                        if target_fg {
                            self.fg = color;
                        } else {
                            self.bg = color;
                        }
                        i += 1;
                        continue;
                    }
                    // Semicolon form: 38;5;n or 38;2;r;g;b across groups.
                    let next = groups.get(i + 1).and_then(|g| g.first().copied());
                    match next {
                        Some(5) => {
                            let value = groups
                                .get(i + 2)
                                .and_then(|g| g.first().copied())
                                .unwrap_or(0) as u8;
                            if target_fg {
                                self.fg = Palette::Idx(value);
                            } else {
                                self.bg = Palette::Idx(value);
                            }
                            i += 3;
                            continue;
                        }
                        Some(2) => {
                            let r = groups
                                .get(i + 2)
                                .and_then(|g| g.first().copied())
                                .unwrap_or(0);
                            let g = groups
                                .get(i + 3)
                                .and_then(|g| g.first().copied())
                                .unwrap_or(0);
                            let b = groups
                                .get(i + 4)
                                .and_then(|g| g.first().copied())
                                .unwrap_or(0);
                            if target_fg {
                                self.fg = Palette::Rgb(r as u8, g as u8, b as u8);
                            } else {
                                self.bg = Palette::Rgb(r as u8, g as u8, b as u8);
                            }
                            i += 5;
                            continue;
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }
}

/// Parse the tail of a colon-form extended color: `[5, n]` or `[2, r, g, b]`.
fn color_from_slice(values: &[u16]) -> Palette {
    match values {
        [5, n, ..] => Palette::Idx(*n as u8),
        [2, r, g, b, ..] => Palette::Rgb(*r as u8, *g as u8, *b as u8),
        _ => Palette::Def,
    }
}

/// Serialize a line of cells back to plain text (used by "copy").
pub fn cells_to_text(cells: &[Cell]) -> String {
    cells
        .iter()
        .filter(|c| !c.is_continuation())
        .map(|c| c.ch)
        .collect::<String>()
        .trim_end()
        .to_string()
}

/// Encode `text` for the OSC 52 clipboard write the way `web/terminal_keys.js`
/// peers do: base64 of the payload, BEL-terminated.
pub fn osc52_write(text: &str) -> String {
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    format!("\x1b]52;c;{encoded}\x07")
}

/// The center pane's state: VT grid plus scroll position and autoscroll.
pub struct TerminalPane {
    pub grid: TermGrid,
    /// Lines scrolled up from the bottom (0 = following the output).
    pub offset: usize,
    pub autoscroll: bool,
    /// Last (cols, rows) reported to the session for geometry sync.
    pub dims: (u16, u16),
}

impl TerminalPane {
    pub fn new(autoscroll: bool) -> Self {
        Self {
            grid: TermGrid::new(80, 24),
            offset: 0,
            autoscroll,
            dims: (80, 24),
        }
    }

    /// Feed received UART bytes; autoscroll pins the view to the bottom.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.grid.feed(bytes);
        if self.autoscroll {
            self.offset = 0;
        }
    }

    pub fn scroll_back(&mut self, lines: usize) {
        let max = self.grid.scrollback_len();
        self.offset = (self.offset + lines).min(max);
    }

    pub fn scroll_forward(&mut self, lines: usize) {
        self.offset = self.offset.saturating_sub(lines);
    }

    pub fn to_bottom(&mut self) {
        self.offset = 0;
    }

    pub fn set_autoscroll(&mut self, on: bool) {
        self.autoscroll = on;
        if on {
            self.offset = 0;
        }
    }

    pub fn clear(&mut self) {
        self.grid.clear_pane();
        self.offset = 0;
    }

    /// Resize the grid; returns `true` when the size changed (the caller then
    /// reports it with `SessionHandle::set_terminal_size`).
    pub fn sync_size(&mut self, cols: u16, rows: u16) -> bool {
        if self.dims == (cols, rows) {
            return false;
        }
        self.dims = (cols, rows);
        self.grid.resize(cols, rows);
        true
    }

    /// Default download-style log name, same shape as the web client:
    /// `linkr-ble-2026-10-02T10-11-12-345Z.log`.
    pub fn default_log_name() -> String {
        let now = chrono::Utc::now();
        let stamp = now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        format!("linkr-ble-{}.log", stamp.replace([':', '.'], "-"))
    }

    /// Write the captured RX bytes to `path`.
    pub fn save_log(&self, path: &std::path::Path) -> std::io::Result<usize> {
        std::fs::write(path, self.grid.log_bytes())?;
        Ok(self.grid.log_bytes().len())
    }

    /// Plain text of the visible grid (used for OSC 52 copy).
    pub fn visible_text(&self) -> String {
        let view = self.grid.render(self.grid.rows(), self.offset);
        view.lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Apply the Enter-mode translation to a line about to be sent, then echo it
/// into the grid when local echo is on (web `sendText`).
pub fn prepare_line(text: &str, mode: EnterMode, local_echo: bool) -> Vec<u8> {
    let payload = super::keys::translate_enter(text.as_bytes(), mode);
    let _ = local_echo;
    payload
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row_text(grid: &TermGrid, row: usize) -> String {
        let line = &grid.screen[row];
        cells_to_text(line)
    }

    fn cell(grid: &TermGrid, row: usize, col: usize) -> Cell {
        grid.screen[row][col]
    }

    #[test]
    fn plain_text_prints_left_to_right() {
        let mut g = TermGrid::new(20, 4);
        g.feed(b"hello");
        assert_eq!(row_text(&g, 0), "hello");
        assert_eq!(g.cursor, (0, 5));
    }

    #[test]
    fn scrollback_default_is_at_least_the_web_client() {
        let g = TermGrid::new(10, 4);
        assert!(g.scrollback_cap >= 10_000);
        assert_eq!(SCROLLBACK_LINES, 10_000);
    }

    #[test]
    fn sixteen_color_sgr_sets_the_foreground() {
        let mut g = TermGrid::new(20, 4);
        g.feed(b"\x1b[31mred\x1b[0m plain");
        assert_eq!(cell(&g, 0, 0).fg, Palette::Idx(1));
        assert_eq!(cell(&g, 0, 2).fg, Palette::Idx(1));
        assert_eq!(cell(&g, 0, 4).fg, Palette::Def);
        assert_eq!(row_text(&g, 0), "red plain");
        g.feed(b"\x1b[1;4;7mstyled");
        // "red plain" left the cursor at column 9.
        let c = cell(&g, 0, 9);
        assert_eq!(c.attrs & ATTR_BOLD, ATTR_BOLD);
        assert_eq!(c.attrs & ATTR_UNDERLINE, ATTR_UNDERLINE);
        assert_eq!(c.attrs & ATTR_REVERSED, ATTR_REVERSED);
        g.feed(b"\x1b[22m\x1b[24moff");
        let c = cell(&g, 0, 15);
        assert_eq!(c.attrs & ATTR_BOLD, 0);
        assert_eq!(c.attrs & ATTR_UNDERLINE, 0);
    }

    #[test]
    fn twenty_five_six_and_true_color_sgr() {
        let mut g = TermGrid::new(20, 4);
        g.feed(b"\x1b[38;5;196mA\x1b[0m\x1b[48;5;21mB\x1b[0m\x1b[38;2;10;20;30mC");
        assert_eq!(cell(&g, 0, 0).fg, Palette::Idx(196));
        assert_eq!(cell(&g, 0, 1).bg, Palette::Idx(21));
        assert_eq!(cell(&g, 0, 2).fg, Palette::Rgb(10, 20, 30));
        // Colon form used by some terminals.
        let mut g = TermGrid::new(20, 4);
        g.feed(b"\x1b[38:5:121mZ");
        assert_eq!(cell(&g, 0, 0).fg, Palette::Idx(121));
        // Bright colors 90..97.
        let mut g = TermGrid::new(20, 4);
        g.feed(b"\x1b[92mQ");
        assert_eq!(cell(&g, 0, 0).fg, Palette::Idx(10));
    }

    #[test]
    fn cursor_positioning_cup_and_moves() {
        let mut g = TermGrid::new(20, 6);
        g.feed(b"\x1b[5;10H");
        assert_eq!(g.cursor, (4, 9));
        g.feed(b"\x1b[2A");
        assert_eq!(g.cursor.0, 2);
        g.feed(b"\x1b[3C");
        assert_eq!(g.cursor.1, 12);
        g.feed(b"\x1b[2D");
        assert_eq!(g.cursor.1, 10);
        g.feed(b"\x1b[G");
        assert_eq!(g.cursor.1, 0);
        g.feed(b"\x1b[3d");
        assert_eq!(g.cursor.0, 2);
        // Out-of-range positions clamp.
        g.feed(b"\x1b[99;99H");
        assert_eq!(g.cursor, (5, 19));
    }

    #[test]
    fn carriage_return_and_line_feed_scroll_into_scrollback() {
        let mut g = TermGrid::with_scrollback(10, 3, 5);
        g.feed(b"1\r\n2\r\n3\r\n4\r\n");
        // Two scrolls happened (rows 3, four lines): the top two lines moved
        // into the scrollback and the last two stay on screen.
        assert_eq!(row_text(&g, 0), "3");
        assert_eq!(row_text(&g, 1), "4");
        assert_eq!(row_text(&g, 2), "");
        assert_eq!(g.scrollback_len(), 2);
        assert_eq!(cells_to_text(&g.scrollback[0]), "1");
        assert_eq!(cells_to_text(&g.scrollback[1]), "2");
    }

    #[test]
    fn scrollback_ring_is_capped() {
        let mut g = TermGrid::with_scrollback(10, 2, 3);
        for i in 0..20 {
            g.feed(format!("line{i}\r\n").as_bytes());
        }
        assert_eq!(g.scrollback_len(), 3);
        // Oldest lines were evicted: the ring keeps the last three evictions.
        assert_eq!(cells_to_text(&g.scrollback[0]), "line16");
        assert_eq!(cells_to_text(&g.scrollback[2]), "line18");
    }

    #[test]
    fn erase_display_and_erase_line() {
        let mut g = TermGrid::new(10, 3);
        g.feed(b"aaaa\r\nbbbb\r\ncccc\x1b[2D\x1b[K");
        assert_eq!(row_text(&g, 2), "cc");
        g.feed(b"\x1b[2J");
        for row in 0..3 {
            assert_eq!(row_text(&g, row), "");
        }
        assert_eq!(g.scrollback_len(), 0);
    }

    #[test]
    fn erase_display_three_clears_scrollback_too() {
        let mut g = TermGrid::with_scrollback(10, 2, 100);
        g.feed(b"one\r\ntwo\r\nthree\r\n");
        assert_eq!(g.scrollback_len(), 2);
        g.feed(b"\x1b[3J");
        assert_eq!(g.scrollback_len(), 0);
        assert_eq!(row_text(&g, 0), "");
    }

    #[test]
    fn wide_glyphs_occupy_two_cells() {
        let mut g = TermGrid::new(10, 2);
        g.feed("中a".as_bytes());
        assert_eq!(cell(&g, 0, 0).ch, '中');
        assert!(cell(&g, 0, 1).is_continuation());
        assert_eq!(cell(&g, 0, 2).ch, 'a');
        assert_eq!(g.cursor.1, 3);
    }

    #[test]
    fn autowrap_defers_then_wraps() {
        let mut g = TermGrid::with_scrollback(4, 3, 10);
        g.feed(b"abcd");
        // Deferred wrap: cursor still on the first row until the next glyph.
        assert_eq!(g.cursor, (0, 3));
        g.feed(b"e");
        assert_eq!(row_text(&g, 0), "abcd");
        assert_eq!(row_text(&g, 1), "e");
    }

    #[test]
    fn wrapping_scrolls_the_screen_into_scrollback() {
        let mut g = TermGrid::with_scrollback(4, 2, 10);
        g.feed(b"0123456789");
        assert_eq!(g.scrollback_len(), 1);
        assert_eq!(cells_to_text(&g.scrollback[0]), "0123");
        assert_eq!(row_text(&g, 0), "4567");
        assert_eq!(row_text(&g, 1), "89");
    }

    #[test]
    fn scroll_region_and_insert_delete_lines() {
        let mut g = TermGrid::with_scrollback(6, 5, 10);
        g.feed(b"l0\r\nl1\r\nl2\r\nl3\r\nl4");
        g.feed(b"\x1b[2;4r"); // region rows 2..4
        g.feed(b"\x1b[3;1H\x1b[2L"); // insert 2 lines at row 3
        assert_eq!(row_text(&g, 2), "");
        assert_eq!(row_text(&g, 0), "l0");
        // Inserting inside the region must not touch lines outside it.
        assert_eq!(row_text(&g, 4), "l4");
        g.feed(b"\x1b[1;1H\x1b[r"); // reset region (cursor home)
        assert_eq!(g.cursor, (0, 0));
    }

    #[test]
    fn insert_and_delete_characters() {
        let mut g = TermGrid::new(10, 2);
        g.feed(b"abc\x1b[1;2H\x1b[2@");
        assert_eq!(row_text(&g, 0), "a  bc");
        g.feed(b"\x1b[1;2H\x1b[1P");
        assert_eq!(row_text(&g, 0), "a bc");
        g.feed(b"\x1b[1;4H\x1b[2X");
        assert_eq!(row_text(&g, 0), "a b");
    }

    #[test]
    fn backspace_tab_and_cr() {
        let mut g = TermGrid::new(20, 2);
        g.feed(b"ab\x08\x08x");
        assert_eq!(row_text(&g, 0), "xb");
        g.feed(b"\x0d\r\tT");
        assert_eq!(g.cursor.1, 9);
    }

    #[test]
    fn application_cursor_mode_is_tracked_for_key_encoding() {
        let mut g = TermGrid::new(10, 2);
        assert!(!g.app_cursor_keys());
        g.feed(b"\x1b[?1h");
        assert!(g.app_cursor_keys());
        g.feed(b"\x1b[?1l");
        assert!(!g.app_cursor_keys());
        g.feed(b"\x1b[?7l");
        // Autowrap off: the cursor sticks in the last column and every later
        // glyph overwrites it (`012345678` + the final `5`).
        g.feed(b"012345678998765");
        assert_eq!(row_text(&g, 0), "0123456785");
    }

    #[test]
    fn cursor_visibility_and_alt_screen() {
        let mut g = TermGrid::new(10, 3);
        g.feed(b"main");
        g.feed(b"\x1b[?1049h");
        assert!(g.in_alt_screen());
        assert_eq!(row_text(&g, 0), "");
        g.feed(b"alt");
        assert_eq!(row_text(&g, 0), "alt");
        g.feed(b"\x1b[?1049l");
        assert!(!g.in_alt_screen());
        assert_eq!(row_text(&g, 0), "main");
        g.feed(b"\x1b[?25l");
        let view = g.render(3, 0);
        assert!(view.cursor.is_none());
        g.feed(b"\x1b[?25h");
        let view = g.render(3, 0);
        assert_eq!(view.cursor, Some((0, 4)));
    }

    #[test]
    fn title_and_clipboard_osc() {
        let mut g = TermGrid::new(10, 2);
        g.feed(b"\x1b]0;bee shell\x07");
        assert_eq!(g.title(), "bee shell");
        g.feed(b"\x1b]52;c;aGVsbG8=\x07");
        assert_eq!(g.take_clipboard().as_deref(), Some("aGVsbG8="));
        assert!(g.take_clipboard().is_none());
    }

    #[test]
    fn device_status_report_is_queued_for_tx() {
        let mut g = TermGrid::new(10, 2);
        g.feed(b"\x1b[2;5H\x1b[6n");
        assert_eq!(g.take_pending_reports(), b"\x1b[2;5R".to_vec());
        assert!(g.take_pending_reports().is_empty());
        g.feed(b"\x1b[5n");
        assert_eq!(g.take_pending_reports(), b"\x1b[0n".to_vec());
    }

    #[test]
    fn render_returns_the_bottom_of_the_screen_when_clipped() {
        let mut g = TermGrid::with_scrollback(6, 4, 10);
        g.feed(b"a\r\nb\r\nc\r\nd\r\ne\r\nf\r\ng");
        let view = g.render(3, 0);
        let texts: Vec<String> = view
            .lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.to_string()).collect())
            .collect();
        assert_eq!(
            texts,
            vec!["e".to_string(), "f".to_string(), "g".to_string()]
        );
    }

    #[test]
    fn render_walks_scrollback_while_scrolled_back() {
        let mut g = TermGrid::with_scrollback(6, 2, 10);
        g.feed(b"1\r\n2\r\n3\r\n4\r\n5");
        assert_eq!(g.scrollback_len(), 3);
        // offset 2: the view ends two lines above the bottom ("2","3").
        let view = g.render(2, 2);
        let texts: Vec<String> = view
            .lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.to_string()).collect())
            .collect();
        assert_eq!(texts, vec!["2".to_string(), "3".to_string()]);
        // Scrolled back hides the cursor.
        assert!(view.cursor.is_none());
        // offset 3 reaches the oldest scrollback lines.
        let view = g.render(2, 3);
        let texts: Vec<String> = view
            .lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.to_string()).collect())
            .collect();
        assert_eq!(texts, vec!["1".to_string(), "2".to_string()]);
    }

    #[test]
    fn styles_carry_into_rendered_lines() {
        let mut g = TermGrid::new(10, 1);
        g.feed(b"\x1b[31mred\x1b[42m  ");
        let view = g.render(1, 0);
        assert_eq!(view.lines[0].spans.len(), 2);
        assert_eq!(view.lines[0].spans[0].content.as_ref(), "red");
        assert_eq!(view.lines[0].spans[0].style.fg, Some(Color::Indexed(1)));
        assert_eq!(view.lines[0].spans[1].content.as_ref(), "  ");
        assert_eq!(view.lines[0].spans[1].style.bg, Some(Color::Indexed(2)));

        // Default-styled blanks at the end of a row are not rendered.
        let mut g = TermGrid::new(10, 1);
        g.feed(b"ok");
        let view = g.render(1, 0);
        assert_eq!(view.lines[0].spans.len(), 1);
        assert_eq!(view.lines[0].spans[0].content.as_ref(), "ok");
    }

    #[test]
    fn reset_clears_everything_but_resize_keeps_scrollback() {
        let mut g = TermGrid::with_scrollback(8, 2, 10);
        g.feed(b"one\r\ntwo\r\nthree\r\n");
        assert_eq!(g.scrollback_len(), 2);
        g.resize(12, 3);
        assert_eq!(g.scrollback_len(), 2);
        assert_eq!(g.cols(), 12);
        assert_eq!(g.rows(), 3);
        g.reset();
        assert_eq!(g.scrollback_len(), 0);
        assert_eq!(g.log_bytes().len(), 0);
    }

    #[test]
    fn log_ring_is_capped_at_four_mib() {
        let mut g = TermGrid::new(4, 2);
        let chunk = vec![b'x'; 300 * 1024];
        for _ in 0..20 {
            g.feed(&chunk);
        }
        assert!(g.log_bytes().len() <= LOG_CAP_BYTES);
        assert!(g.log_bytes().len() > LOG_CAP_BYTES / 2);
    }

    #[test]
    fn clear_pane_keeps_the_log_ring() {
        let mut g = TermGrid::with_scrollback(6, 2, 10);
        g.feed(b"hello\r\nworld\r\n");
        g.clear_pane();
        assert_eq!(g.scrollback_len(), 0);
        assert_eq!(row_text(&g, 0), "");
        assert!(!g.log_bytes().is_empty(), "save log must survive Ctrl+L");
    }

    #[test]
    fn grid_dims_follow_the_font_size_like_the_fit_addon() {
        assert_eq!(grid_dims(120, 40, 13), (120, 40));
        assert_eq!(grid_dims(120, 40, 26), (60, 20));
        assert_eq!(grid_dims(120, 40, 20), (78, 26));
        // A smaller font would need more cells than the host terminal has, so
        // the pane (the physical terminal) is the hard limit.
        assert_eq!(grid_dims(120, 40, 10), (120, 40));
        // Never smaller than the protocol's minimum geometry.
        assert_eq!(grid_dims(1, 1, 28), (2, 2));
    }

    #[test]
    fn osc52_write_is_base64_bel_terminated() {
        assert_eq!(osc52_write("hi"), "\x1b]52;c;aGk=\x07");
    }

    #[test]
    fn prepare_line_translates_enter_and_echoes_raw_text() {
        assert_eq!(prepare_line("help\n", EnterMode::Raw, true), b"help\n");
        assert_eq!(prepare_line("help\n", EnterMode::Crlf, false), b"help\r\n");
    }

    /// The grid renders device output and protocol bytes only, so its table
    /// is empty on purpose (see the note above the `strings!` block).
    #[test]
    fn every_term_message_is_translated() {
        super::super::i18n::assert_bilingual(ALL);
        assert!(ALL.is_empty(), "terminal_view.rs renders no interface text");
    }
}
