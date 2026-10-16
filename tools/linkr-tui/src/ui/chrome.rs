//! The two bars: what the session is doing on top, what the keys do below.
//!
//! Each bar is two lines rather than one, because the left and right halves are
//! placed in separate areas: a single line would need its own padding maths and
//! would overlap the moment a counter grew a digit.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{RenderDirection, Sparkline};

use crate::app::App;
use crate::theme::Theme;

/// The separator between the top bar's groups.
const DOT: &str = " · ";

/// The top bar's left half: the state's glyph, the state's name, and the two
/// facts that describe the link — the level it negotiated and the signal it is
/// carrying. Both read as placeholders until the device reports them, which is
/// what the design's first frame shows.
pub fn top_left(app: &App, theme: &Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{} ", app.state_glyph()),
            theme.state_dot(app.state),
        ),
        Span::styled(app.state_name().to_string(), theme.text()),
        Span::styled(DOT.to_string(), theme.muted()),
        Span::styled(app.link_text(), theme.muted()),
        Span::styled(DOT.to_string(), theme.muted()),
        Span::styled(app.signal_text(), theme.muted()),
    ])
}

/// The top bar's right half: traffic counters, the link speed, autoscroll and
/// the clock, in that order — the web client's status strip. It ends flush with
/// the last column, so the strip's right edge does not move with the numbers.
///
/// The two counters wear the two graph colours — RX the accent, TX the focus
/// blue — which is what lets the graph's halves go without a legend of their
/// own.
pub fn top_right(app: &App, theme: &Theme) -> Line<'static> {
    let s = app.strings();
    Line::from(vec![
        Span::styled(format!("{} {}", s.rx, app.rx), theme.accent()),
        Span::styled(DOT.to_string(), theme.muted()),
        Span::styled(format!("{} {}", s.tx, app.tx), theme.focus()),
        Span::styled(DOT.to_string(), theme.muted()),
        Span::styled(app.baud_text(), theme.muted()),
        Span::styled(DOT.to_string(), theme.muted()),
        Span::styled(s.autoscroll.to_string(), theme.muted()),
        Span::styled(DOT.to_string(), theme.muted()),
        Span::styled(app.clock.clone(), theme.muted()),
    ])
}

/// The traffic unit's width at the right end of a `bar_width`-wide top bar:
/// the current rate and the graph behind it.
///
/// Zero when the bar is too narrow to spare the columns. The unit is the last
/// thing the bar gained and the first it gives up, because the counters and
/// the clock read without it — and a hijacked full-width strip would push the
/// session's own state off a small terminal.
pub fn traffic_unit(bar_width: u16) -> u16 {
    match bar_width {
        120.. => 24,
        104..=119 => 18,
        92..=103 => 13,
        _ => 0,
    }
}

/// The unit's label: the newest rate, in both directions together — the sum of
/// the two newest columns the graphs draw. The spaces are part of the label so
/// the rate is held off the clock's tail and the graphs' heads.
///
/// The graphs alone are shape, not scale: without this number a spike could
/// just as well be a megabyte as a byte.
pub fn traffic_label(app: &App, theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(
        format!(" {}/s ", crate::text::human_bytes(app.traffic.rate)),
        theme.accent(),
    ))
}

/// The two graphs: the sampled rates of one direction each, newest at the
/// right edge, RX in the accent and TX in the focus blue — the colours the
/// counters wear, because one series of the two directions together could only
/// ever show their sum.
///
/// The data is reversed with the direction so the widget's own rule — it draws
/// the first samples it is given and stops at the area's width — drops the
/// *oldest* overhang rather than the newest. A graph whose right edge went
/// stale would be worse than no graph.
pub fn rx_graph(app: &App, theme: &Theme) -> Sparkline<'static> {
    samples(
        app.traffic.rx_window.iter().rev().copied(),
        app.traffic.scale(),
        theme.accent(),
    )
}

/// The other direction's graph; see [`rx_graph`].
pub fn tx_graph(app: &App, theme: &Theme) -> Sparkline<'static> {
    samples(
        app.traffic.tx_window.iter().rev().copied(),
        app.traffic.scale(),
        theme.focus(),
    )
}

