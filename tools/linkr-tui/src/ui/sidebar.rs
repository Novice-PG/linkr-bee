//! The sidebar: connection block, view list, quick sends and the watch list.
//!
//! Two layers, deliberately: [`rows`] builds the plain content and marks what
//! each row *is*, [`lines`] colours those rows from the theme. Tests assert on
//! `rows` for structure and on `lines` for colour, so a layout change cannot
//! hide behind a style change.
//!
//! The width helpers live in [`crate::text`] because every view needs the same
//! column arithmetic — a second copy is how the two drift apart.

use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use crate::app::App;
use crate::event::View;
use crate::text::{pad_to, wrap};
use crate::theme::Theme;

/// What a sidebar row means, which decides how it is painted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowStyle {
    /// A section header (`Connection`, `Views`, …).
    Header,
    /// A plain label (`Device: …`).
    Label,
    /// A dimmed hint line.
    Hint,
    /// An action the user can run, not currently selected.
    Action,
    /// The action the sidebar cursor is on: the accent chip.
    Selected,
    /// The view that is showing in the centre column.
    ActiveView,
    /// A shortcut that asks for confirmation first.
    Dangerous,
}

/// One sidebar row: its text and what it means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub text: String,
    pub style: RowStyle,
}

impl Row {
    fn new(text: impl Into<String>, style: RowStyle) -> Row {
        Row {
            text: text.into(),
            style,
        }
    }
}

/// The selectable rows, in cursor order. The sidebar cursor indexes this list,
/// so highlighting and navigation can never disagree with what is drawn.
pub fn targets(app: &App) -> Vec<String> {
    let mut out = vec![format!(
        "{}: {}",
        app.strings().transport_label,
        app.transport_text()
    )];
    out.push(app.strings().connect.to_string());
    out.push(app.strings().switch_device.to_string());
    for view in View::ALL {
        out.push(app.lang.view_name(view).to_string());
    }
    for quick in &app.quick_send {
        out.push(quick.command.clone());
    }
    out
}

/// The whole sidebar, top to bottom.
pub fn rows(app: &App) -> Vec<Row> {
    let s = app.strings();
    let mut rows = Vec::new();
    let mut cursor = 0usize;
    let mut next_cursor = || {
        let here = cursor;
        cursor += 1;
        here
    };
    let selected = |index: usize| app.sidebar_cursor == index;

    // --- Connection -------------------------------------------------------
    rows.push(Row::new(s.section_connection, RowStyle::Header));

    let transport_index = next_cursor();
    rows.push(Row::new(
        format!("{}: {}", s.transport_label, app.transport_text()),
        if selected(transport_index) {
            RowStyle::Selected
        } else {
            RowStyle::Action
        },
    ));
    rows.push(Row::new(
        format!("{}: {}", s.device_label, app.device_name),
        RowStyle::Label,
    ));
    for label in [s.connect, s.switch_device] {
        let index = next_cursor();
        rows.push(Row::new(
            label,
            if selected(index) {
                RowStyle::Selected
            } else {
                RowStyle::Action
            },
        ));
    }

    // --- Views ------------------------------------------------------------
    rows.push(Row::new(s.section_views, RowStyle::Header));
    for view in View::ALL {
        let index = next_cursor();
        let style = if selected(index) {
            RowStyle::Selected
        } else if view == app.view {
            RowStyle::ActiveView
        } else {
            RowStyle::Action
        };
        rows.push(Row::new(app.lang.view_name(view), style));
    }

    // --- Quick send -------------------------------------------------------
    rows.push(Row::new(s.section_quick_send, RowStyle::Header));
    rows.push(Row::new(s.quick_send_hint, RowStyle::Hint));
    for quick in &app.quick_send {
        let index = next_cursor();
        let style = if selected(index) {
            RowStyle::Selected
        } else if quick.dangerous {
            RowStyle::Dangerous
        } else {
            RowStyle::Action
        };
        rows.push(Row::new(quick.command.clone(), style));
    }

    // --- Watch ------------------------------------------------------------
    rows.push(Row::new(s.section_watch, RowStyle::Header));
    if app.watch.is_empty() {
        rows.push(Row::new(s.watch_empty, RowStyle::Hint));
    } else {
        for finding in &app.watch {
            rows.push(Row::new(finding.clone(), RowStyle::Action));
        }
    }

    rows
}

