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

    /// A clip is the answer to a row that is too long — and it is a *cut*, not a
    /// wrap: the row stays one row, so nothing below it moves.
    #[test]
    fn a_clip_keeps_a_row_one_row() {
        let long = "Enter sends · reboot asks first";
        let cut = clip_to(long, 29);
        assert!(columns(&cut) <= 29, "{cut:?}");
        assert!(cut.ends_with('…'), "{cut:?} is not marked as cut");
        assert!(!cut.contains('\n'), "a clip wrapped");
    }
}
