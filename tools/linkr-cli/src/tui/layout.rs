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
        transport: app.live_kind(),
        label: app.info.label.clone(),
        device_id: app.info.device_id.clone(),
        rx_bytes: app.info.rx_bytes,
        tx_bytes: app.info.tx_bytes,
        baud: app.info.baud,
        mode: app.exec_mode,
        clock: chrono::Local::now().format("%H:%M:%S").to_string(),
        detail: app.detail.clone(),
        lang: app.lang(),
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

/// Rows a `lines`-long body can scroll through a `rows`-tall pane. Scrolling
/// past this shows nothing: the offset is applied after rendering, so an
/// unclamped value walks the pane blank (same failure as the palette had).
pub fn scroll_limit(lines: usize, rows: u16) -> u16 {
    (lines as u16).saturating_sub(rows)
}

/// "As far down as the content allows" ([`page_scroll`] resolves it before
/// moving, so it can be stored without knowing the current geometry). The
/// assistant pins to it: the composer is the last line of the transcript.
pub const PIN_END: u16 = u16::MAX;

/// One page of a `Paragraph` scroll offset. The offset counts lines skipped
/// from the top, so `up` walks towards the start — the old code added on
/// PageUp, which paged *down*, and never checked the limit.
pub fn page_scroll(current: u16, limit: u16, step: u16, up: bool) -> u16 {
    let base = current.min(limit);
    if up {
        base.saturating_sub(step)
    } else {
        (base.saturating_add(step)).min(limit)
    }
}

