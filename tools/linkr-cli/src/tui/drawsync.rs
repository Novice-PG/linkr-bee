//! Frame positions that stay correct on a console which disagrees with ratatui
//! about how many columns a glyph occupies.
//!
//! `Backend::draw` merges a cell into the previous write run whenever its
//! column is exactly `previous + 1`; it never looks at how wide the symbol it
//! has just printed is. That is sound only if every symbol advances the cursor
//! by one column. Two real consoles break the assumption, and both surface as
//! the Windows TUI's permanent smearing (F2):
//!
//! 1. **legacy conhost counts CJK as one column.** `Buffer::diff` skips the
//!    shadow column of a wide glyph — the glyph is supposed to cover it — so on
//!    such a console that cell is *never* written: whatever an earlier frame
//!    left there stays for the rest of the session. `Ctrl+L` looks like a fix
//!    only because `Terminal::clear()` blanks the viewport first.
//! 2. **the diff does emit that trailing cell sometimes** (a VS16 emoji
//!    presentation, or a styled wide glyph being replaced by narrower text).
//!    In a two-column console it is then printed one column right of where it
//!    belongs, and every later cell of the same run follows it.
//!
//! [`reframe`] repairs both by rewriting **positions only**: colours and
//! attributes stay inside the backend this module wraps, so the styling path
//! is untouched. A frame that needs no rewriting is handed to the inner
//! backend exactly as it arrived.

use ratatui::backend::{Backend, ClearType, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};
use unicode_width::UnicodeWidthStr;

/// How many columns a double-width glyph takes on this console.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CjkWidth {
    /// Two columns — what ratatui itself assumes (xterm, Windows Terminal).
    Wide,
    /// One column — legacy conhost under cmd/PowerShell counts CJK this way.
    Narrow,
}

/// What the startup probe found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Probe {
    /// Measured on this console, or forced with `LINKR_TUI_CJK`.
    Measured(CjkWidth),
    /// No answer: draw the way ratatui assumes, two columns.
    Assumed,
}

impl Probe {
    /// The width to draw with.
    pub fn width(self) -> CjkWidth {
        match self {
            Probe::Measured(width) => width,
            Probe::Assumed => CjkWidth::Wide,
        }
    }
}

/// A `LINKR_TUI_CJK` value, if it names a width.
///
/// The override exists so a Windows console can be A/B tested without a
/// rebuild: `LINKR_TUI_CJK=narrow linkr --tui` forces the shadow backfill on,
/// `LINKR_TUI_CJK=wide` forces it off.
pub fn forced_width(value: &str) -> Option<CjkWidth> {
    match value.trim().to_ascii_lowercase().as_str() {
        "narrow" | "1" => Some(CjkWidth::Narrow),
        "wide" | "2" => Some(CjkWidth::Wide),
        _ => None,
    }
}

/// Ask the console how wide a CJK glyph is.
///
/// Parks the cursor on a scratch row, prints four CJK glyphs and reads the
/// cursor back: eight columns means two per glyph, four means one. On Windows
/// `crossterm::cursor::position()` is a plain `GetConsoleScreenBufferInfo`
/// call, so no DSR round trip is made and no keystroke in flight can be
/// swallowed. The scratch row is wiped a moment later by the
/// `Terminal::clear()` that starts the TUI.
///
/// POSIX consoles are not probed: they already report two columns, and the DSR
/// query there would read from the very stdin the event loop is about to use.
pub fn probe_cjk_width() -> Probe {
    if let Some(width) = std::env::var("LINKR_TUI_CJK")
        .ok()
        .and_then(|value| forced_width(&value))
    {
        return Probe::Measured(width);
    }
    probe_console()
}

/// How many CJK glyphs the scratch row prints (4 glyphs = 8 columns wide, 4 narrow).
///
/// Only `probe_console` on Windows spends them; everywhere else they exist so
/// the classification test can tie the column count back to the string.
#[cfg_attr(not(windows), allow(dead_code))]
const PROBE_GLYPHS: &str = "中中中中";

