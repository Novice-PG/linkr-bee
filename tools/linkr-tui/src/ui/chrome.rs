//! The two bars: what the session is doing on top, what the keys do below.
//!
//! Each bar is two lines rather than one, because the left and right halves are
//! placed in separate areas: a single line would need its own padding maths and
//! would overlap the moment a counter grew a digit.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::app::App;
use crate::event::ConnectionState;
use crate::theme::Theme;

/// The dot that opens the top bar. A filled circle for a live session, a ring
/// while dialling, and the same glyph in the state's colour otherwise — the web
/// client's status dot.
fn dot(app: &App) -> &'static str {
    match app.state {
        ConnectionState::Disconnected => "○",
        _ => "●",
    }
}

/// The indeterminate busy indicator: three cells, one marching marker. It only
/// shows while something is in flight, so a still frame has no moving parts.
pub fn busy(tick: usize) -> String {
    const CELLS: usize = 3;
    let at = tick % CELLS;
    let cells: Vec<&str> = (0..CELLS)
        .map(|cell| if cell == at { "-" } else { "·" })
        .collect();
    cells.join(" ")
}

/// Separator between the top bar's groups.
const DOT: &str = " · ";

/// The top bar's left half: state dot, state word, and a busy indicator while
/// the state is not settled.
pub fn top_left(app: &App, theme: &Theme) -> Line<'static> {
    let mut spans = vec![
        Span::styled(format!("{} ", dot(app)), theme.state_dot(app.state)),
        Span::styled(app.state_name().to_string(), theme.text()),
    ];
    if matches!(app.state, ConnectionState::Connecting) {
        spans.push(Span::styled(format!(" {}", busy(app.tick)), theme.muted()));
    }
    Line::from(spans)
}

/// The top bar's right half: traffic counters, the link speed, autoscroll and
/// the clock, in that order — the web client's status strip.
pub fn top_right(app: &App, theme: &Theme) -> Line<'static> {
    let s = app.strings();
    let mut spans = vec![
        Span::styled(format!("{} {}", s.rx, app.rx), theme.muted()),
        Span::styled(DOT.to_string(), theme.muted()),
        Span::styled(format!("{} {}", s.tx, app.tx), theme.muted()),
        Span::styled(DOT.to_string(), theme.muted()),
        Span::styled(app.baud_text(), theme.muted()),
        Span::styled(DOT.to_string(), theme.muted()),
        Span::styled(s.autoscroll.to_string(), theme.muted()),
        Span::styled(DOT.to_string(), theme.muted()),
        Span::styled(app.clock.clone(), theme.muted()),
    ];
    if !spans.is_empty() {
        spans.push(Span::raw(" "));
    }
    Line::from(spans)
}

/// The bottom bar's left half: which region has the keyboard, which view is
/// showing, and the session state.
pub fn bottom_left(app: &App, theme: &Theme) -> Line<'static> {
    let s = app.strings();
    let focus = match app.focus {
        crate::app::Focus::Terminal => s.focus_terminal,
        crate::app::Focus::Sidebar => s.focus_sidebar,
    };
    let state_style = theme.state_dot(app.state);
    Line::from(vec![
        Span::styled(format!("[{focus}] "), theme.accent()),
        Span::styled(app.view_name().to_string(), theme.text()),
        Span::styled(DOT.to_string(), theme.muted()),
        Span::styled(app.state_name().to_string(), state_style),
    ])
}

/// The bottom bar's right half: the four key hints the design spells out.
pub fn bottom_right(app: &App, theme: &Theme) -> Line<'static> {
    let s = app.strings();
    Line::from(vec![
        Span::styled(s.hint_palette.to_string(), theme.muted()),
        Span::styled(DOT.to_string(), theme.muted()),
        Span::styled(s.hint_help.to_string(), theme.muted()),
        Span::styled(DOT.to_string(), theme.muted()),
        Span::styled(s.hint_views.to_string(), theme.muted()),
        Span::styled(DOT.to_string(), theme.muted()),
        Span::styled(s.hint_quit.to_string(), theme.muted()),
    ])
}