/// [`scroll_limit`] for the center pane as the key handler sees it. The
/// renderer clamps against the live frame; both read the geometry that
/// `sync_geometry` writes every iteration, so they agree.
pub fn center_scroll_limit(app: &App) -> u16 {
    scroll_limit(center_lines(app, app.center_width).len(), app.center_height)
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
    let offset = app
        .center_scroll
        .min(scroll_limit(lines.len(), center.height));
    frame.render_widget(
        ratatui::widgets::Paragraph::new(lines)
            .scroll((offset, 0))
            .wrap(ratatui::widgets::Wrap { trim: false }),
        center,
    );

    // Bottom status line.
    let lang = app.lang();
    let bottom_text = super::status::bottom_line(
        app.focus.label(lang),
        app.view.label(lang),
        &app.detail,
        bottom.width,
        lang,
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
    let Some(state) = &app.palette else {
        return;
    };
    // Height tracks how many actions match — never the selection — so the box
    // stays put while the list scrolls. The old sizing used `lines.len()`,
    // which shrank as the window slid and, together with a second scroll of an
    // already-windowed list, walked everything off the top of the box.
    const BORDER: u16 = 2;
    const HEADER: u16 = 2; // prompt line + separator
    let rows = super::palette::matches(&state.query)
        .len()
        .clamp(1, super::palette::MAX_VISIBLE) as u16;
    let rect = centered(area, 72, BORDER + HEADER + rows);
    frame.render_widget(ratatui::widgets::Clear, rect);
    frame.render_widget(
        ratatui::widgets::Block::default()
            .borders(ratatui::widgets::Borders::ALL)
            .border_type(ratatui::widgets::BorderType::Rounded)
            .border_style(Style::default().fg(Color::Cyan))
            .title(super::palette::panel_title(state.lang)),
        rect,
    );
    let inner = Rect {
        x: rect.x + 1,
        y: rect.y + 1,
        width: rect.width.saturating_sub(2),
        height: rect.height.saturating_sub(2),
    };
    // The window is computed once, in `render_lines`; the paragraph never
    // scrolls, so the prompt line the cursor sits on cannot drift away.
    let lines =
        super::palette::render_lines(state, usize::from(inner.height.saturating_sub(HEADER)));
    frame.render_widget(ratatui::widgets::Paragraph::new(lines), inner);
    let col = 2 + unicode_width::UnicodeWidthStr::width(state.query.as_str()) as u16;
    frame.set_cursor_position((inner.x + col.min(inner.width.saturating_sub(1)), inner.y));
}

fn draw_dialog(frame: &mut Frame, app: &App, area: Rect) {
    let lines = super::dialogs::render_lines(app, super::dialogs::DIALOG_WIDTH);
    let title = app
        .dialog
        .as_ref()
        .map(|dialog| dialog.title_lang(app.lang()))
        .unwrap_or(super::i18n::t(
            super::dialogs::DLG_TITLE_FALLBACK,
            app.lang(),
        ));
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
    // Scrollable overlays (help / notices) offset their body; the clamp keeps
    // a stale offset from painting an empty box after a terminal resize.
    let scroll = app
        .dialog
        .as_ref()
        .map(super::dialogs::Dialog::scroll_offset)
        .unwrap_or(0)
        .min((lines.len() as u16).saturating_sub(inner.height));
    frame.render_widget(
        ratatui::widgets::Paragraph::new(lines)
            .wrap(ratatui::widgets::Wrap { trim: false })
            .scroll((scroll, 0)),
        inner,
    );
}

/// Focus label used when nothing else says it (kept here for the tests).
pub fn focus_label(focus: Focus, lang: super::i18n::Lang) -> &'static str {
    focus.label(lang)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(width: u16, height: u16) -> Rect {
        Rect::new(0, 0, width, height)
    }

    /// The offset counts lines skipped from the top, so PageUp walks towards
    /// the start. The old handler *added* on PageUp — paging down — and never
    /// checked the limit, which walked the pane blank (the same failure the
    /// command palette had).
    #[test]
    fn paging_follows_the_offset_direction_and_stays_inside_the_content() {
        assert_eq!(scroll_limit(40, 22), 18, "40 lines in a 22-row pane");
        assert_eq!(scroll_limit(10, 22), 0, "content that fits cannot scroll");
        assert_eq!(scroll_limit(0, 22), 0);

        // PageUp moves towards the start (a smaller offset).
        assert_eq!(page_scroll(18, 18, 5, true), 13);
        assert_eq!(page_scroll(5, 18, 5, true), 0, "clamped at the top");
        assert_eq!(page_scroll(0, 18, 5, true), 0);
        // PageDown moves towards the end and stops at the limit.
        assert_eq!(page_scroll(0, 18, 5, false), 5);
        assert_eq!(page_scroll(15, 18, 5, false), 18, "clamped at the bottom");
        assert_eq!(page_scroll(18, 18, 5, false), 18);
        assert_eq!(page_scroll(0, 0, 5, false), 0, "no room to move");
    }

    /// The assistant stores "pin to the end" without knowing the geometry;
    /// the value has to resolve to the limit *before* it moves, or PageUp from
    /// the newest message would clatter against the bottom and do nothing.
    #[test]
    fn the_pin_to_end_sentinel_resolves_before_paging() {
        assert_eq!(page_scroll(PIN_END, 18, 5, true), 13, "PageUp walks up");
        assert_eq!(page_scroll(PIN_END, 18, 5, false), 18, "PageDown stays");
        assert_eq!(page_scroll(PIN_END, 0, 5, true), 0, "content that fits");
    }

    /// A stale offset (the transcript shrank, the terminal resized) must paint
    /// the end of the content rather than an empty pane.
    #[test]
    fn a_stale_offset_paints_the_content_not_a_blank_pane() {
        let lines = 25usize;
        let rows = 20u16;
        let limit = scroll_limit(lines, rows);
        assert_eq!(limit, 5);
        for stale in [0u16, 1, 5, 500, PIN_END] {
            let painted = stale.min(limit);
            assert!(
                lines - painted as usize >= rows as usize,
                "offset {stale} painted past the pane: {lines} - {painted} < {rows}"
            );
        }
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

    /// `SessionInfo::kind` is written when the session is built and teardown
    /// never clears it, so the bottom bar kept printing `Disconnected · BLE`
    /// after the link it named was gone. Only a session that still exists
    /// may name a transport (an attempt in flight still may).
    #[test]
    fn the_status_bar_names_a_transport_only_while_a_session_exists() {
        use crate::event::ConnectionState;

        let mut app = crate::tui::test_app();
        app.info.kind = Some(crate::transport::TransportKind::Ble);

        app.state = ConnectionState::Disconnected;
        assert_eq!(status_model(&app).transport, None, "the link is gone");

        app.state = ConnectionState::Connecting;
        assert_eq!(
            status_model(&app).transport,
            Some(crate::transport::TransportKind::Ble),
            "an attempt is still dialled over something"
        );

        app.state = ConnectionState::Connected;
        assert_eq!(
            status_model(&app).transport,
            Some(crate::transport::TransportKind::Ble)
        );
    }
}