/// One direction's samples as the widget both graphs are built from, on the
/// one scale the two share.
fn samples(data: impl IntoIterator<Item = u64>, scale: u64, style: Style) -> Sparkline<'static> {
    Sparkline::default()
        .data(data)
        .max(scale)
        .direction(RenderDirection::RightToLeft)
        .style(style)
}

/// The glyph that heads the RX graph, in the colour that graph is drawn in.
pub fn rx_arrow(theme: &Theme) -> Span<'static> {
    Span::styled("↓", theme.accent())
}

/// The glyph that heads the TX graph; see [`rx_arrow`].
pub fn tx_arrow(theme: &Theme) -> Span<'static> {
    Span::styled("↑", theme.focus())
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
    let mut spans = vec![Span::styled(format!("[{focus}] "), theme.accent())];
    // The view's own name, then what the keyboard is on *inside* it. A rail
    // focused on a link the user has not picked says "link", because that is
    // the row under the cursor and that is what Enter will act on — a left
    // half that only ever said the view's name left the operator to count rows
    // to know what they were about to press.
    spans.push(Span::styled(app.view_name().to_string(), theme.text()));
    spans.push(Span::styled(format!(" / {}", focus_detail(app, s)), theme.muted()));
    spans.push(Span::styled(DOT.to_string(), theme.muted()));
    spans.push(Span::styled(app.state_name().to_string(), state_style));
    // The latch, while one is armed: the announcement times out, the chip
    // does not, so a user who looked away still sees what the next key will
    // carry.
    if app.latch.is_armed() {
        spans.push(Span::styled(DOT.to_string(), theme.muted()));
        spans.push(Span::styled(
            format!("{} {}", app.latch.names(), s.latch_pending),
            theme.warn(),
        ));
    }
    Line::from(spans)
}

