//! Toasts: the short-lived notices that float over the top-right corner.

use ratatui::text::{Line, Span};

use crate::app::{App, Notice};
use crate::theme::Theme;

/// How many columns of padding sit inside a toast's border.
const PADDING: usize = 2;

/// How long before its time a toast starts to dim. The last stretch of a
/// notice's life, which is warning enough to read it as leaving rather than as
/// gone.
const FADE: std::time::Duration = std::time::Duration::from_millis(700);

/// A toast's body: the notice text, one line per notice, in the current theme.
pub fn lines(app: &App, theme: &Theme) -> Vec<Line<'static>> {
    let now = std::time::Instant::now();
    app.notices
        .iter()
        .enumerate()
        .map(|(index, notice)| {
            // The dim belongs to the *stack*, not to the notice: the log
            // keeps every word at full strength, and a notice that outlives
            // its toast is read there.
            let style = match app.toast_until.get(index) {
                Some(until) if until.saturating_duration_since(now) < FADE => theme.muted(),
                _ => theme.toast(),
            };
            Line::from(Span::styled(notice.text.clone(), style))
        })
        .collect()
}

/// The width a toast needs for its text, border and padding included, so the
/// box hugs its content instead of taking a guessed slice of the screen. An
/// empty stack needs no box at all, and says so with zero.
pub fn width(notices: &[Notice]) -> u16 {
    if notices.is_empty() {
        return 0;
    }
    let widest = notices
        .iter()
        .map(|notice| unicode_width::UnicodeWidthStr::width(notice.text.as_str()))
        .max()
        .unwrap_or(0);
    // Two border columns plus the padding on each side.
    (widest + 2 * PADDING + 2) as u16
}

/// The level whose border a stack of toasts should wear: the loudest notice
/// present, so an error is never framed as a mere info.
pub fn border_level(notices: &[Notice]) -> Option<crate::event::NoticeLevel> {
    use crate::event::NoticeLevel;
    let mut level = None;
    for notice in notices {
        let candidate = notice.level;
        level = match (level, candidate) {
            (Some(NoticeLevel::Error), _) | (_, NoticeLevel::Error) => Some(NoticeLevel::Error),
            (Some(NoticeLevel::Warn), _) | (_, NoticeLevel::Warn) => Some(NoticeLevel::Warn),
            _ => Some(NoticeLevel::Info),
        };
    }
    level
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::NoticeLevel;

    fn notice(text: &str, level: NoticeLevel) -> Notice {
        Notice {
            text: text.to_string(),
            level,
        }
    }

    /// No notices, no toast: an empty stack paints nothing and asks for no box.
    #[test]
    fn an_empty_stack_paints_nothing() {
        let app = App::default();
        assert!(lines(&app, &Theme::DARK).is_empty());
        assert_eq!(width(&[]), 0);
        assert_eq!(border_level(&[]), None);
    }

    /// One line per notice, each on the toast ground, whatever its level — the
    /// level shows in the border, never by recolouring the text.
    #[test]
    fn every_notice_gets_one_line_on_the_toast_ground() {
        let mut app = App::default();
        app.push_notice(notice("连接中…", NoticeLevel::Info));
        app.push_notice(notice("boom", NoticeLevel::Error));
        let painted = lines(&app, &Theme::DARK);
        assert_eq!(painted.len(), 2);
        assert_eq!(painted[0].spans[0].content, "连接中…");
        assert_eq!(painted[1].spans[0].content, "boom");
        for line in &painted {
            assert_eq!(line.spans[0].style.bg, Some(Theme::DARK.panel_3));
            assert_eq!(line.spans[0].style.fg, Some(Theme::DARK.text));
        }
    }

    /// A toast dims in its last stretch, and the dim is the stack's business
    /// only: the notice the log holds keeps its full-strength words.
    #[test]
    fn a_toast_dims_before_it_goes() {
        let mut app = App::default();
        app.push_notice(notice("going", NoticeLevel::Info));
        app.push_notice(notice("fresh", NoticeLevel::Info));
        let now = std::time::Instant::now();
        app.toast_until[0] = now;
        app.toast_until[1] = now + crate::app::TOAST_LIFETIME;

        let painted = lines(&app, &Theme::DARK);
        assert_eq!(
            painted[0].spans[0].style,
            Theme::DARK.muted(),
            "the one whose time is up dims"
        );
        assert_eq!(
            painted[1].spans[0].style,
            Theme::DARK.toast(),
            "and a fresh one does not"
        );
    }

    /// The box is as wide as the widest notice plus its chrome: a wide (CJK)
    /// notice must not be clipped by a box sized for a narrow one.
    #[test]
    fn the_box_hugs_the_widest_notice() {
        let narrow = notice("hi", NoticeLevel::Info);
        let wide = notice("主题: 浅色", NoticeLevel::Info);
        let narrow_width = width(&[narrow]);
        let wide_width = width(std::slice::from_ref(&wide));
        assert!(wide_width > narrow_width);
        let widest = unicode_width::UnicodeWidthStr::width(wide.text.as_str()) as u16;
        assert_eq!(wide_width, widest + 6);
    }

    /// The border wears the loudest level in the stack, so an error is never
    /// framed as an info just because an info arrived later.
    #[test]
    fn the_border_wears_the_loudest_level() {
        use NoticeLevel::*;
        assert_eq!(border_level(&[notice("a", Info)]), Some(Info));
        assert_eq!(
            border_level(&[notice("a", Info), notice("b", Warn)]),
            Some(Warn)
        );
        assert_eq!(
            border_level(&[notice("a", Warn), notice("b", Info)]),
            Some(Warn)
        );
        assert_eq!(
            border_level(&[notice("a", Warn), notice("b", Error)]),
            Some(Error)
        );
        assert_eq!(
            border_level(&[notice("a", Error), notice("b", Info)]),
            Some(Error)
        );
        assert_eq!(
            border_level(&[notice("a", Info), notice("b", Error), notice("c", Warn)]),
            Some(Error)
        );
    }
}
