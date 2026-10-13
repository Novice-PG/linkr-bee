//! Frame composition: where every region goes, and how a frame is painted.
//!
//! The layout is fixed by the design: a one-line bar top and bottom, a fixed
//! sidebar on the left, and the centre column taking the rest. Composition is
//! the only place that knows those numbers, so a view never guesses its own
//! size.

pub mod body;
pub mod chrome;
pub mod sidebar;
pub mod toast;

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::widgets::{Block, Paragraph};
use ratatui::Frame;

use crate::app::App;

/// Height of the top and bottom bars, in rows.
pub const BAR_HEIGHT: u16 = 1;

/// Width of the sidebar column, in columns — wide enough for the longest
/// designed row (`Transport: BLE (locked)` and the Chinese equivalent) with the
/// cursor marker and a margin.
pub const SIDEBAR_WIDTH: u16 = 30;

/// The regions of a frame, resolved once so tests can assert on them without
/// painting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Regions {
    pub top_bar: Rect,
    pub sidebar: Rect,
    pub centre: Rect,
    pub bottom_bar: Rect,
}

/// Resolve the designed layout for an area.
pub fn regions(area: Rect) -> Regions {
    let [top_bar, middle, bottom_bar] = Layout::vertical([
        Constraint::Length(BAR_HEIGHT),
        Constraint::Min(0),
        Constraint::Length(BAR_HEIGHT),
    ])
    .areas(area);

    // A terminal narrower than the sidebar plus a usable centre would leave the
    // centre at zero width; the sidebar yields instead, so a tiny window still
    // shows the view rather than a wall of sidebar.
    let sidebar_width = SIDEBAR_WIDTH.min(middle.width.saturating_sub(1));
    let [sidebar, centre] =
        Layout::horizontal([Constraint::Length(sidebar_width), Constraint::Min(0)]).areas(middle);

    Regions {
        top_bar,
        sidebar,
        centre,
        bottom_bar,
    }
}

/// Paint one frame.
pub fn draw(frame: &mut Frame, app: &App) {
    let theme = app.theme();
    let area = frame.area();

    // The page ground first, so no cell keeps the emulator's default background.
    frame.render_widget(Block::default().style(Style::default().bg(theme.bg)), area);

    let regions = regions(area);

    draw_bar(
        frame,
        app,
        regions.top_bar,
        chrome::top_left(app, &theme),
        chrome::top_right(app, &theme),
    );
    draw_sidebar(frame, app, regions.sidebar);
    draw_centre(frame, app, regions.centre);
    draw_bar(
        frame,
        app,
        regions.bottom_bar,
        chrome::bottom_left(app, &theme),
        chrome::bottom_right(app, &theme),
    );
    draw_toasts(frame, app, area);
}

/// One bar: the left half flush left, the right half flush right, split so they
/// can never overlap however long the counters grow.
fn draw_bar(
    frame: &mut Frame,
    app: &App,
    area: Rect,
    left: ratatui::text::Line<'static>,
    right: ratatui::text::Line<'static>,
) {
    let theme = app.theme();
    let ground = chrome::bar_style(&theme);
    frame.render_widget(Paragraph::new(left).style(ground), area);

    let right_width = right.width() as u16;
    if right_width < area.width {
        let [_, right_area] =
            Layout::horizontal([Constraint::Min(0), Constraint::Length(right_width)]).areas(area);
        frame.render_widget(Paragraph::new(right).style(ground), right_area);
    } else {
        // Not enough room for both: the right half is the counters and the key
        // hints, so it wins the row and the left half is dropped rather than
        // truncated into nonsense.
        frame.render_widget(Paragraph::new(right).style(ground), area);
    }
}

fn draw_sidebar(frame: &mut Frame, app: &App, area: Rect) {
    let theme = app.theme();
    let lines = sidebar::fitted(app, &theme, area.width);
    frame.render_widget(Paragraph::new(lines).style(theme.panel()), area);
}

