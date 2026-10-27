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
            Control::Assistant,
            if compact { "[F6]" } else { "[F6 AI]" },
            assistant_visible,
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
