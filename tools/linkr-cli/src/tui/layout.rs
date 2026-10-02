//! Frame composition: top status bar, left sidebar, center view, bottom status
//! line plus the overlay layer (palette, dialogs, toasts).
//!
//! Everything here renders from the state in [`super::state::App`]; no side
//! effects, so the geometry maths is easy to follow.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::Frame;

use super::state::{App, Focus, View};
use super::status::StatusModel;
use crate::event::NoticeLevel;

/// Sidebar width: 30 columns, shrunk on narrow terminals.
pub const SIDEBAR_WIDTH: u16 = 30;

/// Split the frame into (top, body, bottom).
pub fn zones(area: Rect) -> (Rect, Rect, Rect) {
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .split(area);
    (rows[0], rows[1], rows[2])
}

/// Split the body into (sidebar, center).
pub fn columns(body: Rect, sidebar_visible: bool) -> (Option<Rect>, Rect) {
    if !sidebar_visible {
        return (None, body);
    }
    let width = SIDEBAR_WIDTH.min(body.width.saturating_sub(20).max(10));
    let cols = Layout::horizontal([Constraint::Length(width), Constraint::Min(10)]).split(body);
    (Some(cols[0]), cols[1])
}

/// Build the status model from the live session snapshot.
pub fn status_model(app: &App) -> StatusModel {
    StatusModel {
        state: app.state,
        transport: app.info.kind,
        label: app.info.label.clone(),
        device_id: app.info.device_id.clone(),
        rx_bytes: app.info.rx_bytes,
        tx_bytes: app.info.tx_bytes,
        baud: app.info.baud,
        mode: app.exec_mode,
        clock: chrono::Local::now().format("%H:%M:%S").to_string(),
        detail: app.detail.clone(),
    }
}

/// Center body of the current view as plain lines.
pub fn center_lines(app: &App, width: u16) -> Vec<Line<'static>> {
    match app.view {
        View::Terminal => terminal_lines(app, width),
        View::Diagnostics => super::diagnostics_view::render_lines(app, width),
        View::Network => super::network_view::render_lines(app),
        View::Assistant => super::assistant_view::render_lines(app, width),
    }
}

fn terminal_lines(app: &App, width: u16) -> Vec<Line<'static>> {
    let (_cols, rows) =
        super::terminal_view::grid_dims(width, app.center_height, app.settings.font_size);
    let view = app.terminal.grid.render(rows, app.terminal.offset);
    let mut lines = view.lines;
    lines.truncate(rows as usize);
    lines
}

/// Render the whole frame.
pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let (top, body, bottom) = zones(area);
    let sidebar_visible = area.width >= 60;
    let (sidebar, center) = columns(body, sidebar_visible);

    // Top status bar.
    let model = status_model(app);
    frame.render_widget(
        ratatui::widgets::Paragraph::new(super::status::status_line(&model, top.width))
            .style(Style::default().bg(Color::Black)),
        top,
    );

    // Sidebar.
    if let Some(rect) = sidebar {
        let lines = super::sidebar::render_lines(app);
        frame.render_widget(
            ratatui::widgets::Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: false }),
            rect,
        );
        // Column separator.
        let separator = Rect {
            x: rect.x + rect.width.saturating_sub(1),
            y: rect.y,
            width: 1,
            height: rect.height,
        };
        frame.render_widget(
            ratatui::widgets::Paragraph::new(
                (0..separator.height)
                    .map(|_| Line::from(Span::styled("│", Style::default().fg(Color::DarkGray))))
                    .collect::<Vec<_>>(),
            ),
            separator,
        );
    }

    // Center pane.
    let lines = center_lines(app, center.width);
    frame.render_widget(
        ratatui::widgets::Paragraph::new(lines)
            .scroll((app.center_scroll, 0))
            .wrap(ratatui::widgets::Wrap { trim: false }),
        center,
    );

    // Bottom status line.
    let bottom_text = super::status::bottom_line(
        app.focus.label(),
        app.view.label(),
        &app.detail,
        bottom.width,
    );
    frame.render_widget(
        ratatui::widgets::Paragraph::new(Line::from(Span::styled(
            bottom_text,
            Style::default().fg(Color::Gray),
        )))
        .style(Style::default().bg(Color::Black)),
        bottom,
    );

    draw_toasts(frame, app, area);

    if app.palette.is_some() {
        draw_palette(frame, app, area);
    } else if app.dialog.is_some() {
        draw_dialog(frame, app, area);
    }
}

// --- overlays ----------------------------------------------------------------

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width.saturating_sub(2).max(10));
    let height = height.min(area.height.saturating_sub(2).max(3));
    Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    }
}

