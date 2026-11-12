//! Always-visible panel controls. Paint and hit testing share one measured row.

use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};

use crate::app::App;
use crate::event::View;
use crate::i18n::Lang;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    Sidebar,
    Assistant,
    Focus,
}

pub struct Bar {
    pub line: Line<'static>,
    targets: Vec<(u16, u16, Control)>,
}

pub fn bar(app: &App, screen: Rect) -> Bar {
    let area = super::regions_for(screen, app.sidebar_visible, app.panel_wanted()).bottom_bar;
    let theme = app.theme();
    let zh = app.lang == Lang::Zh;
    let compact = area.width < 60;
    let region = super::regions_for(screen, app.sidebar_visible, app.panel_wanted());
    let sidebar_visible = region.sidebar.width > 0 || super::sidebar_drawer(app, screen).is_some();
    let assistant_visible = region.assistant.width > 0 || app.view == View::Assistant;
    let focus = app.layout_restore.is_some();
    let buttons = [
        (
            Control::Assistant,
            if compact { "[F6]" } else { "[F6 AI]" },
            assistant_visible,
        ),
        (
            Control::Sidebar,
            if compact {
                "[F7]"
            } else if zh {
                "[F7 左栏]"
            } else {
                "[F7 Nav]"
            },
            sidebar_visible,
        ),
        (
            Control::Focus,
            if compact {
                "[F8]"
            } else if focus {
                if zh {
                    "[F8 还原]"
                } else {
                    "[F8 Restore]"
                }
            } else if zh {
                "[F8 专注]"
            } else {
                "[F8 Focus]"
            },
            focus,
        ),
    ];
    let mut spans = Vec::new();
    let mut targets = Vec::new();
    let mut used = 0;
    for (action, label, active) in buttons {
        let width = crate::text::columns(label) as u16;
        let gap = u16::from(used > 0);
        if used + gap + width > area.width {
            break;
        }
        if gap > 0 {
            spans.push(Span::raw(" "));
            used += gap;
        }
        targets.push((used, used + width, action));
        spans.push(Span::styled(
            label,
            if active {
                theme.accent_strong()
            } else {
                theme.muted()
            },
        ));
        used += width;
    }
    let help = if area.width >= 110 {
        if zh {
            " · Ctrl+P · F1 帮助 · Ctrl+Q 退出"
        } else {
            " · Ctrl+P · F1 help · Ctrl+Q quit"
        }
    } else if area.width >= 90 {
        if zh {
            " · F1 帮助 · Ctrl+Q 退出"
        } else {
            " · F1 help · Ctrl+Q quit"
        }
    } else {
        " F1 Ctrl+Q"
    };
    if used as usize + crate::text::columns(help) <= area.width as usize {
        spans.push(Span::styled(help, theme.muted()));
    }
    Bar {
        line: Line::from(spans),
        targets,
    }
}

pub fn hit(app: &App, screen: Rect, point: Position) -> Option<Control> {
    let area = super::regions_for(screen, app.sidebar_visible, app.panel_wanted()).bottom_bar;
    if !area.contains(point) {
        return None;
    }
    let row = bar(app, screen);
    let start = area.right().saturating_sub(row.line.width() as u16);
    row.targets.into_iter().find_map(|(left, right, action)| {
        (point.x >= start + left && point.x < start + right).then_some(action)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(width: u16, height: u16) -> Rect {
        Rect {
            x: 0,
            y: 0,
            width,
            height,
        }
    }

    fn labels(app: &App, width: u16) -> Vec<String> {
        bar(app, screen(width, 24))
            .line
            .spans
            .into_iter()
            .map(|span| span.content.to_string())
            .filter(|span| span.starts_with('['))
            .collect()
    }

    /// The panel keys read left to right in the order their function keys go:
    /// F6 assistant, F7 sidebar, F8 focus. A screen too narrow for the words
    /// keeps the same order, and so does Chinese.
    #[test]
    fn the_panel_keys_run_f6_f7_f8() {
        let mut app = App {
            lang: Lang::En,
            ..App::default()
        };
        assert_eq!(
            labels(&app, 120)[..3],
            ["[F6 AI]", "[F7 Nav]", "[F8 Focus]"]
        );
        assert_eq!(labels(&app, 50)[..3], ["[F6]", "[F7]", "[F8]"]);

        app.lang = Lang::Zh;
        assert_eq!(
            labels(&app, 120)[..3],
            ["[F6 AI]", "[F7 左栏]", "[F8 专注]"]
        );
        assert_eq!(labels(&app, 50)[..3], ["[F6]", "[F7]", "[F8]"]);
    }

    /// Each label's hit box follows the label, whichever order the row is in.
    #[test]
    fn each_key_hits_the_control_under_it() {
        let app = App {
            lang: Lang::En,
            ..App::default()
        };
        let row = bar(&app, screen(120, 24));
        let (left, right, action) = row.targets[0];
        let point = Position::new(
            screen(120, 24).right() - row.line.width() as u16 + left,
            screen(120, 24).bottom() - 1,
        );
        assert_eq!(action, Control::Assistant);
        assert!(hit(&app, screen(120, 24), point) == Some(Control::Assistant));
        assert!(left < right);
    }
}