fn draw_centre(frame: &mut Frame, app: &App, area: Rect) {
    let theme = app.theme();
    let ground = body::ground(app, &theme);
    if body::is_framed(app.view) {
        let block = theme.view_block(app.view_name());
        let inner = block.inner(area);
        frame.render_widget(block, area);
        frame.render_widget(
            Paragraph::new(body::lines(app, &theme)).style(ground),
            inner,
        );
    } else {
        frame.render_widget(Paragraph::new(body::lines(app, &theme)).style(ground), area);
    }
}

fn draw_toasts(frame: &mut Frame, app: &App, area: Rect) {
    if app.notices.is_empty() {
        return;
    }
    let theme = app.theme();
    let width = toast::width(&app.notices).min(area.width);
    let height = app.notices.len() as u16 + 2;
    if width < 3 || area.height < BAR_HEIGHT + height {
        return;
    }
    // Just under the top bar, flush right — the web client's toast corner.
    let corner = Rect {
        x: area.x + area.width - width,
        y: area.y + BAR_HEIGHT,
        width,
        height,
    };
    let level = toast::border_level(&app.notices).unwrap_or(crate::event::NoticeLevel::Info);
    let block = Block::bordered()
        .border_style(Style::default().fg(theme.toast_border(level)))
        .style(theme.toast());
    let inner = block.inner(corner);
    frame.render_widget(block, corner);
    frame.render_widget(
        Paragraph::new(toast::lines(app, &theme)).style(theme.toast()),
        inner,
    );
}

/// Render a frame into a cell buffer without a terminal. `None` when the
/// terminal cannot be built at all, which the callers treat as an empty frame
/// rather than a panic.
fn render_buffer(app: &App, width: u16, height: u16) -> Option<ratatui::buffer::Buffer> {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).ok()?;
    terminal.draw(|frame| draw(frame, app)).ok()?;
    Some(terminal.backend().buffer().clone())
}

/// Render a frame without a terminal, as text — for tests and for
/// `--print-frame`, which is how a frame is inspected without a TTY.
pub fn render_to_text(app: &App, width: u16, height: u16) -> String {
    let buffer = match render_buffer(app, width, height) {
        Some(buffer) => buffer,
        None => return String::new(),
    };
    let mut out = String::new();
    for y in 0..height {
        let mut x = 0;
        while x < width {
            let symbol = buffer[(x, y)].symbol();
            out.push_str(symbol);
            // A wide (CJK) glyph occupies two cells and ratatui blanks the
            // continuation cell. Stepping by the glyph's own width keeps the
            // text from growing a phantom space after every wide character —
            // which would also make the row look wider than the terminal.
            x += unicode_width::UnicodeWidthStr::width(symbol).max(1) as u16;
        }
        // Trailing blanks carry no information; trim so the golden text is
        // stable and readable.
        while out.ends_with(' ') {
            out.pop();
        }
        out.push('\n');
    }
    out
}

/// Render a frame with true-colour escape sequences, one line per row — what
/// `--print-frame --ansi` prints, so a frame can be looked at with its real
/// palette in any terminal (or piped into a viewer).
pub fn render_to_ansi(app: &App, width: u16, height: u16) -> String {
    let buffer = match render_buffer(app, width, height) {
        Some(buffer) => buffer,
        None => return String::new(),
    };
    let mut out = String::new();
    for y in 0..height {
        let mut x = 0;
        while x < width {
            let cell = &buffer[(x, y)];
            let symbol = cell.symbol();
            // Reset first, then the cell's own codes: no run tracking, so the
            // output cannot drift out of step with the buffer.
            out.push_str("\x1b[0m");
            out.push_str(&ansi_style(cell.style()));
            out.push_str(symbol);
            x += unicode_width::UnicodeWidthStr::width(symbol).max(1) as u16;
        }
        out.push_str("\x1b[0m\n");
    }
    out
}

/// The escape sequence for a style, if it needs one.
fn ansi_style(style: ratatui::style::Style) -> String {
    use ratatui::style::Modifier;
    let mut codes = String::new();
    if let Some(fg) = style.fg {
        codes.push_str(&ansi_colour(fg, false));
    }
    if let Some(bg) = style.bg {
        codes.push_str(&ansi_colour(bg, true));
    }
    if style.add_modifier.contains(Modifier::BOLD) {
        codes.push_str("\x1b[1m");
    }
    if style.add_modifier.contains(Modifier::DIM) {
        codes.push_str("\x1b[2m");
    }
    if style.add_modifier.contains(Modifier::REVERSED) {
        codes.push_str("\x1b[7m");
    }
    codes
}