/// The sidebar painted: the cursor row first, so a selected row's chip covers
/// the whole line, and every row indented under its section header.
///
/// `width` is the sidebar's inner width. The selected row is padded to it, so
/// the chip reads as a button rather than as coloured text; the padding counts
/// display columns, not characters, so a CJK row is padded correctly.
pub fn lines(app: &App, theme: &Theme, width: u16) -> Vec<Line<'static>> {
    let cursor_marker = "▶ ";
    let indent = "  ";
    let width = width as usize;
    rows(app)
        .into_iter()
        .map(|row| {
            let (text, style) = match row.style {
                RowStyle::Header => (row.text, theme.section_header()),
                RowStyle::Label => (format!("{indent}{}", row.text), theme.text()),
                RowStyle::Hint => (format!("{indent}{}", row.text), theme.muted()),
                RowStyle::Action => (format!("{indent}{}", row.text), theme.text()),
                RowStyle::Selected => {
                    let marked = format!("{cursor_marker}{}", row.text);
                    (pad_to(&marked, width), theme.selected())
                }
                RowStyle::ActiveView => (
                    format!("{indent}{}", row.text),
                    theme.text().add_modifier(Modifier::BOLD),
                ),
                RowStyle::Dangerous => (format!("{indent}{}", row.text), theme.danger()),
            };
            Line::from(Span::styled(text, style))
        })
        .collect()
}

