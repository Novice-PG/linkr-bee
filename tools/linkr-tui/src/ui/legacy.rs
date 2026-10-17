//! The compatibility pass for the old Windows console.
//!
//! Windows has two consoles behind the same window: Windows Terminal, which
//! is a modern terminal emulator and the one the interface was designed
//! against, and conhost — the host every console application got before it,
//! still what a plain `linkr.exe` opens in. conhost is not a terminal
//! emulator in the same sense: it re-renders the screen itself, draws its own
//! scrollbar over the right edge, and decides on its own how wide a glyph is.
//!
//! Three of its decisions are what a frame painted the ordinary way runs
//! into, and each has one cheap, local answer:
//!
//! * **Synthetic bold.** conhost has no bold face for most of the fonts it
//!   can be given, so it fakes it by drawing the glyph twice, a pixel apart.
//!   On Latin text that reads as bold; on CJK it smears the strokes into the
//!   neighbouring cell — the "two characters drawn on top of each other" a
//!   screenshot showed. The pass takes `BOLD` off every cell: the console
//!   keeps its own idea of emphasis (the colour is still ours), and nothing
//!   bleeds.
//! * **Ambiguous widths.** In a console using a CJK font or code page, the
//!   characters Unicode marks "ambiguous" — box drawing, block elements,
//!   arrows, `…`, `·` — are drawn *two* cells wide, where the width tables
//!   ratatui uses put them in one. A frame is laid out by those tables, so
//!   every ambiguous glyph pushes the rest of its line a cell to the right:
//!   labels ride over their neighbours and the line's end is clipped. The
//!   pass replaces the glyphs this interface draws with one-cell stand-ins
//!   ([`STAND_INS`]) — the same shapes in ASCII, which no font renders wide.
//! * **The right edge.** The console's scrollbar lies over the window's last
//!   column or two, and the frame's right edge — the counters, the tail of a
//!   line — is painted under it. [`RIGHT_MARGIN`] columns are left unpainted
//!   for it.
//!
//! All of this is off unless the process is talking to conhost: Windows
//! Terminal sets `WT_SESSION` in its environment, and a terminal that does
//! not is a terminal that gets the frame the design asks for. The pass is a
//! pure function of the buffer, so the tests can paint one and look.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;

/// Columns kept free at the right edge under the legacy console, where the
/// scrollbar sits over the window's last cells.
///
/// Two, because conhost's scrollbar is one and the column behind it — the
/// one a wide glyph can bleed a pixel into — is the other.
pub const RIGHT_MARGIN: u16 = 2;

/// Whether this process is painting for the legacy console.
pub fn legacy_console() -> bool {
    legacy_console_for(cfg!(windows), std::env::var_os("WT_SESSION").is_some())
}

/// The rule itself, apart from the environment so it can be tested.
fn legacy_console_for(windows: bool, has_wt_session: bool) -> bool {
    windows && !has_wt_session
}

/// The area a frame is painted into.
///
/// The whole window but for the margin the scrollbar takes — and the whole
/// window when there is no legacy console to take it.
pub fn drawing_area(full: Rect, legacy: bool) -> Rect {
    if legacy {
        Rect {
            width: full.width.saturating_sub(RIGHT_MARGIN),
            ..full
        }
    } else {
        full
    }
}

/// Narrow one painted frame: stand-ins for the ambiguous glyphs, and no bold.
///
/// Painted frames only — inside the draw call, after every widget has run.
/// `--print-frame` and the tests paint through the same call with the pass
/// off, so what is inspected on a developer's machine is what the design
/// asks for, not what one console needs.
pub fn prepare(buffer: &mut Buffer) {
    for cell in &mut buffer.content {
        cell.modifier.remove(Modifier::BOLD);
        if let Some(stand_in) = narrow_symbol(cell.symbol()) {
            cell.set_symbol(stand_in);
        }
    }
}

/// The stand-in for a symbol, when there is one.
///
/// Single-glyph symbols only: a cell holding a composed sequence (an emoji
/// with a modifier, a letter with a combining mark) is not one of this
/// interface's own glyphs, and guessing at its width is exactly the mistake
/// the pass exists to undo.
fn narrow_symbol(symbol: &str) -> Option<&'static str> {
    let mut chars = symbol.chars();
    let ch = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    narrow(ch)
}

/// The stand-ins, as a table so the test below can walk it.
///
/// Every second column is one ASCII cell, and that is the whole requirement:
/// a stand-in that were wider — or itself ambiguous — would move the text it
/// was meant to keep in place. The first column is every ambiguous-width
/// glyph this interface draws, found by walking the source; a glyph the
/// console renders wide that is not here is a line that walks right on the
/// old console, and adding it is one row.
const STAND_INS: [(char, &str); 32] = [
    // Dashes and the small punctuation the layout uses as separators.
    ('\u{2014}', "-"),  // — em dash
    ('\u{2013}', "-"),  // – en dash
    ('\u{b7}', "."),    // · middle dot
    ('\u{2026}', "."),  // … ellipsis
    ('\u{d7}', "x"),    // × multiplication sign
    ('\u{b0}', "o"),    // ° degree sign
    ('\u{2248}', "~"),  // ≈ almost equal
    ('\u{201c}', "\""), // “ left double quote
    ('\u{201d}', "\""), // ” right double quote
    // Arrows, as the hints and labels that use them.
    ('\u{2190}', "<"), // ←
    ('\u{2191}', "^"), // ↑
    ('\u{2192}', ">"), // →
    ('\u{2193}', "v"), // ↓
    // The circles and diamonds that mark state.
    ('\u{25cf}', "*"), // ● filled circle
    ('\u{25c6}', "+"), // ◆ filled diamond
    ('\u{25cb}', "o"), // ○ open circle
    ('\u{25d0}', "o"), // ◐ half-filled circle
    // Bars and blocks: the progress bar and the sparkline, graded so a
    // stand-in still carries the shape the block did.
    ('\u{2581}', "_"), // ▁ one eighth
    ('\u{2582}', "_"), // ▂ one quarter
    ('\u{2583}', "="), // ▃ three eighths
    ('\u{2584}', "="), // ▄ half
    ('\u{2585}', "#"), // ▅ five eighths
    ('\u{2586}', "#"), // ▆ three quarters
    ('\u{2587}', "#"), // ▇ seven eighths
    ('\u{2588}', "#"), // █ full
    ('\u{258c}', "|"), // ▌ left half
    // Box drawing: the divider and the corners around it.
    ('\u{2500}', "-"), // ─ horizontal
    ('\u{2502}', "|"), // │ vertical
    ('\u{2503}', "|"), // ┃ heavy vertical
    ('\u{256d}', ","), // ╭ top-left arc
    ('\u{256f}', "'"), // ╯ bottom-right arc
    ('\u{2570}', "`"), // ╰ bottom-left arc
];

