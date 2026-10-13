//! The centre column: what sits beside the sidebar.
//!
//! The serial terminal is the exception among the views — it is a pane the
//! device writes into, not a framed document, so it takes the terminal ground
//! and no border. Every other view is a frame with a title. Views whose bodies
//! are still being built say so honestly instead of showing an empty frame.

use ratatui::text::{Line, Span};

use crate::app::App;
use crate::event::View;
use crate::theme::Theme;

/// Whether a view is drawn as a framed document. The serial terminal is not.
pub fn is_framed(view: View) -> bool {
    !matches!(view, View::Terminal)
}

/// The ground the centre column sits on.
pub fn ground(app: &App, theme: &Theme) -> ratatui::style::Style {
    match app.view {
        View::Terminal => theme.terminal(),
        _ => theme.panel(),
    }
}

/// The body of a view that has no content yet: one muted line that says which
/// view it is, so an unimplemented pane is never mistaken for a broken one.
pub fn lines(app: &App, theme: &Theme) -> Vec<Line<'static>> {
    if !is_framed(app.view) {
        return Vec::new();
    }
    vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("  {}", app.strings().view_empty),
            theme.muted(),
        )),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Lang;

    /// The serial terminal is the only full-bleed view: it takes the terminal
    /// ground and no frame, so the device's output is not fenced in.
    #[test]
    fn the_serial_terminal_is_the_only_unframed_view() {
        for view in View::ALL {
            let framed = is_framed(view);
            assert_eq!(framed, view != View::Terminal, "{view:?}");
        }
    }

    /// The ground follows the view: the terminal pane gets the terminal colour,
    /// every framed view gets the panel.
    #[test]
    fn the_ground_follows_the_view() {
        let theme = Theme::DARK;
        for view in View::ALL {
            let app = App {
                view,
                ..App::default()
            };
            let expected = if view == View::Terminal {
                theme.terminal_bg
            } else {
                theme.panel
            };
            assert_eq!(ground(&app, &theme).bg, Some(expected), "{view:?}");
        }
    }

    /// An unfinished view says so, in the current language, in muted text —
    /// never a bare frame.
    #[test]
    fn an_unfinished_view_says_so() {
        let theme = Theme::DARK;
        let mut app = App {
            lang: Lang::Zh,
            ..App::default()
        };
        app.view = View::Assistant;
        let painted = lines(&app, &theme);
        let text: String = painted
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        assert!(text.contains("尚未实现"), "{text:?}");
        for line in &painted {
            for span in &line.spans {
                assert_eq!(span.style.fg, Some(theme.muted));
            }
        }
        app.lang = Lang::En;
        let text: String = lines(&app, &theme)
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        assert!(text.contains("not implemented"), "{text:?}");
    }

    /// The terminal pane has no placeholder: it is empty until the device
    /// writes, which is what the designed first frame shows.
    #[test]
    fn the_terminal_pane_has_no_placeholder() {
        let app = App {
            view: View::Terminal,
            ..App::default()
        };
        assert!(lines(&app, &Theme::DARK).is_empty());
    }
}
