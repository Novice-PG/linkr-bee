//! The modal layer: the command palette, and the dialogs that go over it.
//!
//! Painting lives here rather than in `palette`/`dialogs` so those two stay
//! headless — their state, their arithmetic and their keys are testable without
//! a terminal, and this is the only place that knows what a `Rect` is.
//!
//! A modal *covers* the frame rather than reflowing it: the design's layout is
//! the same whether or not an overlay is up, so the region under the box is
//! cleared first and everything else is left exactly where it was.

use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;

use crate::app::App;
use crate::{dialogs, palette};

/// How much of the region a dialog asks for.
const DIALOG_WIDTH_PERCENT: u16 = 78;

/// The palette asks for more of it: its rows are a table, and a table that has
/// to clip is a table the user cannot read.
const PALETTE_WIDTH_PERCENT: u16 = 92;

/// How much of the region a modal asks for vertically.
const HEIGHT_PERCENT: u16 = 76;

/// The room a modal needs before it is worth showing at all. Below this the box
/// would be its own border with nothing inside, which is worse than leaving the
/// view visible.
const MIN_WIDTH: u16 = 24;
const MIN_HEIGHT: u16 = 5;

/// The widest a modal grows, so a very wide terminal does not stretch the
/// palette's columns across the screen.
const MAX_WIDTH: u16 = 96;

/// Paint whichever modal is open.
///
/// The two modals are a stack, and only the top of it is painted. A dialog is
/// raised *over* the palette — `dialogs` closes the palette when it opens — so
/// a dialog wins; the palette is painted only when no dialog is up, which is
/// the state Ctrl+P leaves behind when it opens over one. Painting both was
/// what made Ctrl+P look dead: the palette was open but hidden under the
/// dialog, and the keyboard went to the dialog, so Escape was the only way to
/// find it again.
///
/// A modal is centred in the *centre column*, not over the whole frame: the
/// rail, the counters and the key hints stay visible, which is the point of a
/// palette that can be read while the session is still running.
pub fn paint(frame: &mut Frame, app: &App, area: Rect) {
    let region = super::regions(area, app.sidebar_visible).centre;
    if region.width < MIN_WIDTH || region.height < MIN_HEIGHT {
        return;
    }
    // Top of the stack first. A dialog closes the palette when it opens, so
    // the only way both are up is `Ctrl+P` raising the palette *over* a
    // dialog — and that is the one the keyboard answers to, so that is the
    // one that is painted.
    if let Some(state) = &app.palette {
        paint_palette(frame, app, state, region);
    } else if let Some(dialog) = &app.dialog {
        paint_dialog(frame, app, dialog, region);
    }
}

