//! The centre column: what sits beside the sidebar.
//!
//! The serial terminal is the exception among the views — it is a pane the
//! device writes into, not a framed document, so it takes the terminal ground
//! and no border. Every other view is a frame with a title. Views whose bodies
//! are still being built say so honestly instead of showing an empty frame.
//!
//! A body is a *pure function of the state plus the width it was given*: the
//! composition layer knows how wide the pane is and passes it down, so a view
//! never guesses its own size and never paints past it. That is the whole
//! contract, and it is why a body can be asserted on without a terminal.

use ratatui::text::{Line, Span};

use crate::app::App;
use crate::event::View;
use crate::theme::Theme;
use crate::views;

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

/// The body of a view, as lines for a pane `width` columns wide.
pub fn lines(app: &App, theme: &Theme, width: u16) -> Vec<Line<'static>> {
    match app.view {
        // A pane, not a document: the device's output is the content, and until
        // it writes there is nothing — which is what the design's first frame
        // shows.
        View::Terminal => Vec::new(),
        View::Diagnostics => views::diagnostics::lines(app, theme, width),
        _ => unfinished(app, theme, width),
    }
}

/// The body of a view that has no content yet: one muted line that says which
/// view it is, so an unimplemented pane is never mistaken for a broken one.
fn unfinished(app: &App, theme: &Theme, width: u16) -> Vec<Line<'static>> {
    vec![
        Line::from(""),
        Line::from(Span::styled(
            crate::text::clip_to(&format!("  {}", app.strings().view_empty), width as usize),
            theme.muted(),
        )),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Lang;

    /// The text of a body, for assertions about what a pane shows.
    fn text_of(rows: &[Line<'static>]) -> String {
        rows.iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect()
    }

    /// The serial terminal is the only full-bleed view: it takes the terminal
    /// ground and no frame, so the device's output is not fenced in.
    #[test]
    fn the_serial_terminal_is_the_only_unframed_view() {
        for view in View::ALL {
            assert_eq!(is_framed(view), view != View::Terminal, "{view:?}");
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

    /// Every view has a body that says something, and the only silent one is the
    /// terminal: a pane waiting for the device.
    #[test]
    fn only_the_terminal_pane_is_silent() {
        let theme = Theme::DARK;
        for view in View::ALL {
            let app = App {
                view,
                ..App::default()
            };
            let rows = lines(&app, &theme, 60);
            if view == View::Terminal {
                assert!(rows.is_empty(), "{view:?} painted a placeholder");
            } else {
                assert!(
                    !text_of(&rows).trim().is_empty(),
                    "{view:?} painted nothing"
                );
            }
        }
    }

    /// An unfinished view says so, in the current language, in muted text —
    /// never a bare frame. Diagnostics is no longer one of them.
    #[test]
    fn an_unfinished_view_says_so() {
        let theme = Theme::DARK;
        let mut app = App {
            lang: Lang::Zh,
            ..App::default()
        };
        app.view = View::Assistant;
        let painted = lines(&app, &theme, 60);
        let text = text_of(&painted);
        assert!(text.contains("尚未实现"), "{text:?}");
        for line in &painted {
            for span in &line.spans {
                assert_eq!(span.style.fg, Some(theme.muted));
            }
        }
        app.lang = Lang::En;
        assert!(text_of(&lines(&app, &theme, 60)).contains("not implemented"));

        // The diagnostics view is built, so it no longer says that.
        app.view = View::Diagnostics;
        let text = text_of(&lines(&app, &theme, 60));
        assert!(!text.contains("not implemented"), "{text:?}");
        assert!(text.contains("Diagnostics"), "{text:?}");
    }

    /// A view's body is told how wide the pane is, and never paints past it —
    /// the contract the composition layer relies on.
    #[test]
    fn a_body_never_paints_past_the_width_it_was_given() {
        let theme = Theme::DARK;
        for width in [4u16, 12, 30, 80] {
            for view in View::ALL {
                let app = App {
                    view,
                    ..App::default()
                };
                for line in lines(&app, &theme, width) {
                    let joined: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                    assert!(
                        crate::text::columns(&joined) <= width as usize,
                        "{view:?} at {width}: {joined:?}"
                    );
                }
            }
        }
    }
}