/// The bar ground: a panel, full width.
pub fn bar_style(theme: &Theme) -> Style {
    theme.panel()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Lang;

    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// The top bar names the state and, while dialling, shows the busy
    /// indicator; a settled session shows no moving parts.
    #[test]
    fn the_top_bar_shows_the_state_and_a_busy_indicator_only_while_dialling() {
        let theme = Theme::DARK;
        let mut app = App {
            lang: Lang::Zh,
            ..App::default()
        };
        app.state = ConnectionState::Connecting;
        let text = line_text(&top_left(&app, &theme));
        assert!(text.contains("连接中…"), "{text:?}");
        assert!(text.contains('-') && text.contains('·'), "{text:?}");

        app.state = ConnectionState::Connected;
        let text = line_text(&top_left(&app, &theme));
        assert!(text.contains("已连接"), "{text:?}");
        assert!(
            !text.contains('-'),
            "connected still shows the busy bar: {text:?}"
        );
    }

    /// The busy indicator marches and wraps: three cells, one marker, always
    /// exactly one marker whatever tick it is handed.
    #[test]
    fn the_busy_indicator_marches_and_always_has_one_marker() {
        let frames: Vec<String> = (0..6).map(busy).collect();
        for frame in &frames {
            assert_eq!(frame.matches('-').count(), 1, "{frame:?}");
            assert_eq!(frame.matches('·').count(), 2, "{frame:?}");
        }
        assert_eq!(frames[0], frames[3], "the cycle repeats after three ticks");
        assert_ne!(frames[0], frames[1]);
    }

    /// The dot changes with the state, and takes the state's colour.
    #[test]
    fn the_status_dot_follows_the_state() {
        let theme = Theme::DARK;
        let mut app = App::default();
        let cases = [
            (ConnectionState::Connected, theme.accent_strong(), "●"),
            (ConnectionState::Connecting, theme.warn(), "●"),
            (ConnectionState::Failed, theme.danger(), "●"),
            (ConnectionState::Disconnected, theme.muted(), "○"),
        ];
        for (state, style, glyph) in cases {
            app.state = state;
            let line = top_left(&app, &theme);
            assert_eq!(line.spans[0].style.fg, style.fg, "{state:?}");
            assert_eq!(line.spans[0].content.trim_end(), glyph, "{state:?}");
        }
        // A session with anything live to say is filled; only the resting
        // state is hollow, so the dot reads at a glance.
        assert_eq!(dot(&App::default()), "○");
    }

    /// The right half reads counters, speed, autoscroll and the clock, in that
    /// order, in both languages.
    #[test]
    fn the_top_bar_right_reads_the_counters_in_order() {
        for (lang, rx, tx, auto) in [
            (Lang::Zh, "接收", "发送", "自动滚动"),
            (Lang::En, "RX", "TX", "Auto"),
        ] {
            let mut app = App {
                lang,
                ..App::default()
            };
            app.rx = 12;
            app.tx = 34;
            app.clock = "19:52:50".to_string();
            let text = line_text(&top_right(&app, &Theme::DARK));
            let expected = format!("{rx} 12 · {tx} 34 · — · {auto} · 19:52:50 ");
            assert_eq!(text, expected);
        }
    }

    /// The baud slot fills in once the device reports a speed, and the counters
    /// keep their places — the strip's width must not depend on the numbers.
    #[test]
    fn the_baud_slot_fills_in_without_moving_the_counters() {
        let mut app = App {
            lang: Lang::En,
            ..App::default()
        };
        app.baud = Some(115_200);
        app.clock = "19:52:50".to_string();
        let text = line_text(&top_right(&app, &Theme::DARK));
        assert_eq!(text, "RX 0 · TX 0 · 115200 · Auto · 19:52:50 ");
    }

    /// The bottom bar's left half names the focus, the view and the state.
    #[test]
    fn the_bottom_bar_left_names_focus_view_and_state() {
        let mut app = App {
            lang: Lang::Zh,
            ..App::default()
        };
        app.view = crate::event::View::Network;
        app.state = ConnectionState::Connecting;
        let text = line_text(&bottom_left(&app, &Theme::DARK));
        assert_eq!(text, "[终端] 网络 · 连接中…");

        app.focus = crate::app::Focus::Sidebar;
        let text = line_text(&bottom_left(&app, &Theme::DARK));
        assert_eq!(text, "[侧栏] 网络 · 连接中…");
    }

    /// The right half lists the four designed hints, in order, in both
    /// languages — the exact strings the screenshots show.
    #[test]
    fn the_bottom_bar_right_lists_the_four_hints() {
        let zh = line_text(&bottom_right(
            &App {
                lang: Lang::Zh,
                ..App::default()
            },
            &Theme::DARK,
        ));
        assert_eq!(zh, "Ctrl+P 面板 · F1 帮助 · F2-F6 视图 · Ctrl+Q 退出");
        let en = line_text(&bottom_right(
            &App {
                lang: Lang::En,
                ..App::default()
            },
            &Theme::DARK,
        ));
        assert_eq!(en, "Ctrl+P palette · F1 help · F2-F6 views · Ctrl+Q quit");
    }

    /// Every span in both bars takes its colour from the theme, so switching
    /// themes repaints the chrome with no other change.
    #[test]
    fn both_bars_take_their_colours_from_the_theme() {
        let app = App {
            lang: Lang::En,
            ..App::default()
        };
        let dark = [
            top_left(&app, &Theme::DARK),
            top_right(&app, &Theme::DARK),
            bottom_left(&app, &Theme::DARK),
            bottom_right(&app, &Theme::DARK),
        ];
        for line in &dark {
            for span in &line.spans {
                assert!(
                    span.style.fg.is_some() || span.content.trim().is_empty(),
                    "unstyled span {:?}",
                    span.content
                );
            }
        }
        let light_right = top_right(&app, &Theme::LIGHT);
        assert_eq!(light_right.spans[0].style.fg, Some(Theme::LIGHT.muted));
        assert_ne!(
            light_right.spans[0].style.fg,
            top_right(&app, &Theme::DARK).spans[0].style.fg
        );
    }
}