/// The scratch row, 0-based: far enough down to leave the prompt alone.
#[cfg_attr(not(windows), allow(dead_code))]
const PROBE_ROW: u16 = 6;

/// The columns four CJK glyphs advance the cursor by, or `None` when that is
/// neither 8 (two columns each) nor 4 (one column each).
pub fn classify(advance: u16) -> Option<CjkWidth> {
    match advance {
        8 => Some(CjkWidth::Wide),
        4 => Some(CjkWidth::Narrow),
        _ => None,
    }
}

#[cfg(windows)]
fn probe_console() -> Probe {
    use crossterm::{cursor, queue, style::Print};
    use std::io::Write as _;

    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    let glyph_cols = 2 * PROBE_GLYPHS.chars().count() as u16;
    if cols < glyph_cols + 8 || rows < 2 {
        return Probe::Assumed;
    }
    let row = PROBE_ROW.min(rows - 1);
    let mut out = std::io::stdout();
    if queue!(out, cursor::MoveTo(0, row), Print(PROBE_GLYPHS)).is_err() {
        return Probe::Assumed;
    }
    if out.flush().is_err() {
        return Probe::Assumed;
    }
    // conhost applies its VT writes synchronously; the pause only keeps a very
    // slow console from answering about whatever was printed before us.
    std::thread::sleep(std::time::Duration::from_millis(15));
    match cursor::position() {
        Ok((x, _)) => match classify(x) {
            Some(width) => Probe::Measured(width),
            None => Probe::Assumed,
        },
        Err(_) => Probe::Assumed,
    }
}

#[cfg(not(windows))]
fn probe_console() -> Probe {
    Probe::Assumed
}

/// How many columns the console is asked to give this symbol.
fn glyph_width(cell: &Cell) -> usize {
    UnicodeWidthStr::width(cell.symbol())
}

/// True when `cells[index]` sits in the shadow column of the glyph before it.
fn is_shadow(cells: &[(u16, u16, &Cell)], index: usize) -> bool {
    let Some(&(x, y, _)) = cells.get(index) else {
        return false;
    };
    let Some(&prev) = index.checked_sub(1).and_then(|i| cells.get(i)) else {
        return false;
    };
    let (px, py, prev_cell) = prev;
    py == y && glyph_width(prev_cell) >= 2 && x == px.saturating_add(1)
}

/// True when [`reframe`] would have to rewrite this frame.
fn needs_reframe(cells: &[(u16, u16, &Cell)], width: CjkWidth) -> bool {
    for (index, &(x, y, cell)) in cells.iter().enumerate() {
        match width {
            // Two-column console: a trailing cell right after a wide glyph is
            // a column the glyph already covers, and `draw` would print it one
            // column too far right.
            CjkWidth::Wide if glyph_width(cell) >= 2 && is_shadow(cells, index + 1) => {
                return true;
            }
            // One-column console: a wide glyph whose shadow column the diff
            // skipped will never be painted by anyone.
            CjkWidth::Narrow
                if glyph_width(cell) >= 2
                    && !cells
                        .get(index + 1)
                        .is_some_and(|&(nx, ny, _)| ny == y && nx == x.saturating_add(1)) =>
            {
                return true;
            }
            _ => {}
        }
    }
    false
}

/// The cells the wrapped backend should receive for this frame, or `None` when
/// the frame already reads correctly and can be passed through untouched.
///
/// * [`CjkWidth::Narrow`] — emit the skipped shadow column as a space in the
///   glyph's own style. The space is what keeps `draw`'s one-column run
///   arithmetic honest as well: after printing it the cursor sits exactly
///   where the next cell expects to be.
/// * [`CjkWidth::Wide`] — drop a trailing cell emitted straight after a wide
///   glyph. Dropping is what the diff meant to do in the first place; keeping
///   it is what shifts the rest of the run.
pub fn reframe(cells: &[(u16, u16, &Cell)], width: CjkWidth) -> Option<Vec<(u16, u16, Cell)>> {
    if !needs_reframe(cells, width) {
        return None;
    }
    let mut out: Vec<(u16, u16, Cell)> = Vec::with_capacity(cells.len() + 4);
    for (index, &(x, y, cell)) in cells.iter().enumerate() {
        if width == CjkWidth::Wide && is_shadow(cells, index) {
            continue;
        }
        out.push((x, y, cell.clone()));
        if width != CjkWidth::Narrow || glyph_width(cell) < 2 {
            continue;
        }
        let shadow_follows = cells
            .get(index + 1)
            .is_some_and(|&(nx, ny, _)| ny == y && nx == x.saturating_add(1));
        if !shadow_follows {
            let mut shadow = cell.clone();
            shadow.set_symbol(" ");
            out.push((x.saturating_add(1), y, shadow));
        }
    }
    Some(out)
}

