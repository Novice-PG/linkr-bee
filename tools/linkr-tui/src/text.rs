//! Display-width text helpers, shared by every view.
//!
//! A terminal cell is not a character: a CJK glyph occupies two columns. Every
//! view that pads, clips or wraps text has to count *columns*, and the helpers
//! therefore live in one place instead of being re-derived — and mis-derived —
//! per view. The failures this prevents are the ones that actually shipped:
//! `{:<14}` counted characters, so the Chinese grid labels pushed the separator
//! four columns right of the English rows, and a row one column too long wrapped
//! and shifted every row below it.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// The number of terminal columns `text` occupies. A glyph the width table does
/// not know counts as zero, which keeps an odd character from inflating a row.
pub fn columns(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

/// Left-align `text` in a `width`-column field.
///
/// Padding never truncates: text already at or past `width` comes back whole,
/// because a row wider than its column is a layout problem to see rather than
/// one to hide.
pub fn pad_to(text: &str, width: usize) -> String {
    match width.checked_sub(columns(text)) {
        Some(missing) if missing > 0 => format!("{text}{}", " ".repeat(missing)),
        _ => text.to_string(),
    }
}

/// Cut `text` to at most `width` columns, marking the cut with `…`.
///
/// The ellipsis takes a column of its own, so the result is never wider than
/// `width`; a zero-column field yields nothing at all. A wide glyph that does
/// not fit in the room left is dropped rather than split, so the result cannot
/// end on the second half of a character.
pub fn clip_to(text: &str, width: usize) -> String {
    if columns(text) <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let room = width - 1;
    let mut out = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let glyph = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + glyph > room {
            break;
        }
        out.push(ch);
        used += glyph;
    }
    out.push('…');
    out
}

/// Greedy wrap to `width` display columns: break at the last whitespace when
/// there is one (which consumes it, as a terminal does), otherwise between
/// glyphs. Empty text yields nothing to paint.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut out = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;
    let mut last_space: Option<usize> = None;

    for ch in text.chars() {
        let glyph = UnicodeWidthChar::width(ch).unwrap_or(0);
        if current_width + glyph > width && !current.is_empty() {
            match last_space {
                Some(at) if at > 0 => {
                    let rest = current[at + 1..].to_string();
                    current.truncate(at);
                    out.push(std::mem::take(&mut current));
                    current = rest;
                }
                _ => {
                    out.push(std::mem::take(&mut current));
                }
            }
            current_width = columns(current.as_str());
            last_space = None;
        }
        if ch == ' ' {
            last_space = Some(current.len());
        }
        current.push(ch);
        current_width += glyph;
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Columns are columns, not characters: the whole point of the module.
    #[test]
    fn a_wide_glyph_counts_as_two_columns() {
        assert_eq!(columns("abc"), 3);
        assert_eq!(columns("中文"), 4);
        assert_eq!(columns("a中"), 3);
        assert_eq!(columns(""), 0);
    }

    /// Padding measures columns, so a CJK field lines up with an ASCII one.
    #[test]
    fn padding_measures_columns() {
        assert_eq!(pad_to("ab", 5), "ab   ");
        assert_eq!(pad_to("运行时间", 14).len(), 14 + 4);
        assert_eq!(columns(&pad_to("运行时间", 14)), 14);
        assert_eq!(columns(&pad_to("BLE 访问", 14)), 14);
        assert_eq!(columns(&pad_to("上传队列", 14)), 14);
        assert_eq!(columns(&pad_to("Firmware", 14)), 14);
    }

    /// Padding a field that is already full — or too small — changes nothing.
    #[test]
    fn padding_never_truncates() {
        assert_eq!(pad_to("abcdef", 6), "abcdef");
        assert_eq!(pad_to("abcdef", 3), "abcdef");
        assert_eq!(pad_to("", 2), "  ");
        assert_eq!(pad_to("abc", 0), "abc");
    }

    /// A clip fits the field, marks itself and never splits a wide glyph.
    #[test]
    fn clipping_fits_the_field_and_marks_the_cut() {
        assert_eq!(clip_to("short", 10), "short");
        assert_eq!(clip_to("exactly ten", 11), "exactly ten");
        assert_eq!(clip_to("abcdefghij", 4), "abc…");
        assert_eq!(columns(&clip_to("abcdefghij", 4)), 4);
        assert_eq!(clip_to("anything", 0), "");
        // A wide glyph that does not fit the room left is dropped whole.
        let two = clip_to("中文中文", 3);
        assert_eq!(columns(&two), 3);
        assert_eq!(two, "中…");
        for budget in 1..12usize {
            for text in ["abcdefghijkl", "中文中文中文", "a中b中c中", "字"] {
                let cut = clip_to(text, budget);
                assert!(
                    columns(&cut) <= budget,
                    "{text:?} at {budget} came out as {cut:?}"
                );
            }
        }
    }

    /// A run with no whitespace breaks between glyphs rather than overflowing:
    /// Chinese has no spaces to break at, which is how the old panel ran off the
    /// column instead of wrapping.
    #[test]
    fn a_run_without_spaces_breaks_between_glyphs() {
        let text = "这个提示没有空格但是太长了";
        let pieces = wrap(text, 8);
        assert!(pieces.len() > 1, "{pieces:?}");
        for piece in &pieces {
            assert!(columns(piece) <= 8, "{piece:?}");
        }
        assert_eq!(pieces.concat(), text);
    }

    /// A hard break loses nothing and reorders nothing; a row that already fits
    /// comes through untouched.
    #[test]
    fn a_hard_break_is_lossless() {
        for (text, width) in [
            ("short", 30usize),
            ("exactly ten", 11),
            ("abcdefghij", 4),
            ("中文字符测试", 3),
        ] {
            let pieces = wrap(text, width);
            assert_eq!(pieces.concat(), text, "{text:?} at {width}");
            for piece in &pieces {
                assert!(columns(piece) <= width, "{piece:?} at {width}");
            }
        }
    }

    /// Breaking at a space consumes that space, which is what a terminal does —
    /// so the pieces rejoin to the original with their separators.
    #[test]
    fn a_space_break_consumes_its_space() {
        let pieces = wrap("a b c d e f g", 4);
        assert_eq!(pieces, vec!["a b", "c d", "e f", "g"]);
        assert_eq!(pieces.join(" "), "a b c d e f g");
    }

    /// Nothing to wrap yields nothing to paint, and a zero-width column cannot
    /// wrap at all, so the text survives whole rather than being diced away.
    #[test]
    fn degenerate_widths_keep_their_text() {
        assert_eq!(wrap("", 5), Vec::<String>::new());
        assert_eq!(wrap("", 0), Vec::<String>::new());
        assert_eq!(wrap("abc", 0), vec!["abc".to_string()]);
    }
}