/// The sidebar as the frame paints it: [`lines`], with any row too wide for the
/// column broken onto the next line. The design's English hint is the case that
/// needs it — `Enter sends · reboot asks first` wraps with `first` on its own
/// row — and a Chinese row has no spaces to break at, so a run longer than the
/// column breaks between glyphs instead of running under the centre column.
pub fn fitted(app: &App, theme: &Theme, width: u16) -> Vec<Line<'static>> {
    lines(app, theme, width)
        .into_iter()
        .flat_map(|line| match line.spans.into_iter().next() {
            Some(span) => wrap(&span.content, width as usize)
                .into_iter()
                .map(|piece| Line::from(Span::styled(piece, span.style)))
                .collect::<Vec<_>>(),
            None => Vec::new(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Lang;
    use crate::ui::SIDEBAR_WIDTH;
    use unicode_width::UnicodeWidthStr;

    fn app_en() -> App {
        App {
            lang: Lang::En,
            ..App::default()
        }
    }

    /// The rows appear in the designed order: four sections, each header
    /// followed by its content, with the connection block first.
    #[test]
    fn the_sections_appear_in_design_order() {
        let app = app_en();
        let rows = rows(&app);
        let headers: Vec<&str> = rows
            .iter()
            .filter(|r| r.style == RowStyle::Header)
            .map(|r| r.text.as_str())
            .collect();
        assert_eq!(headers, vec!["Connection", "Views", "Quick send", "Watch"]);
        // The first row of the list is the first header.
        assert_eq!(rows[0].style, RowStyle::Header);
    }

    /// Every view is listed, under the Views header, and the active one is
    /// marked so the sidebar and the centre column agree.
    #[test]
    fn the_active_view_is_marked() {
        let mut app = app_en();
        app.view = View::Network;
        let rows = rows(&app);
        let active: Vec<&str> = rows
            .iter()
            .filter(|r| r.style == RowStyle::ActiveView)
            .map(|r| r.text.as_str())
            .collect();
        assert_eq!(active, vec!["Network"]);
        for view in View::ALL {
            assert!(
                rows.iter().any(|r| r.text == app.lang.view_name(view)),
                "{view:?} missing from the sidebar"
            );
        }
    }

    /// The cursor lands on exactly one row, and only on a selectable one — a
    /// header can never be the cursor's target.
    #[test]
    fn the_cursor_marks_exactly_one_selectable_row() {
        let mut app = app_en();
        let selectable = targets(&app).len();
        for index in 0..selectable {
            app.sidebar_cursor = index;
            let rows = rows(&app);
            let selected: Vec<&Row> = rows
                .iter()
                .filter(|r| r.style == RowStyle::Selected)
                .collect();
            assert_eq!(
                selected.len(),
                1,
                "cursor {index} marked {} rows",
                selected.len()
            );
            assert_eq!(selected[0].text, targets(&app)[index]);
        }
    }

    /// A cursor past the end marks nothing rather than panicking: a stale
    /// cursor after the quick-send list shrinks must not crash a frame.
    #[test]
    fn a_cursor_past_the_end_marks_nothing() {
        let mut app = app_en();
        app.sidebar_cursor = targets(&app).len() + 7;
        let rows = rows(&app);
        assert_eq!(
            rows.iter()
                .filter(|r| r.style == RowStyle::Selected)
                .count(),
            0
        );
    }

    /// The default cursor is on the transport row — the chip the design shows
    /// in the first frame.
    #[test]
    fn the_default_cursor_is_on_the_transport_row() {
        let mut app = app_en();
        app.transport_locked = true;
        let rows = rows(&app);
        let selected = rows.iter().find(|r| r.style == RowStyle::Selected).unwrap();
        assert_eq!(selected.text, "Transport: BLE (locked)");
    }

    /// The watch section shows its empty state as a hint, and the findings
    /// replace it once there are any.
    #[test]
    fn the_watch_section_shows_either_findings_or_its_empty_state() {
        let mut app = app_en();
        assert!(rows(&app)
            .iter()
            .any(|r| r.text == "no findings yet" && r.style == RowStyle::Hint));
        app.watch = vec!["panic @ 0x400d1234".to_string()];
        let rows = rows(&app);
        assert!(!rows.iter().any(|r| r.text == "no findings yet"));
        assert!(rows.iter().any(|r| r.text == "panic @ 0x400d1234"));
    }

    /// The guarded shortcut is coloured as dangerous; the plain ones are not.
    #[test]
    fn only_guarded_shortcuts_are_dangerous() {
        let app = app_en();
        let rows = rows(&app);
        let dangerous: Vec<&str> = rows
            .iter()
            .filter(|r| r.style == RowStyle::Dangerous)
            .map(|r| r.text.as_str())
            .collect();
        assert_eq!(dangerous, vec!["reboot"]);
    }

    /// The painted rows carry the theme: headers are accent, hints muted, the
    /// cursor row is the accent chip. This is the "no literal colours in the
    /// view" rule, enforced.
    #[test]
    fn painted_rows_take_their_colours_from_the_theme() {
        let app = app_en();
        let theme = Theme::DARK;
        let painted = lines(&app, &theme, SIDEBAR_WIDTH);
        let rows = rows(&app);
        assert_eq!(painted.len(), rows.len());
        for (row, line) in rows.iter().zip(&painted) {
            let span = &line.spans[0];
            match row.style {
                RowStyle::Header => assert_eq!(span.style.fg, Some(theme.accent)),
                RowStyle::Hint => assert_eq!(span.style.fg, Some(theme.muted)),
                RowStyle::Selected => {
                    assert_eq!(span.style.bg, Some(theme.accent));
                    assert_eq!(span.style.fg, Some(theme.accent_ink));
                }
                RowStyle::Dangerous => assert_eq!(span.style.fg, Some(theme.danger)),
                _ => assert_eq!(span.style.fg, Some(theme.text)),
            }
        }
    }

    /// The other theme paints the same rows with its own values — the switch is
    /// a value change, not a code path.
    #[test]
    fn the_light_theme_paints_the_same_rows_differently() {
        let app = app_en();
        let dark = lines(&app, &Theme::DARK, SIDEBAR_WIDTH);
        let light = lines(&app, &Theme::LIGHT, SIDEBAR_WIDTH);
        assert_eq!(dark.len(), light.len());
        let header_dark = dark[0].spans[0].style.fg;
        let header_light = light[0].spans[0].style.fg;
        assert_eq!(header_dark, Some(Theme::DARK.accent));
        assert_eq!(header_light, Some(Theme::LIGHT.accent));
        assert_ne!(header_dark, header_light);
    }

    /// A selected row starts with the marker and the rest are indented: the
    /// chip has to cover the cursor mark, so the mark is part of the row text.
    #[test]
    fn the_cursor_row_carries_the_marker() {
        let app = app_en();
        let painted = lines(&app, &Theme::DARK, SIDEBAR_WIDTH);
        let marked: Vec<&str> = painted
            .iter()
            .filter(|l| l.spans[0].content.starts_with("▶ "))
            .map(|l| l.spans[0].content.as_ref())
            .collect();
        assert_eq!(marked.len(), 1);
        assert!(marked[0].starts_with("▶ Transport:"));
    }

    /// Wide (CJK) text keeps its indent and never overflows the column — the
    /// class of bug that shifted the old panel: a two-column glyph counted as
    /// one made the chip and every row after it sit wrong.
    #[test]
    fn wide_text_keeps_its_indent_and_never_overflows() {
        for lang in [Lang::Zh, Lang::En] {
            let app = App {
                lang,
                ..App::default()
            };
            for (row, line) in rows(&app)
                .iter()
                .zip(lines(&app, &Theme::DARK, SIDEBAR_WIDTH))
            {
                let text = line.spans[0].content.to_string();
                if row.style == RowStyle::Header {
                    assert!(!text.starts_with(' '), "header {text:?} is indented");
                } else if row.style == RowStyle::Selected {
                    // The cursor row carries the marker where the indent goes,
                    // so the chip starts at column zero.
                    assert!(
                        text.starts_with("▶ "),
                        "cursor row {text:?} lost its marker"
                    );
                } else {
                    assert!(text.starts_with("  "), "row {text:?} lost its indent");
                }
            }

            // What is painted never overflows, in either language: a row too
            // wide for the column is wrapped, not spilled.
            for line in fitted(&app, &Theme::DARK, SIDEBAR_WIDTH) {
                let text = line.spans[0].content.to_string();
                let width = UnicodeWidthStr::width(text.as_str());
                assert!(
                    width <= SIDEBAR_WIDTH as usize,
                    "{lang:?} painted row {text:?} is {width} columns wide"
                );
            }
        }

        // The Chinese rows are genuinely wide, so the width accounting above is
        // exercised rather than passing on ASCII by accident.
        let zh = App {
            lang: Lang::Zh,
            ..App::default()
        };
        let painted = lines(&zh, &Theme::DARK, SIDEBAR_WIDTH);
        let view_row = painted
            .iter()
            .find(|l| l.spans[0].content.contains("串口终端"))
            .expect("the serial terminal row");
        let text = view_row.spans[0].content.to_string();
        assert!(
            UnicodeWidthStr::width(text.as_str()) > text.chars().count(),
            "{text:?} carries no wide glyph"
        );
    }

    /// No row is ever empty, in either language: a blank row in the sidebar is
    /// a layout bug the design does not have.
    #[test]
    fn no_row_is_ever_blank() {
        for lang in [Lang::Zh, Lang::En] {
            let app = App {
                lang,
                ..App::default()
            };
            for row in rows(&app) {
                assert!(!row.text.trim().is_empty(), "blank row in {lang:?}");
            }
        }
    }

    /// The selected row's chip spans the whole sidebar column, in both
    /// languages: the padding counts display columns, so a CJK row's chip lines
    /// up with the narrow ones instead of stopping short.
    #[test]
    fn the_selected_chip_spans_the_column_in_both_languages() {
        for lang in [Lang::Zh, Lang::En] {
            let app = App {
                lang,
                ..App::default()
            };
            let painted = lines(&app, &Theme::DARK, SIDEBAR_WIDTH);
            let chip = painted
                .iter()
                .find(|l| l.spans[0].style.bg == Some(Theme::DARK.accent))
                .expect("a selected row");
            let width = UnicodeWidthStr::width(chip.spans[0].content.as_ref());
            assert_eq!(width, SIDEBAR_WIDTH as usize, "{lang:?} chip {width} wide");
        }
    }

    /// A chip is padded, never truncated: a sidebar too narrow for its longest
    /// row keeps the text whole rather than cutting it off silently.
    #[test]
    fn a_narrow_column_pads_but_never_truncates() {
        let app = App {
            lang: Lang::Zh,
            ..App::default()
        };
        let painted = lines(&app, &Theme::DARK, 4);
        let chip = painted
            .iter()
            .find(|l| l.spans[0].style.bg == Some(Theme::DARK.accent))
            .expect("a selected row");
        assert_eq!(
            chip.spans[0].content.as_ref(),
            "▶ 传输方式: BLE",
            "a chip narrower than its text must keep the text whole and unpadded"
        );
    }

    /// The English hint is the one row too wide for the column, and it wraps
    /// exactly as the design draws it: the break falls before `first`, which
    /// starts a new row at the left margin.
    #[test]
    fn the_long_english_hint_wraps_as_the_design_draws_it() {
        let app = App {
            lang: Lang::En,
            ..App::default()
        };
        let pieces: Vec<String> = fitted(&app, &Theme::DARK, SIDEBAR_WIDTH)
            .iter()
            .map(|l| l.spans[0].content.to_string())
            .collect();
        let hint = "  Enter sends · reboot asks first";
        assert!(UnicodeWidthStr::width(hint) > SIDEBAR_WIDTH as usize);
        let start = pieces
            .iter()
            .position(|p| p.starts_with("  Enter"))
            .expect("the hint row");
        assert_eq!(pieces[start], "  Enter sends · reboot asks");
        assert_eq!(pieces[start + 1], "first");
        assert!(
            !pieces[start + 1].starts_with(' '),
            "the wrapped tail kept an indent"
        );
    }

    /// The Chinese hint fits the column, so it stays on one row: the wrap is
    /// driven by the width, not by the language.
    #[test]
    fn the_chinese_hint_fits_on_one_row() {
        let app = App {
            lang: Lang::Zh,
            ..App::default()
        };
        let pieces: Vec<String> = fitted(&app, &Theme::DARK, SIDEBAR_WIDTH)
            .iter()
            .map(|l| l.spans[0].content.to_string())
            .collect();
        let hint = "  Enter 发送 · reboot 先确认";
        assert!(UnicodeWidthStr::width(hint) <= SIDEBAR_WIDTH as usize);
        assert!(pieces.iter().any(|p| p == hint), "{pieces:?}");
    }
}