fn draw_toasts(frame: &mut Frame, app: &App, area: Rect) {
    let toasts: Vec<&super::state::Toast> = app.notices.toasts.iter().collect();
    if toasts.is_empty() {
        return;
    }
    let width = toasts
        .iter()
        .map(|toast| unicode_width::UnicodeWidthStr::width(toast.text.as_str()) + 4)
        .max()
        .unwrap_or(20)
        .min(area.width.saturating_sub(4).max(10) as usize) as u16;
    let height = toasts.len() as u16;
    let rect = Rect {
        x: area.x + area.width.saturating_sub(width + 1),
        y: area.y + 1,
        width,
        height,
    };
    let lines: Vec<Line> = toasts
        .iter()
        .map(|toast| {
            let style = match toast.level {
                NoticeLevel::Info => Style::default().fg(Color::Cyan),
                NoticeLevel::Warn => Style::default().fg(Color::LightYellow),
                NoticeLevel::Error => Style::default().fg(Color::LightRed),
            };
            Line::from(Span::styled(
                format!(" {}", toast.text),
                style.bg(Color::Black),
            ))
        })
        .collect();
    frame.render_widget(ratatui::widgets::Clear, rect);
    frame.render_widget(ratatui::widgets::Paragraph::new(lines), rect);
}

fn draw_palette(frame: &mut Frame, app: &App, area: Rect) {
    let lines = super::palette::render_lines(app);
    let height = (lines.len() as u16 + 2).min(area.height.saturating_sub(2).max(3));
    let rect = centered(area, 72, height);
    frame.render_widget(ratatui::widgets::Clear, rect);
    frame.render_widget(
        ratatui::widgets::Block::default()
            .borders(ratatui::widgets::Borders::ALL)
            .border_type(ratatui::widgets::BorderType::Rounded)
            .border_style(Style::default().fg(Color::Cyan))
            .title(" Command palette (Ctrl+P) "),
        rect,
    );
    let inner = Rect {
        x: rect.x + 1,
        y: rect.y + 1,
        width: rect.width.saturating_sub(2),
        height: rect.height.saturating_sub(2),
    };
    frame.render_widget(
        ratatui::widgets::Paragraph::new(lines).scroll((
            app.palette
                .as_ref()
                .map(|state| state.selected.saturating_sub(9) as u16)
                .unwrap_or(0),
            0,
        )),
        inner,
    );
    if let Some(state) = &app.palette {
        let col = 2 + unicode_width::UnicodeWidthStr::width(state.query.as_str()) as u16;
        frame.set_cursor_position((inner.x + col.min(inner.width.saturating_sub(1)), inner.y));
    }
}

fn draw_dialog(frame: &mut Frame, app: &App, area: Rect) {
    let lines = super::dialogs::render_lines(app, 74);
    let title = app
        .dialog
        .as_ref()
        .map(|dialog| dialog.title())
        .unwrap_or("Dialog");
    let height = (lines.len() as u16 + 2).min(area.height.saturating_sub(2).max(3));
    let rect = centered(area, 78, height);
    frame.render_widget(ratatui::widgets::Clear, rect);
    frame.render_widget(
        ratatui::widgets::Block::default()
            .borders(ratatui::widgets::Borders::ALL)
            .border_type(ratatui::widgets::BorderType::Rounded)
            .border_style(Style::default().fg(Color::Yellow))
            .title(format!(" {title} ")),
        rect,
    );
    let inner = Rect {
        x: rect.x + 1,
        y: rect.y + 1,
        width: rect.width.saturating_sub(2),
        height: rect.height.saturating_sub(2),
    };
    frame.render_widget(
        ratatui::widgets::Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: false }),
        inner,
    );
}

/// Focus label used when nothing else says it (kept here for the tests).
pub fn focus_label(focus: Focus) -> &'static str {
    focus.label()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(width: u16, height: u16) -> Rect {
        Rect::new(0, 0, width, height)
    }

    #[test]
    fn zones_reserve_one_row_top_and_bottom() {
        let (top, body, bottom) = zones(area(80, 24));
        assert_eq!(top.height, 1);
        assert_eq!(bottom.height, 1);
        assert_eq!(body.height, 22);
        assert_eq!(top.y, 0);
        assert_eq!(body.y, 1);
        assert_eq!(bottom.y, 23);
    }

    #[test]
    fn columns_hide_the_sidebar_on_narrow_terminals() {
        let (sidebar, center) = columns(area(50, 20), false);
        assert!(sidebar.is_none());
        assert_eq!(center.width, 50);

        let (sidebar, center) = columns(area(120, 20), true);
        assert_eq!(sidebar.unwrap().width, SIDEBAR_WIDTH);
        assert_eq!(center.width, 120 - SIDEBAR_WIDTH);
    }

    #[test]
    fn narrow_sidebar_does_not_eat_the_center() {
        let (sidebar, center) = columns(area(45, 20), true);
        let width = sidebar.unwrap().width;
        assert!(width <= 45 - 10, "sidebar too wide: {width}");
        assert!(center.width >= 10);
    }

    #[test]
    fn centered_box_stays_inside_the_frame() {
        let rect = centered(area(40, 10), 78, 30);
        assert!(rect.width <= 38);
        assert!(rect.height <= 8);
        assert!(rect.x + rect.width <= 40);
        assert!(rect.y + rect.height <= 10);
    }
}