/// The one-cell stand-in for an ambiguous-width glyph.
///
/// ASCII returns early — it is most of every frame and none of it needs a
/// stand-in — and the table is walked only for the rest.
fn narrow(ch: char) -> Option<&'static str> {
    if ch.is_ascii() {
        return None;
    }
    STAND_INS
        .iter()
        .find(|(glyph, _)| *glyph == ch)
        .map(|(_, stand_in)| *stand_in)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Style;
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn windows_terminal_is_not_the_legacy_console() {
        assert!(!legacy_console_for(false, false), "not Windows at all");
        assert!(!legacy_console_for(false, true));
        assert!(
            legacy_console_for(true, false),
            "Windows outside Windows Terminal is the console this pass is for"
        );
        assert!(
            !legacy_console_for(true, true),
            "Windows Terminal says who it is"
        );
    }

    #[test]
    fn the_right_margin_is_kept_only_for_the_legacy_console() {
        let full = Rect::new(0, 0, 100, 30);
        assert_eq!(drawing_area(full, false), full);
        let legacy = drawing_area(full, true);
        assert_eq!(legacy.width, 100 - RIGHT_MARGIN);
        assert_eq!(legacy.height, full.height, "only the right edge gives way");
        // A window too narrow for the margin keeps what width it has.
        assert_eq!(drawing_area(Rect::new(0, 0, 1, 30), true).width, 0);
    }

    /// Every stand-in is one ASCII cell of width one: that is the whole
    /// contract, because a stand-in that were wider than its glyph would move
    /// the line it is in exactly where the glyph did.
    #[test]
    fn every_stand_in_is_one_ascii_cell() {
        for (glyph, stand_in) in STAND_INS {
            assert_eq!(
                stand_in.chars().count(),
                1,
                "{glyph:?} got {stand_in:?}, which is more than one glyph"
            );
            assert!(
                stand_in.is_ascii(),
                "{glyph:?} got {stand_in:?}, which a console could render wide"
            );
            assert_eq!(
                UnicodeWidthStr::width(stand_in),
                1,
                "{glyph:?} got {stand_in:?}, which is not one column"
            );
        }
    }

    /// The ambiguous glyphs themselves, so a glyph that *is* in the table
    /// with an ASCII spelling of its own cannot quietly leave it.
    #[test]
    fn the_tables_glyphs_are_the_ones_the_console_renders_wide() {
        for (glyph, _) in STAND_INS {
            assert!(!glyph.is_ascii(), "{glyph:?} needs no stand-in");
            assert_eq!(
                unicode_width::UnicodeWidthChar::width(glyph),
                Some(1),
                "{glyph:?} is not one column by the tables ratatui lays out with"
            );
            assert_eq!(narrow(glyph), narrow_symbol(&glyph.to_string()));
        }
    }

    #[test]
    fn prepare_narrows_the_glyphs_and_takes_the_bold_off() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 5, 1));
        buffer[(0, 0)]
            .set_symbol("│")
            .set_style(Style::default().add_modifier(Modifier::BOLD));
        buffer[(1, 0)]
            .set_symbol("中")
            .set_style(Style::default().add_modifier(Modifier::BOLD));
        buffer[(2, 0)].set_symbol("a");
        buffer[(3, 0)].set_symbol("→");
        buffer[(4, 0)].set_symbol("é");

        prepare(&mut buffer);

        assert_eq!(buffer[(0, 0)].symbol(), "|");
        assert!(
            !buffer[(0, 0)].modifier.contains(Modifier::BOLD),
            "the console would double-strike this"
        );
        // A glyph the console draws wide because it *is* wide stays: the
        // frame and the console agree about it, and the label means what it
        // says.
        assert_eq!(buffer[(1, 0)].symbol(), "中");
        assert!(
            !buffer[(1, 0)].modifier.contains(Modifier::BOLD),
            "synthetic bold on CJK is the smear the pass exists for"
        );
        assert_eq!(buffer[(2, 0)].symbol(), "a");
        assert_eq!(buffer[(3, 0)].symbol(), ">");
        // A glyph with no stand-in is left alone rather than guessed at.
        assert_eq!(buffer[(4, 0)].symbol(), "é");
    }

    #[test]
    fn a_composed_symbol_is_not_a_glyph_to_substitute() {
        // An emoji with its modifier, or a letter with a combining mark: one
        // cell, two chars, and not one of this interface's glyphs.
        assert_eq!(narrow_symbol("👍🏽"), None);
        assert_eq!(narrow_symbol("e\u{301}"), None);
        assert_eq!(narrow_symbol(""), None);
        assert_eq!(narrow_symbol("…"), Some("."));
    }
}