/// A `width`×`height` box centred in `area`, never larger than it.
fn centred(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

/// The size a modal asks for, given how much of the width it wants.
fn wanted(area: Rect, share: u16, max_height: u16) -> (u16, u16) {
    let width = (area.width * share / 100)
        .clamp(MIN_WIDTH, MAX_WIDTH)
        .min(area.width);
    let height = (area.height * HEIGHT_PERCENT / 100)
        .clamp(MIN_HEIGHT, max_height)
        .min(area.height);
    (width, height)
}

fn paint_dialog(frame: &mut Frame, app: &App, dialog: &dialogs::Dialog, area: Rect) {
    let theme = app.theme();
    // As tall as its content, up to the ceiling the region allows: the body
    // scrolls, so a box that is too tall wastes the pane behind it, and a box
    // that is too short would refuse to show a long notice.
    let (width, ceiling) = wanted(area, DIALOG_WIDTH_PERCENT, area.height);
    let body = dialogs::body_height(app, dialog, width.saturating_sub(2));
    let height = (body as u16 + 3).clamp(MIN_HEIGHT, ceiling);
    let rect = centred(area, width, height);
    frame.render_widget(Clear, rect);

    let block = theme.overlay_block(dialogs::title(dialog, app.lang));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    // The legend rides the box's bottom row, so it stays put however far the
    // body has been scrolled.
    let body_height = inner.height.saturating_sub(1) as usize;
    let mut lines = dialogs::lines(dialog, app, inner.width, body_height);
    lines.push(dialogs::legend(dialog, app.lang, &theme));
    frame.render_widget(Paragraph::new(lines).style(theme.panel()), inner);
}

fn paint_palette(frame: &mut Frame, app: &App, state: &palette::PaletteState, area: Rect) {
    let theme = app.theme();
    // prompt + separator + blank + legend, plus the box's own two rows: the
    // window is whatever is left, capped at the registry's own maximum.
    const CHROME_ROWS: u16 = 6;
    let room = area.height.saturating_sub(CHROME_ROWS) as usize;
    let visible = palette::MAX_VISIBLE.min(room).max(1);
    let (width, _) = wanted(area, PALETTE_WIDTH_PERCENT, area.height);
    let rect = centred(area, width, visible as u16 + CHROME_ROWS);
    frame.render_widget(Clear, rect);

    let block = theme.overlay_block(palette::panel_title(app.lang));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let mut lines = palette::lines_in(state, app.lang, visible, &theme);
    lines.push(Line::default());
    lines.push(palette::legend(app.lang, &theme));
    frame.render_widget(Paragraph::new(lines).style(theme.panel()), inner);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::View;
    use crate::i18n::Lang;
    use crate::theme::ThemeChoice;
    use crate::ui;

    fn area(width: u16, height: u16) -> Rect {
        Rect {
            x: 0,
            y: 0,
            width,
            height,
        }
    }

    /// The box is centred, fits inside the frame, and grows with the terminal
    /// but never past its own ceiling.
    #[test]
    fn a_modal_is_centred_and_bounded() {
        for (w, h) in [(80u16, 24u16), (120, 40), (200, 60), (26, 8)] {
            let frame = area(w, h);
            // Sized against the centre column, which is what the painter passes
            // in — the rail is not the modal's to use.
            let region = crate::ui::regions(frame, true).centre;
            let (bw, bh) = wanted(region, PALETTE_WIDTH_PERCENT, region.height);
            assert!(bw <= MAX_WIDTH.min(region.width), "{w}x{h}: width {bw}");
            assert!(
                bw <= region.width && bh <= region.height,
                "{w}x{h}: {bw}x{bh} spills"
            );
            let rect = centred(region, bw, bh);
            assert!(
                rect.right() <= region.right() && rect.bottom() <= region.bottom(),
                "{w}x{h}: {rect:?}"
            );
            // Centred: the margins on the two sides differ by at most one
            // column, which is as centred as an odd remainder allows.
            let left = rect.x - region.x;
            let right = region.right() - rect.right();
            assert!(left.abs_diff(right) <= 1, "{w}x{h}: {left} vs {right}");
        }
    }

    /// Below its own minimum the modal is not drawn at all: half a border with
    /// nothing in it is worse than the view it covers.
    #[test]
    fn a_frame_too_small_gets_no_modal() {
        let mut app = App {
            lang: Lang::En,
            ..App::default()
        };
        crate::palette::open(&mut app);
        for (w, h) in [(1u16, 1u16), (MAR_W, 2), (MAR_W, MIN_HEIGHT)] {
            let text = ui::render_to_text(&app, w, h);
            assert!(!text.contains("Command palette"), "{w}x{h}:\n{text}");
        }
        let text = ui::render_to_text(&app, 80, 24);
        assert!(text.contains("Command palette"), "{text}");
        const MAR_W: u16 = MIN_WIDTH - 1;
    }

    /// The palette paints over the frame without reflowing it: the row count is
    /// unchanged, the box carries its title and legend, and the view behind it
    /// is still where the layout put it.
    #[test]
    fn the_palette_paints_over_the_frame() {
        let mut app = App {
            lang: Lang::En,
            ..App::default()
        };
        let without = ui::render_to_text(&app, 100, 30);
        crate::palette::open(&mut app);
        let with = ui::render_to_text(&app, 100, 30);

        assert_eq!(with.lines().count(), without.lines().count());
        assert!(with.contains("Command palette (Ctrl+P)"), "{with}");
        assert!(
            with.contains("Open terminal view"),
            "the rows are listed:\n{with}"
        );
        // The window is a fixed number of rows from the highlighted one, opened
        // here at the top — so the shortcut looked for is one that window is
        // actually showing, not one further down the list.
        assert!(with.contains("Ctrl+Up"), "and their shortcuts:\n{with}");
        assert!(with.contains("run"), "the legend is there:\n{with}");
        assert!(
            with.contains('╭') && with.contains('╰'),
            "the box is drawn with its own corners:\n{with}"
        );
        // The chrome the modal did not cover is untouched.
        let head = with.lines().next().unwrap_or_default();
        assert!(head.contains("RX"), "the top bar survived: {head:?}");
        let rail = with.lines().nth(1).unwrap_or_default();
        assert!(rail.contains("Connection"), "the rail survived: {rail:?}");
    }

    /// The palette is the top of the modal stack: with a dialog under it the
    /// palette is what is painted, because it is also what takes the keys.
    /// Painting the dialog here was what made `Ctrl+P` look dead — the palette
    /// opened, stayed hidden, and Escape was the only way to find it.
    #[test]
    fn the_palette_paints_over_a_dialog() {
        let mut app = App {
            lang: Lang::En,
            ..App::default()
        };
        crate::dialogs::ask(
            &mut app,
            crate::dialogs::ConfirmAction::Send("reboot".into()),
        );
        assert!(app.dialog.is_some());

        crate::palette::open(&mut app);
        let text = ui::render_to_text(&app, 100, 30);
        assert!(
            text.contains("Command palette"),
            "the palette is on top:\n{text}"
        );
        assert!(
            !text.contains("to the device?"),
            "the dialog under it is not painted:\n{text}"
        );

        // Closing the top of the stack puts the dialog back, unharmed.
        app.palette = None;
        let text = ui::render_to_text(&app, 100, 30);
        assert!(text.contains("to the device?"), "{text}");
        assert!(app.dialog.is_some(), "the dialog survived");
    }

    /// A dialog raised while the palette is up takes the keyboard and the
    /// screen: the two are never both answering, and the one that was opened
    /// last is the one that is.
    #[test]
    fn a_dialog_raised_over_the_palette_is_the_one_painted() {
        let mut app = App {
            lang: Lang::En,
            ..App::default()
        };
        crate::palette::open(&mut app);
        crate::dialogs::open_help(&mut app);
        let text = ui::render_to_text(&app, 100, 30);
        assert!(text.contains("Keyboard help"), "{text}");
        assert!(
            !text.contains("Command palette (Ctrl+P)"),
            "the palette gave up the screen:\n{text}"
        );
    }

    /// The palette follows the language, title and rows alike.
    #[test]
    fn the_palette_follows_the_language() {
        let mut app = App {
            lang: Lang::Zh,
            ..App::default()
        };
        crate::palette::open(&mut app);
        let text = ui::render_to_text(&app, 100, 30);
        assert!(text.contains("命令面板"), "{text}");
        assert!(text.contains("打开终端视图"), "{text}");
    }

    /// A dialog paints with its own title and legend, and the palette's title
    /// is not on screen at the same time.
    #[test]
    fn a_dialog_paints_over_the_frame() {
        let mut app = App {
            lang: Lang::En,
            ..App::default()
        };
        crate::dialogs::open_help(&mut app);
        let text = ui::render_to_text(&app, 100, 30);
        assert!(text.contains("Keyboard help"), "{text}");
        assert!(
            text.contains("Command palette"),
            "the chord list is shown:\n{text}"
        );
        assert!(text.contains("Ctrl+Q"), "{text}");
        assert!(
            !text.contains("Command palette (Ctrl+P)"),
            "no palette title"
        );

        app.dialog = Some(crate::dialogs::Dialog::Confirm(crate::dialogs::Confirm {
            prompt: "Reboot the bridge?".into(),
            detail: "It will not come back on its own.".into(),
            action: crate::dialogs::ConfirmAction::Send("reboot".into()),
            yes: false,
        }));
        let text = ui::render_to_text(&app, 100, 30);
        assert!(text.contains("Reboot the bridge?"), "{text}");
        assert!(
            text.contains("It will not come back"),
            "the consequence is shown:\n{text}"
        );
        assert!(
            text.contains("Confirm") && text.contains("Cancel"),
            "both buttons are drawn:\n{text}"
        );
        assert!(!text.contains("Keyboard help"), "the help was replaced");
    }

    /// The notice log paints over the pane and shows the log's rows.
    #[test]
    fn the_notice_log_paints_over_the_frame() {
        let mut app = App {
            lang: Lang::Zh,
            ..App::default()
        };
        app.push_notice(crate::app::Notice {
            text: "链路已建立：stdio: -".into(),
            level: crate::event::NoticeLevel::Info,
        });
        crate::dialogs::open_notices(&mut app);
        let text = ui::render_to_text(&app, 100, 30);
        assert!(text.contains("通知记录"), "{text}");
        assert!(text.contains("链路已建立"), "{text}");
        assert!(text.contains("信息"), "the level is spelled out:\n{text}");
    }

    /// A modal never paints a row wider than the frame, and never panics on a
    /// degenerate area — the resize-storm case.
    #[test]
    fn a_modal_survives_every_size() {
        let mut app = App {
            lang: Lang::Zh,
            ..App::default()
        };
        crate::palette::open(&mut app);
        for (w, h) in [(24u16, 5u16), (40, 12), (120, 40), (1, 1), (3, 3), (0, 0)] {
            let text = ui::render_to_text(&app, w, h);
            assert_eq!(text.lines().count(), h as usize, "{w}x{h}");
            for line in text.lines() {
                assert!(
                    unicode_width::UnicodeWidthStr::width(line) <= w as usize,
                    "{w}x{h}: {line:?}"
                );
            }
        }
    }

    /// A modal is deterministic and follows the theme's ground: the same state
    /// paints the same bytes, and the two themes differ.
    #[test]
    fn a_modal_follows_the_theme() {
        let mut app = App::default();
        crate::palette::open(&mut app);
        assert_eq!(
            ui::render_to_text(&app, 100, 30),
            ui::render_to_text(&app, 100, 30)
        );
        let dark = ui::render_to_ansi(&app, 100, 30);
        app.theme = ThemeChoice::Light;
        let light = ui::render_to_ansi(&app, 100, 30);
        assert_ne!(dark, light);
    }

    /// The palette's window shrinks with the room it is given rather than
    /// running off the bottom of the box.
    #[test]
    fn the_palette_window_fits_its_room() {
        for height in [8u16, 16, 24, 40] {
            let mut app = App {
                lang: Lang::En,
                ..App::default()
            };
            crate::palette::open(&mut app);
            let text = ui::render_to_text(&app, 100, height);
            let rows: Vec<&str> = text.lines().collect();
            let box_top = rows
                .iter()
                .position(|line| line.contains("Command palette"))
                .expect("the title");
            let box_bottom = rows
                .iter()
                .rposition(|line| line.contains('╰'))
                .expect("the closing corner");
            assert!(box_bottom > box_top, "{height}: the box is closed");
            assert!(
                box_bottom < height as usize,
                "{height}: the box ran off the frame"
            );
            assert_eq!(rows.len(), height as usize, "{height}: the frame reflowed");
        }
    }

    /// A confirmation is as tall as its content, not as tall as the screen: an
    /// otherwise empty box reads as a broken dialog rather than a careful one.
    #[test]
    fn a_confirmation_box_fits_its_content() {
        let mut app = App {
            lang: Lang::En,
            ..App::default()
        };
        crate::dialogs::ask(
            &mut app,
            crate::dialogs::ConfirmAction::Send("reboot".into()),
        );
        let text = ui::render_to_text(&app, 110, 32);
        let rows: Vec<&str> = text.lines().collect();
        let top = rows
            .iter()
            .position(|line| line.contains("Confirm"))
            .expect("the title");
        let bottom = rows
            .iter()
            .rposition(|line| line.contains('╰'))
            .expect("the closing corner");
        let box_rows = bottom - top + 1;
        assert!(text.contains("reboot"), "the command is named:\n{text}");
        assert!(text.contains("Cancel"), "the buttons are drawn:\n{text}");
        assert!(
            (5..=10).contains(&box_rows),
            "the confirm box is {box_rows} rows, not a snug one:\n{text}"
        );
    }

    /// The notice log grows with its content up to the ceiling, then stops —
    /// the box is as tall as it can usefully be, not as tall as the log.
    #[test]
    fn the_notice_log_grows_to_its_ceiling() {
        let mut app = App {
            lang: Lang::En,
            ..App::default()
        };
        for i in 0..40 {
            app.push_notice(crate::app::Notice {
                text: format!("notice {i}"),
                level: crate::event::NoticeLevel::Info,
            });
        }
        crate::dialogs::open_notices(&mut app);
        let text = ui::render_to_text(&app, 110, 32);
        let rows: Vec<&str> = text.lines().collect();
        let top = rows
            .iter()
            .position(|line| line.contains("Notice log"))
            .expect("the title");
        let bottom = rows
            .iter()
            .rposition(|line| line.contains('╰'))
            .expect("the closing corner");
        assert!(
            bottom - top + 1 < rows.len(),
            "the log box took the whole frame:\n{text}"
        );
        assert!(text.contains("notice 39"), "the newest is shown:\n{text}");
    }

    /// The view under a modal is not moved: the sidebar's rows stay in their
    /// columns, so an overlay is an overlay and not a second layout.
    #[test]
    fn the_view_under_a_modal_does_not_move() {
        let app = App {
            lang: Lang::En,
            view: View::Network,
            ..App::default()
        };
        let without: Vec<String> = ui::render_to_text(&app, 110, 30)
            .lines()
            .map(|line| line.chars().take(ui::SIDEBAR_TEXT_WIDTH as usize).collect())
            .collect();
        let mut covered = app;
        crate::palette::open(&mut covered);
        let with: Vec<String> = ui::render_to_text(&covered, 110, 30)
            .lines()
            .map(|line| line.chars().take(ui::SIDEBAR_TEXT_WIDTH as usize).collect())
            .collect();
        assert_eq!(without, with, "the rail is painted identically");
    }
}