/// What the keyboard is on inside the focused view, in the interface language.
///
/// A function of the focus rather than of the view, because a rail's rows are
/// the same rows whichever view is open — the transport, the two connection
/// actions, and the quick sends — and naming the row is what tells the user
/// what Enter is about to do.
fn focus_detail(app: &App, s: &crate::i18n::Strings) -> String {
    match app.focus {
        crate::app::Focus::Sidebar => {
            let targets = crate::ui::sidebar::targets(app);
            targets
                .get(app.sidebar_cursor)
                .cloned()
                .unwrap_or_else(|| s.focus_terminal.to_string())
        }
        crate::app::Focus::Terminal => s.term_input.to_string(),
    }
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

/// The dot the status pill opens with. A filled circle in every state, so the
/// pill reads as a state light and the colour is what carries the state.
const PILL_DOT: &str = "●";

/// The terminal pane's status pill: the live state, on a chip, for the pane's
/// head row.
///
/// The web client keeps this pill in its header — one dot, the state, and the
/// device it is on. The pane's head row is where the design puts it. The device
/// name is already a sidebar row of its own, so the pill carries the dot and
/// the state and nothing else.
///
/// The dot takes the state's colour from the same place the top bar's glyph and
/// the sidebar's do, so the three can never disagree about what the session is
/// doing — which is what the design's own screenshot does, painting the same
/// connecting state amber in two places and blue in this one.
pub fn status_pill(app: &App, theme: &Theme) -> Line<'static> {
    Line::from(vec![
        // The padding is part of the chip, so it carries the chip's ground.
        Span::styled(format!(" {PILL_DOT} "), theme.pill_dot(app.state)),
        Span::styled(app.state_name().to_string(), theme.pill()),
        Span::styled(" ", theme.pill()),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::ConnectionState;
    use crate::i18n::Lang;
    use crate::text::columns;

    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// The pill names the live state, opens with a filled dot in the state's own
    /// colour, and sits on the chip's ground — the design's `● 连接中…`.
    #[test]
    fn the_status_pill_names_the_state_with_a_state_coloured_dot() {
        let theme = Theme::DARK;
        let mut app = App {
            lang: Lang::Zh,
            ..App::default()
        };
        app.state = ConnectionState::Connecting;
        let pill = status_pill(&app, &theme);
        assert_eq!(line_text(&pill), " ● 连接中… ");
        assert_eq!(pill.spans[0].style.fg, Some(theme.warn));
        assert_eq!(pill.spans[0].style.bg, Some(theme.panel_3));
        assert_eq!(pill.spans[1].style, theme.pill());
        assert_eq!(pill.spans[2].style, theme.pill());

        app.state = ConnectionState::Connected;
        let connected = status_pill(&app, &theme);
        assert_eq!(line_text(&connected), " ● 已连接 ");
        // The dot is the only thing that moves with the state, and it is the
        // same colour the top bar's glyph and the sidebar's use.
        assert_eq!(connected.spans[0].style.fg, Some(theme.accent_strong));
        assert_eq!(connected.spans[1].content, app.state_name());
    }

    /// The pill's geometry is the design's: the dot, the state, and one column
    /// of padding each side. Its width is what puts its right edge in the
    /// designed column, so a change here moves the pill.
    #[test]
    fn the_pill_is_the_dot_the_state_and_its_padding() {
        let theme = Theme::DARK;
        let app = App {
            lang: Lang::En,
            state: ConnectionState::Connecting,
            ..App::default()
        };
        let pill = status_pill(&app, &theme);
        assert_eq!(line_text(&pill), " ● connecting… ");
        assert_eq!(pill.width(), 1 + 1 + 1 + columns("connecting…") + 1);
    }

    /// Both themes paint the pill: a light pane is not left without its state.
    #[test]
    fn the_pill_follows_the_theme() {
        let app = App::default();
        let dark = status_pill(&app, &Theme::DARK);
        let light = status_pill(&app, &Theme::LIGHT);
        assert_eq!(line_text(&dark), line_text(&light));
        assert_eq!(dark.spans[1].style, Theme::DARK.pill());
        assert_eq!(light.spans[1].style, Theme::LIGHT.pill());
    }

    /// The top bar names the state and the two facts about its link. Until the
    /// device reports either, both slots hold the placeholder — the frame the
    /// design's screenshots show.
    #[test]
    fn the_top_bar_names_the_state_and_its_link_facts() {
        let theme = Theme::DARK;
        let mut app = App {
            lang: Lang::Zh,
            ..App::default()
        };
        app.state = ConnectionState::Connecting;
        assert_eq!(line_text(&top_left(&app, &theme)), "◐ 连接中… · ─ · ─");

        app.state = ConnectionState::Connected;
        assert_eq!(line_text(&top_left(&app, &theme)), "● 已连接 · ─ · ─");

        app.link_level = Some(2);
        app.signal_dbm = Some(-58);
        assert_eq!(
            line_text(&top_left(&app, &theme)),
            "● 已连接 · 链路 L2 · -58 dBm"
        );

        app.lang = Lang::En;
        assert_eq!(
            line_text(&top_left(&app, &theme)),
            "● connected · link L2 · -58 dBm"
        );
    }

    /// The glyph changes with the state, and takes the state's colour.
    #[test]
    fn the_status_glyph_follows_the_state() {
        let theme = Theme::DARK;
        let mut app = App::default();
        let cases = [
            (ConnectionState::Connected, theme.accent_strong(), "●"),
            (ConnectionState::Connecting, theme.warn(), "◐"),
            (ConnectionState::Failed, theme.danger(), "◆"),
            (ConnectionState::Disconnected, theme.muted(), "○"),
        ];
        for (state, style, glyph) in cases {
            app.state = state;
            let line = top_left(&app, &theme);
            assert_eq!(line.spans[0].style.fg, style.fg, "{state:?}");
            assert_eq!(line.spans[0].content.trim_end(), glyph, "{state:?}");
        }
    }

    /// The traffic unit appears only where it fits: nothing on a classic
    /// 80-column bar, a wider slice for the graph as the bar grows.
    #[test]
    fn the_traffic_unit_grows_with_the_bar() {
        assert_eq!(traffic_unit(80), 0);
        assert_eq!(traffic_unit(91), 0);
        assert_eq!(traffic_unit(92), 13);
        assert_eq!(traffic_unit(103), 13);
        assert_eq!(traffic_unit(104), 18);
        assert_eq!(traffic_unit(119), 18);
        assert_eq!(traffic_unit(120), 24);
        assert_eq!(traffic_unit(200), 24);
    }

    /// The label is the rate in human bytes, held off its neighbours by a
    /// space each side, in the accent the unit has always worn — it names the
    /// scale both graphs share, not either direction on its own.
    #[test]
    fn the_traffic_label_names_the_rate_in_human_bytes() {
        let theme = Theme::DARK;
        let mut app = App {
            lang: Lang::En,
            ..App::default()
        };
        assert_eq!(line_text(&traffic_label(&app, &theme)), " 0 B/s ");
        app.traffic.rate = 1536;
        assert_eq!(line_text(&traffic_label(&app, &theme)), " 1.5 KB/s ");
        assert_eq!(
            traffic_label(&app, &theme).spans[0].style.fg,
            Some(theme.accent)
        );
        assert_eq!(
            traffic_label(&app, &Theme::LIGHT).spans[0].style.fg,
            Some(Theme::LIGHT.accent)
        );
    }

    /// The right half reads counters, speed, autoscroll and the clock, in that
    /// order — and reads the same in both languages, because the tokens in it
    /// are deliberately untranslated.
    #[test]
    fn the_top_bar_right_reads_the_counters_in_order() {
        for lang in [Lang::Zh, Lang::En] {
            let mut app = App {
                lang,
                ..App::default()
            };
            app.rx = 12;
            app.tx = 34;
            app.clock = "19:52:50".to_string();
            let text = line_text(&top_right(&app, &Theme::DARK));
            assert_eq!(
                text, "RX 12 · TX 34 · ─ · Auto · 19:52:50",
                "{lang:?} translates a token the design keeps Latin"
            );
            assert!(!text.ends_with(' '), "{text:?} carries a trailing space");
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
        assert_eq!(text, "RX 0 · TX 0 · 115200 · Auto · 19:52:50");
    }

    /// One frame names the session's state once and the same way: the top bar
    /// and the bottom bar both carry the state's own name, and neither carries
    /// another state's — a frame that said `连接中…` in one place and
    /// `connecting…` in the other would be showing two answers to one question.
    #[test]
    fn the_bars_name_the_state_the_same_way() {
        for lang in [Lang::Zh, Lang::En] {
            for state in [
                ConnectionState::Disconnected,
                ConnectionState::Connecting,
                ConnectionState::Connected,
                ConnectionState::Failed,
            ] {
                let app = App {
                    lang,
                    state,
                    ..App::default()
                };
                let theme = Theme::DARK;
                let top = top_left(&app, &theme);
                let bottom = bottom_left(&app, &theme);
                // The state's name is its own span, so the check is exact rather
                // than a search: `disconnected` contains `connected`.
                assert_eq!(top.spans[1].content.as_ref(), app.state_name());
                assert_eq!(
                    top.spans[0].content.trim_end(),
                    app.state_glyph(),
                    "{lang:?}/{state:?}"
                );
                assert_eq!(
                    bottom.spans.last().unwrap().content.as_ref(),
                    app.state_name()
                );
                assert_eq!(
                    bottom.spans.last().unwrap().style.fg,
                    theme.state_dot(state).fg
                );
            }
        }
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
        assert_eq!(text, "[终端] 网络 / 输入 · 连接中…");

        // The rail names the row the cursor is on, not just the view: a left
        // half that says "侧栏 网络" tells the user which view they are in and
        // nothing about what Enter will do.
        app.focus = crate::app::Focus::Sidebar;
        let text = line_text(&bottom_left(&app, &Theme::DARK));
        assert!(text.starts_with("[侧栏] 网络 / "), "{text}");
        assert!(text.contains("传输"), "the row is named: {text}");
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
        assert_eq!(zh, "Ctrl+P 面板 · F1 帮助 · F2-F5 视图 · Ctrl+Q 退出");
        let en = line_text(&bottom_right(
            &App {
                lang: Lang::En,
                ..App::default()
            },
            &Theme::DARK,
        ));
        assert_eq!(en, "Ctrl+P palette · F1 help · F2-F5 views · Ctrl+Q quit");
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
        // The counters wear the two graph colours, and both move with the
        // theme like everything else.
        let light_right = top_right(&app, &Theme::LIGHT);
        assert_eq!(light_right.spans[0].style.fg, Some(Theme::LIGHT.accent));
        assert_eq!(light_right.spans[2].style.fg, Some(Theme::LIGHT.focus));
        assert_ne!(
            light_right.spans[0].style.fg,
            top_right(&app, &Theme::DARK).spans[0].style.fg
        );
    }
}
