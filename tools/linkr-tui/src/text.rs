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

/// Substitute `{}` placeholders in order.
///
/// The templates are `&'static str` from the string table and the arguments are
/// runtime values, so this is the smallest thing that joins them — and it is
/// written once rather than per overlay, so a translated string with its
/// arguments in the other order still reads correctly everywhere.
///
/// A template with more placeholders than arguments keeps the leftover `{}`
/// rather than eating the text: a half-formatted message is a bug to see, not
/// one to hide.
pub fn fill(template: &str, args: &[String]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    let mut used = 0;
    while let Some(at) = rest.find("{}") {
        out.push_str(&rest[..at]);
        match args.get(used) {
            Some(value) => out.push_str(value),
            None => out.push_str("{}"),
        }
        used += 1;
        rest = &rest[at + 2..];
    }
    out.push_str(rest);
    out
}

/// A byte count as a person reads it: `512 B`, `1.0 KB`, `1.4 MB`.
///
/// Binary units, not decimal ones: the sizes here are file lengths and buffer
/// counts, which is what `df`, `ls -h` and every file manager mean by `KB`. One
/// place for it, so a notice and the row it is about cannot disagree about how
/// big the same file is.
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 7] = ["B", "KB", "MB", "GB", "TB", "PB", "EB"];
    let count = bytes as f64;
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    // Walk the units instead of stopping at one of them: the sizes on this side
    // come from a file picker, which will happily offer a disk image, and an
    // exabyte printed as megabytes is a row several screens wide.
    let mut step = count / 1024.0;
    let mut at = 1;
    while step >= 1024.0 && at + 1 < UNITS.len() {
        step /= 1024.0;
        at += 1;
    }
    format!("{:.1} {}", step, UNITS[at])
}

/// The four bars a WiFi list draws for a signal strength, strongest reading
/// first: `▂▄▆█` at -55 dBm or better, one bar fewer for each ten less.
///
/// A number on its own is a measurement; four bars are a ranking, which is
/// what a list is read for — which of these is worth joining. The thresholds
/// are the usual WiFi ones, and the reading is the firmware's own.
///
/// The empty slots are `▁`, the bottom eighth of the same block family, not
/// an underscore: an underscore sits on the baseline where a border sits, and
/// a row of them reads as a stray rule rather than as a meter with room left.
pub fn signal_bars(dbm: i32) -> &'static str {
    match dbm {
        -55.. => "▂▄▆█",
        -65..=-56 => "▂▄▆▁",
        -75..=-66 => "▂▄▁▁",
        _ => "▂▁▁▁",
    }
}

/// A `width`-cell meter, split into filled and empty halves so a caller can
/// colour them apart: `█` for the share of `used` in `total`, `░` for the
/// rest — the glyph pair ratatui's own `Gauge` draws, for the rows that live
/// inside a paragraph rather than in an area of their own.
///
/// `used` is clamped to `total`; a zero `total` is all track, because a share
/// with no denominator is not a share.
pub fn meter(used: u64, total: u64, width: usize) -> (String, String) {
    // Wide arithmetic: a hostile `used/total` off the wire must not overflow,
    // and half-up is what lets a share just past the middle fill the middle
    // cell. A zero denominator is all track, hence the 1.
    let used = u128::from(used.min(total));
    let total = u128::from(total);
    let filled = ((used * width as u128 + total / 2) / total.max(1)) as usize;
    ("█".repeat(filled), "░".repeat(width - filled))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sizes read the way a file manager writes them, and the threshold is the
    /// unit's own: 1023 bytes is bytes, 1024 is a kilobyte.
    #[test]
    fn sizes_are_human_and_binary() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1023), "1023 B");
        assert_eq!(human_bytes(1024), "1.0 KB");
        assert_eq!(human_bytes(1_500_000), "1.4 MB");
        assert_eq!(human_bytes(1024 * 1024), "1.0 MB");
        // Never wider than a short column: the whole point of the unit.
        for bytes in [0u64, 1, 999, 1023, 1024, 10_000, 1_000_000, u64::MAX] {
            assert!(
                columns(&human_bytes(bytes)) <= 12,
                "{bytes} reads as {:?}",
                human_bytes(bytes)
            );
        }
    }

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

    /// `fill` substitutes in order and leaves a template with more placeholders
    /// than arguments readable rather than eating the text.
    #[test]
    fn fill_substitutes_in_order() {
        assert_eq!(fill("a {} b {}", &["1".into(), "2".into()]), "a 1 b 2");
        assert_eq!(fill("no placeholders", &[]), "no placeholders");
        assert_eq!(fill("{} and {}", &["1".into()]), "1 and {}");
        assert_eq!(
            fill("已保存 {} 字节到 {}", &["41".into(), "x.log".into()]),
            "已保存 41 字节到 x.log"
        );
    }

    /// The bars rank a reading the way a WiFi list does — full at -55 dBm or
    /// better, one bar fewer for each ten less — and every reading gets at
    /// least one bar rather than a blank that looks like a missing device.
    #[test]
    fn signal_bars_rank_a_reading() {
        assert_eq!(signal_bars(-30), "▂▄▆█");
        assert_eq!(signal_bars(-55), "▂▄▆█");
        assert_eq!(signal_bars(-56), "▂▄▆▁");
        assert_eq!(signal_bars(-65), "▂▄▆▁");
        assert_eq!(signal_bars(-66), "▂▄▁▁");
        assert_eq!(signal_bars(-75), "▂▄▁▁");
        assert_eq!(signal_bars(-76), "▂▁▁▁");
        assert_eq!(signal_bars(-100), "▂▁▁▁");
        assert_eq!(columns(signal_bars(-54)), 4, "four columns, whichever bars");
        assert!(
            !signal_bars(-100).contains('_'),
            "an empty slot is a block, not a rule under the row"
        );
    }

    /// A meter is `width` cells whatever the share: half up, clamped to the
    /// ends, and all track when there is no denominator.
    #[test]
    fn a_meter_fills_in_proportion() {
        assert_eq!(meter(0, 16384, 10), ("".into(), "░".repeat(10)));
        assert_eq!(meter(16384, 16384, 10), ("█".repeat(10), "".into()));
        assert_eq!(
            meter(8192, 16384, 10),
            ("█".repeat(5), "░".repeat(5)),
            "half is half"
        );
        assert_eq!(
            meter(1, 3, 3),
            ("█".into(), "░░".into()),
            "a third rounds up"
        );
        assert_eq!(meter(9, 10, 10), ("█".repeat(9), "░".into()));
        // Over-full and no denominator never spill the width.
        assert_eq!(meter(20_000, 16384, 10), ("█".repeat(10), "".into()));
        assert_eq!(meter(5, 0, 4), ("".into(), "░".repeat(4)));
        assert_eq!(
            meter(u64::MAX, u64::MAX, 10),
            ("█".repeat(10), "".into()),
            "the widest readings still add up"
        );
        // The two halves always add up to exactly the width asked for.
        for (used, total) in [(0u64, 1u64), (1, 7), (999, 1000), (5, 2), (0, 0)] {
            for width in [0usize, 1, 3, 10] {
                let (fill, track) = meter(used, total, width);
                assert_eq!(columns(&fill) + columns(&track), width, "{used}/{total}");
            }
        }
    }
}