/// Wraps the real backend and repairs frame positions before they reach it.
///
/// Everything except [`Backend::draw`] is delegated unchanged, so cursor
/// handling, clearing, sizing and scrolling behave exactly as before.
pub struct SyncBackend<B> {
    inner: B,
    cjk: CjkWidth,
}

impl<B> SyncBackend<B> {
    /// Wrap `inner`, drawing frames for a console with the given glyph width.
    pub fn new(inner: B, cjk: CjkWidth) -> Self {
        Self { inner, cjk }
    }

    /// The glyph width this backend draws for.
    pub fn cjk_width(&self) -> CjkWidth {
        self.cjk
    }
}

impl<B: Backend> Backend for SyncBackend<B> {
    type Error = B::Error;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        let cells: Vec<(u16, u16, &Cell)> = content.collect();
        match reframe(&cells, self.cjk) {
            // Nothing to repair: hand the frame over as it arrived.
            None => self.inner.draw(cells.into_iter()),
            Some(owned) => self
                .inner
                .draw(owned.iter().map(|&(x, y, ref cell)| (x, y, cell))),
        }
    }

    fn append_lines(&mut self, n: u16) -> Result<(), Self::Error> {
        self.inner.append_lines(n)
    }

    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        self.inner.hide_cursor()
    }

    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        self.inner.show_cursor()
    }

    fn get_cursor_position(&mut self) -> Result<Position, Self::Error> {
        self.inner.get_cursor_position()
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> Result<(), Self::Error> {
        self.inner.set_cursor_position(position)
    }

    fn clear(&mut self) -> Result<(), Self::Error> {
        self.inner.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> Result<(), Self::Error> {
        self.inner.clear_region(clear_type)
    }

    fn size(&self) -> Result<Size, Self::Error> {
        self.inner.size()
    }

    fn window_size(&mut self) -> Result<WindowSize, Self::Error> {
        self.inner.window_size()
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        self.inner.flush()
    }

    // `scroll_region_up`/`scroll_region_down` belong to the optional
    // `scrolling-regions` trait feature: ratatui does not enable it for us, so
    // the default implementations above (no-op) are what the terminal gets.
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::{Color, Modifier};

    /// A frame at consecutive columns, each symbol advancing by its own width
    /// — exactly the shape `Buffer::diff` produces, where a wide glyph's
    /// shadow column is never in the list. Every cell is styled, so a
    /// synthesized shadow can be told apart from a plain space.
    fn frame(symbols: &[&str]) -> Vec<(u16, u16, Cell)> {
        let mut column = 0u16;
        symbols
            .iter()
            .map(|symbol| {
                let mut cell = Cell::new(" ");
                cell.set_symbol(symbol);
                cell.bg = Color::Blue;
                cell.modifier = Modifier::BOLD;
                let at = column;
                column += UnicodeWidthStr::width(*symbol) as u16;
                (at, 0, cell)
            })
            .collect()
    }

    /// A frame with columns placed by hand: the shape the diff emits only from
    /// its forced path, where the trailing column of a wide glyph *is* yielded.
    fn hand(positions: &[(u16, &str)]) -> Vec<(u16, u16, Cell)> {
        positions
            .iter()
            .map(|&(x, symbol)| {
                let mut cell = Cell::new(" ");
                cell.set_symbol(symbol);
                (x, 0, cell)
            })
            .collect()
    }

    /// Borrowed view of an owned frame — the type `reframe` takes.
    fn borrowed(cells: &[(u16, u16, Cell)]) -> Vec<(u16, u16, &Cell)> {
        cells.iter().map(|&(x, y, ref c)| (x, y, c)).collect()
    }

    /// The sequence `Backend::draw` emits for `cells`: a `MoveTo` whenever the
    /// column is not `previous + 1`, otherwise plain continuation — and, for
    /// each symbol, where a console that gives a wide glyph `wide_columns`
    /// columns would really put it.
    fn replay(cells: &[(u16, u16, Cell)], wide_columns: u16) -> Vec<(u16, u16, String)> {
        let mut last_pos: Option<(u16, u16)> = None;
        let mut cursor = (0u16, 0u16);
        let mut painted = Vec::new();
        for &(x, y, ref cell) in cells {
            let continues = matches!(last_pos, Some((px, py)) if x == px + 1 && y == py);
            if !continues {
                cursor = (x, y);
            }
            let symbol = cell.symbol().to_string();
            if !symbol.is_empty() {
                painted.push((cursor.0, cursor.1, symbol));
            }
            let step = if UnicodeWidthStr::width(cell.symbol()) >= 2 {
                wide_columns
            } else {
                1
            };
            cursor = (cursor.0.saturating_add(step), cursor.1);
            last_pos = Some((x, y));
        }
        painted
    }

    fn columns(cells: &[(u16, u16, Cell)]) -> Vec<(u16, &str)> {
        cells.iter().map(|&(x, _, ref c)| (x, c.symbol())).collect()
    }

    #[test]
    fn plain_frames_pass_through_untouched() {
        let cells = frame(&["S", "e", "t", "u"]);
        assert!(reframe(&borrowed(&cells), CjkWidth::Wide).is_none());
        assert!(reframe(&borrowed(&cells), CjkWidth::Narrow).is_none());
    }

    #[test]
    fn a_two_column_console_leaves_cjk_frames_alone() {
        // The everyday case on Linux and Windows Terminal: the diff skips the
        // shadow column, the glyph covers it, nothing needs rewriting.
        let cells = frame(&["串", "口", ":", " ", "A"]);
        assert_eq!(
            columns(&cells),
            vec![(0, "串"), (2, "口"), (4, ":"), (5, " "), (6, "A")]
        );
        assert!(reframe(&borrowed(&cells), CjkWidth::Wide).is_none());
    }

    #[test]
    fn a_one_column_console_paints_the_shadow_the_diff_skipped() {
        // ratatui skips column 1 because 中 is supposed to cover it. A console
        // that measures 中 as one column would never write that column, and
        // whatever an earlier frame left there would stay forever.
        let cells = frame(&["中", "文"]);
        let out = reframe(&borrowed(&cells), CjkWidth::Narrow)
            .expect("a one-column console needs the shadow painted");
        assert_eq!(
            columns(&out),
            vec![(0, "中"), (1, " "), (2, "文"), (3, " ")],
            "each wide glyph gets a shadow space"
        );
        assert_eq!(
            out[1].2.bg,
            Color::Blue,
            "the shadow keeps the glyph's own style"
        );
        assert_eq!(out[1].2.modifier, Modifier::BOLD);
    }

    #[test]
    fn a_shadow_the_diff_already_emitted_is_not_doubled() {
        let cells = hand(&[(0, "中"), (1, " "), (2, "x")]);
        assert!(reframe(&borrowed(&cells), CjkWidth::Narrow).is_none());
    }

    #[test]
    fn a_two_column_console_drops_the_trailing_cell() {
        // The forced path: the diff yields the glyph *and* the column it covers.
        let cells = hand(&[(0, "😀"), (1, " "), (2, "x")]);
        let out = reframe(&borrowed(&cells), CjkWidth::Wide)
            .expect("the trailing cell must not reach draw");
        assert_eq!(columns(&out), vec![(0, "😀"), (2, "x")]);
    }

    #[test]
    fn nothing_drifts_on_a_one_column_console() {
        let cells = frame(&["传", "输", "方", "式"]);
        let borrowed = borrowed(&cells);
        let rewired = reframe(&borrowed, CjkWidth::Narrow).expect("CJK needs rewriting");
        let painted = replay(&rewired, 1);
        for &(x, _, ref cell) in &cells {
            assert!(
                painted
                    .iter()
                    .any(|&(px, py, ref s)| px == x && py == 0 && s == cell.symbol()),
                "{:?} must be painted at column {x}; painted: {painted:?}",
                cell.symbol(),
            );
        }
    }

    #[test]
    fn nothing_drifts_on_a_two_column_console() {
        // Before the rewrite the trailing cell prints one column right and
        // takes the rest of the run with it — that is F2, frame by frame.
        let cells = hand(&[(0, "😀"), (1, " "), (2, "a"), (3, "b")]);
        let borrowed = borrowed(&cells);
        let rewired = reframe(&borrowed, CjkWidth::Wide).expect("trailing cell must go");
        assert_eq!(
            replay(&rewired, 2),
            vec![
                (0, 0, "😀".to_string()),
                (2, 0, "a".to_string()),
                (3, 0, "b".to_string()),
            ]
        );
        assert_eq!(
            replay(&cells, 2),
            vec![
                (0, 0, "😀".to_string()),
                (2, 0, " ".to_string()),
                (3, 0, "a".to_string()),
                (4, 0, "b".to_string()),
            ],
            "unrewritten: the space lands at column 2 and a at 3"
        );
    }

    #[test]
    fn the_real_diff_leaves_the_shadow_to_us() {
        // Straight from ratatui, not from this file's idea of it: two CJK
        // glyphs in an empty row. The diff yields the glyphs and skips the
        // columns they cover, which is exactly the cell a one-column console
        // would otherwise never be shown again.
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;

        let area = Rect::new(0, 0, 8, 1);
        let prev = Buffer::empty(area);
        let mut next = Buffer::empty(area);
        next.set_string(0, 0, "中文", Color::Blue);

        let diff = prev.diff(&next);
        let yielded: Vec<u16> = diff.iter().map(|&(x, _, _)| x).collect();
        assert_eq!(
            yielded,
            vec![0, 2],
            "columns 1 and 3 are the skipped shadows"
        );

        let out = reframe(&diff, CjkWidth::Narrow).expect("a one-column console needs both");
        assert_eq!(
            columns(&out),
            vec![(0, "中"), (1, " "), (2, "文"), (3, " ")]
        );
        // …and a two-column console keeps receiving exactly what it was sent.
        assert!(reframe(&diff, CjkWidth::Wide).is_none());
    }

    #[test]
    fn a_frame_is_never_rewritten_twice() {
        let cells = frame(&["中", "文"]);
        let input = borrowed(&cells);
        let once = reframe(&input, CjkWidth::Narrow).expect("first pass rewrites");
        assert!(reframe(&borrowed(&once), CjkWidth::Narrow).is_none());
    }

    #[test]
    fn the_probe_reads_eight_as_wide_and_four_as_narrow() {
        assert_eq!(classify(8), Some(CjkWidth::Wide));
        assert_eq!(classify(4), Some(CjkWidth::Narrow));
        assert_eq!(classify(0), None, "wrapped or clamped: not an answer");
        assert_eq!(classify(6), None);
        assert_eq!(
            classify(2 * PROBE_GLYPHS.chars().count() as u16),
            Some(CjkWidth::Wide),
            "four glyphs, two columns each"
        );
    }

    #[test]
    fn the_override_names_a_width() {
        assert_eq!(forced_width("narrow"), Some(CjkWidth::Narrow));
        assert_eq!(forced_width(" WIDE "), Some(CjkWidth::Wide));
        assert_eq!(forced_width("1"), Some(CjkWidth::Narrow));
        assert_eq!(forced_width("banana"), None);
    }

    #[test]
    fn an_unanswered_probe_draws_the_way_ratatui_assumes() {
        assert_eq!(Probe::Assumed.width(), CjkWidth::Wide);
        assert_eq!(
            Probe::Measured(CjkWidth::Narrow).width(),
            CjkWidth::Narrow,
            "a measured narrow console must stay narrow"
        );
    }
}