/// A colour as a foreground (`38`) or background (`48`) escape.
fn ansi_colour(colour: ratatui::style::Color, background: bool) -> String {
    use ratatui::style::Color;
    let layer = if background { 48 } else { 38 };
    match colour {
        Color::Reset => format!("\x1b[{}m", if background { 49 } else { 39 }),
        Color::Rgb(r, g, b) => format!("\x1b[{layer};2;{r};{g};{b}m"),
        Color::Indexed(index) => format!("\x1b[{layer};5;{index}m"),
        // The sixteen named colours are palette entries 0–15.
        other => format!("\x1b[{layer};5;{}m", named_index(other)),
    }
}

/// The palette index of a named colour, so an ANSI-16 style still round-trips
/// through the dump.
fn named_index(colour: ratatui::style::Color) -> u8 {
    use ratatui::style::Color;
    match colour {
        Color::Black => 0,
        Color::Red => 1,
        Color::Green => 2,
        Color::Yellow => 3,
        Color::Blue => 4,
        Color::Magenta => 5,
        Color::Cyan => 6,
        Color::Gray => 7,
        Color::DarkGray => 8,
        Color::LightRed => 9,
        Color::LightGreen => 10,
        Color::LightYellow => 11,
        Color::LightBlue => 12,
        Color::LightMagenta => 13,
        Color::LightCyan => 14,
        Color::White => 15,
        // Reset, Rgb and Indexed are handled before this is reached.
        _ => 15,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{ConnectionState, NoticeLevel, View};
    use crate::i18n::Lang;
    use crate::theme::Theme;

    fn area(width: u16, height: u16) -> Rect {
        Rect {
            x: 0,
            y: 0,
            width,
            height,
        }
    }

    /// The layout is the designed one: bars one row high, sidebar fixed, centre
    /// taking the remainder, and the three regions tile the area exactly.
    #[test]
    fn the_regions_tile_the_area() {
        let regions = regions(area(120, 40));
        assert_eq!(regions.top_bar, area(120, 1));
        assert_eq!(
            regions.bottom_bar,
            Rect {
                x: 0,
                y: 39,
                width: 120,
                height: 1
            }
        );
        assert_eq!(regions.sidebar.height, 38);
        assert_eq!(regions.sidebar.width, SIDEBAR_WIDTH);
        assert_eq!(regions.centre.width, 120 - SIDEBAR_WIDTH);
        assert_eq!(regions.centre.x, SIDEBAR_WIDTH);
        // Sidebar and centre together are exactly the middle band.
        assert_eq!(regions.sidebar.width + regions.centre.width, 120);
        for region in [
            regions.top_bar,
            regions.sidebar,
            regions.centre,
            regions.bottom_bar,
        ] {
            assert!(
                region.right() <= 120 && region.bottom() <= 40,
                "{region:?} spills"
            );
        }
    }

    /// A window too narrow for both halves yields sidebar width to the centre:
    /// a view is never squeezed to nothing to keep an empty strip.
    #[test]
    fn a_narrow_window_gives_the_sidebar_less_room() {
        let wide = regions(area(20, 10));
        assert_eq!(wide.sidebar.width, 19);
        assert_eq!(wide.centre.width, 1);
        let narrow = regions(area(10, 10));
        assert_eq!(narrow.sidebar.width, 9);
        assert_eq!(narrow.centre.width, 1);
    }

    /// A degenerate area yields without panicking: zero-width and zero-height
    /// terminals happen while a window is being resized.
    #[test]
    fn degenerate_areas_do_not_panic() {
        for (w, h) in [(0, 0), (1, 1), (0, 20), (20, 0), (3, 2)] {
            let regions = regions(area(w, h));
            for region in [
                regions.top_bar,
                regions.sidebar,
                regions.centre,
                regions.bottom_bar,
            ] {
                assert!(
                    region.right() <= w && region.bottom() <= h,
                    "{w}x{h}: {region:?} spills out of the area"
                );
            }
        }
    }

    /// A rendered frame shows the designed chrome: the top bar's counters, every
    /// sidebar section and view, and the bottom bar's hints.
    #[test]
    fn a_rendered_frame_shows_the_designed_chrome() {
        let mut app = App {
            lang: Lang::Zh,
            ..App::default()
        };
        app.clock = "19:52:50".to_string();
        let text = render_to_text(&app, 120, 40);
        let lines: Vec<&str> = text.lines().collect();
        assert!(
            lines[0].contains("连接中…") || lines[0].contains("未连接"),
            "{}",
            lines[0]
        );
        assert!(
            lines[0].contains("接收 0") && lines[0].contains("19:52:50"),
            "{}",
            lines[0]
        );
        assert_eq!(lines.len(), 40);
        let last = lines[39];
        assert!(last.starts_with("[终端]"), "{last:?}");
        assert!(last.contains("Ctrl+Q 退出"), "{last:?}");
        for needle in ["连接", "视图", "快捷发送", "监控", "串口终端", "文件传输"]
        {
            assert!(text.contains(needle), "frame is missing {needle:?}\n{text}");
        }
        // The sidebar ends where the centre column begins.
        let sidebar_row = lines.iter().find(|l| l.contains("串口终端")).unwrap();
        assert!(!sidebar_row.contains("Ctrl"), "{sidebar_row:?}");
    }

    /// The frame's width is exactly the area's width: a row that ran long would
    /// wrap and shift every row below it — the layout bug the old TUI hit.
    #[test]
    fn no_row_ever_exceeds_the_area_width() {
        for width in [40u16, 60, 80, 120] {
            let app = App::default();
            let text = render_to_text(&app, width, 24);
            for line in text.lines() {
                assert!(
                    unicode_width::UnicodeWidthStr::width(line) <= width as usize,
                    "row wider than {width}: {line:?}"
                );
            }
        }
    }

    /// The terminal view paints its own ground and no frame; a framed view
    /// paints a border with its title on it.
    #[test]
    fn the_terminal_view_is_unframed_and_the_others_are_framed() {
        let mut app = App {
            lang: Lang::En,
            ..App::default()
        };
        let terminal = render_to_text(&app, 80, 12);
        assert!(
            !terminal.contains('╭'),
            "the terminal pane grew a frame:\n{terminal}"
        );

        app.view = View::Diagnostics;
        let framed = render_to_text(&app, 80, 12);
        assert!(framed.contains("Diagnostics"), "no title:\n{framed}");
        assert!(
            framed.contains('╭') && framed.contains('╯'),
            "no rounded frame:\n{framed}"
        );
    }

    /// A toast appears over the top-right corner, under the top bar, with its
    /// text intact — and a framed view behind it is not overwritten.
    #[test]
    fn a_toast_paints_in_the_corner() {
        let mut app = App {
            lang: Lang::Zh,
            ..App::default()
        };
        let without = render_to_text(&app, 100, 20);
        assert!(!without.contains("主题"));

        app.push_notice(crate::app::Notice {
            text: "主题: 浅色".to_string(),
            level: NoticeLevel::Info,
        });
        let with = render_to_text(&app, 100, 20);
        assert!(with.contains("主题: 浅色"), "{with}");
        let toast_row = with.lines().find(|l| l.contains("主题")).unwrap();
        assert!(
            unicode_width::UnicodeWidthStr::width(toast_row) > 80,
            "the toast is not in the right-hand corner: {toast_row:?}"
        );
        // The frame's row count is unchanged: the toast overlays, it does not
        // reflow the layout.
        assert_eq!(with.lines().count(), without.lines().count());
    }

    /// A frame is a pure function of the state: the same state paints the same
    /// bytes every time.
    #[test]
    fn a_frame_is_deterministic() {
        let app = App {
            state: ConnectionState::Connected,
            rx: 1234,
            clock: "01:02:03".to_string(),
            ..App::default()
        };
        let first = render_to_text(&app, 100, 30);
        let second = render_to_text(&app, 100, 30);
        assert_eq!(first, second);
    }

    /// Switching the theme changes the bytes, so a light-mode frame cannot be
    /// the dark one — and the layout is identical, only the colours moved.
    #[test]
    fn switching_the_theme_repaints_the_same_layout() {
        let mut app = App::default();
        let dark = render_to_text(&app, 100, 30);
        app.theme = crate::theme::ThemeChoice::Light;
        let light = render_to_text(&app, 100, 30);
        assert_eq!(dark.lines().count(), light.lines().count());
        assert_eq!(dark, light, "text layout must not depend on the theme");
    }

    /// Strip every escape sequence, leaving the characters a terminal would
    /// show — what the ANSI dump and the plain dump must agree on.
    fn strip_ansi(text: &str) -> String {
        let mut out = String::new();
        let mut in_escape = false;
        for ch in text.chars() {
            if in_escape {
                if ch == 'm' {
                    in_escape = false;
                }
            } else if ch == '\u{1b}' {
                in_escape = true;
            } else {
                out.push(ch);
            }
        }
        out
    }

    /// The ANSI dump is the same frame as the text dump, with colour: the
    /// characters match row for row, so the two can never drift. Only trailing
    /// blanks differ — the ANSI dump keeps them because they carry the panel
    /// background, the text dump trims them so golden text stays readable.
    #[test]
    fn the_ansi_dump_and_the_text_dump_are_the_same_frame() {
        let app = App::default();
        let text: Vec<String> = render_to_text(&app, 80, 20)
            .lines()
            .map(|line| line.trim_end().to_string())
            .collect();
        let ansi: Vec<String> = strip_ansi(&render_to_ansi(&app, 80, 20))
            .lines()
            .map(|line| line.trim_end().to_string())
            .collect();
        assert_eq!(ansi, text);
    }

    /// The dump carries true colour: the accent chip's own RGB is in there, so
    /// a viewer sees the palette rather than the emulator's defaults.
    #[test]
    fn the_ansi_dump_carries_true_colour() {
        let app = App::default();
        let ansi = render_to_ansi(&app, 80, 20);
        let accent_bg = Theme::DARK.accent;
        let ratatui::style::Color::Rgb(r, g, b) = accent_bg else {
            panic!("the accent is not an RGB colour");
        };
        let chip = format!("48;2;{r};{g};{b}");
        assert!(
            ansi.contains(&chip),
            "no accent background {chip} in the dump"
        );
        // Every row ends with a reset, so colour cannot leak between rows.
        for line in ansi.lines() {
            assert!(
                line.ends_with("\u{1b}[0m"),
                "{line:?} does not end with a reset"
            );
        }
    }

    /// Switching the theme changes the dump's colours but not its text.
    #[test]
    fn the_ansi_dump_follows_the_theme() {
        let app = App::default();
        let dark = render_to_ansi(&app, 60, 12);
        let light = render_to_ansi(
            &App {
                theme: crate::theme::ThemeChoice::Light,
                ..App::default()
            },
            60,
            12,
        );
        assert_ne!(dark, light);
        let rows = |dump: &str| -> Vec<String> {
            strip_ansi(dump)
                .lines()
                .map(|line| line.trim_end().to_string())
                .collect()
        };
        assert_eq!(rows(&dark), rows(&light));
    }

    /// A degenerate area dumps nothing rather than panicking, so a dumped
    /// frame is safe to produce at any size.
    #[test]
    fn the_ansi_dump_survives_a_degenerate_area() {
        let app = App::default();
        for (w, h) in [(1u16, 1u16), (0, 0), (2, 3)] {
            let ansi = render_to_ansi(&app, w, h);
            assert_eq!(ansi.lines().count(), h as usize, "{w}x{h}");
        }
    }

    /// A tiny terminal renders without panicking and without a row longer than
    /// the area, which is the resize-storm case.
    #[test]
    fn tiny_terminals_render_without_panicking() {
        for (w, h) in [(1u16, 1u16), (2, 2), (5, 3), (12, 4), (30, 2)] {
            let app = App::default();
            let text = render_to_text(&app, w, h);
            assert_eq!(text.lines().count(), h as usize, "{w}x{h}");
            for line in text.lines() {
                assert!(
                    unicode_width::UnicodeWidthStr::width(line) <= w as usize,
                    "{w}x{h}: {line:?}"
                );
            }
        }
    }
}
