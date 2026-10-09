//! Frame composition: top status bar, left sidebar, center view, bottom status
//! line plus the overlay layer (palette, dialogs, toasts).
//!
//! Everything here renders from the state in [`super::state::App`]; no side
//! effects, so the geometry maths is easy to follow.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::Frame;

use super::state::{App, View};
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
        View::Transfer => super::transfer_view::render_lines(app, width),
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
    // The drag selection, reversed straight into the rows it covers. The
    // selection names absolute rows, so it belongs to the text and not to the
    // screen: scrolling the pane moves the highlight with the content, and a
    // row outside the viewport is simply never visited here.
    let selection = app.terminal.selection.filter(|it| !it.is_empty());
    if let Some(selection) = selection {
        let ((start_row, start_col), (end_row, end_col)) = selection.range();
        for (index, line) in lines.iter_mut().enumerate() {
            let row = view.start + index;
            if row < start_row || row > end_row {
                continue;
            }
            let from = if row == start_row { start_col } else { 0 };
            let to = if row == end_row {
                end_col.saturating_add(1)
            } else {
                usize::MAX
            };
            invert_columns(line, from, to);
            if from == 0 && to == usize::MAX && line_width(line) == 0 {
                // A blank row inside the block would read as a hole in it:
                // give it the pane's width, reversed, like every terminal.
                line.spans.push(Span::styled(
                    " ".repeat(usize::from(width)),
                    Style::default().reversed(),
                ));
            }
        }
    }
    // The grid knows where the caret is and whether the device asked for it
    // to be shown (`GridView::cursor`), but nothing ever drew it: a pane with
    // no caret is one you type into blind. The mark lives inside the line, so
    // it stays on the right cell whatever the pane is then scrolled by.
    //
    // Not while a selection covers it, though: inverting a cell that is
    // already inverted would punch a hole in the block, and a hole reads as
    // "this cell is not selected" while it would still be copied.
    if selection.is_none() {
        if let Some((row, col)) = view.cursor {
            if let Some(line) = lines.get_mut(row as usize) {
                mark_cursor(line, col);
            }
        }
    }
    lines
}

/// Display columns taken up by a rendered row.
fn line_width(line: &Line<'static>) -> usize {
    line.spans
        .iter()
        .map(|span| unicode_width::UnicodeWidthStr::width(span.content.as_ref()))
        .sum()
}

/// Reverse the display columns `[from, to)` of one rendered row: one cell for
/// the caret, whatever a drag covered for a selection.
///
/// `cells_to_line` merges neighbouring cells that share a style into one span,
/// so the span is walked glyph by glyph and re-cut into the three runs (before,
/// inside, after) that the range splits it into. A glyph that straddles an edge
/// counts as *inside* — the same rule [`super::terminal_view`] cuts a selection
/// with — because inverting half of a wide character paints nothing at all.
fn invert_columns(line: &mut Line<'static>, from: usize, to: usize) {
    if from >= to {
        return;
    }
    let mut out: Vec<Span<'static>> = Vec::with_capacity(line.spans.len() + 2);
    let mut column = 0usize;
    for span in std::mem::take(&mut line.spans) {
        let text: String = span.content.into();
        // 0 = not reached the range yet, 1 = inside it, 2 = past it. The
        // columns only move forward, so the three runs never interleave.
        let mut run = 0usize;
        let mut runs = [String::new(), String::new(), String::new()];
        for ch in text.chars() {
            let width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            let inside = column < to && column + width > from;
            column += width;
            run = if inside {
                1
            } else if run == 1 {
                2
            } else {
                run
            };
            runs[run].push(ch);
        }
        for (index, chunk) in runs.into_iter().enumerate() {
            if chunk.is_empty() {
                continue;
            }
            let style = if index == 1 {
                span.style.reversed()
            } else {
                span.style
            };
            out.push(Span::styled(chunk, style));
        }
    }
    line.spans = out;
}

/// Invert the single cell the caret sits on — a block cursor, the way every
/// terminal draws one.
///
/// `cells_to_line` drops the trailing blanks of a row, so a caret past the
/// last non-blank cell would sit on a cell that no longer exists: the blanks
/// are drawn back up to it.
fn mark_cursor(line: &mut Line<'static>, col: u16) {
    let col = usize::from(col);
    let width = line_width(line);
    if col >= width {
        let style = line.spans.last().map(|span| span.style).unwrap_or_default();
        line.spans
            .push(Span::styled(" ".repeat(col - width + 1), style.reversed()));
        return;
    }
    invert_columns(line, col, col + 1);
}

/// Cut one row to `budget` display columns, keeping the style of every span
/// that survives. Without this ratatui wraps an over-wide row, and in a fixed
/// height pane with no scroll that pushes everything below it down and off
/// screen.
fn clip_line(line: Line<'static>, budget: u16) -> Line<'static> {
    let text: String = line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect();
    if unicode_width::UnicodeWidthStr::width(text.as_str()) <= budget as usize {
        return line;
    }
    if budget == 0 {
        return Line::default();
    }
    // One column is kept back for the ellipsis, exactly like
    // `dialogs::clip_columns`.
    let room = budget - 1;
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(line.spans.len());
    let mut used = 0u16;
    let mut cut = false;
    for span in line.spans {
        if cut {
            break;
        }
        let content: String = span.content.into();
        let mut kept = String::new();
        for ch in content.chars() {
            let width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0) as u16;
            if used + width > room {
                cut = true;
                break;
            }
            kept.push(ch);
            used += width;
        }
        spans.push(Span::styled(kept, span.style));
    }
    if cut {
        let style = spans.last().map(|span| span.style).unwrap_or_default();
        spans.push(Span::styled("…", style));
    }
    Line::from(spans)
}

/// How many physical rows `line` occupies once `.wrap()` has broken it to
/// `width` display columns — the sidebar's text column, separator included in
/// the pane but not in `width`.
///
/// Chinese sidebar rows carry no spaces to break at, so the emulator fills
/// every row to the last column and the count is the ceiling of the width —
/// which is what this returns. It is the *lower* bound for text that does
/// break at spaces (a word cannot straddle a row), so a pane that fits this
/// count may still drop its last row: one row, at the bottom, which is the
/// same row the ellipsis would have taken anyway.
fn wrapped_rows(line: &Line<'_>, width: u16) -> u16 {
    if width == 0 {
        return 1;
    }
    let text: String = line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect();
    let cells = unicode_width::UnicodeWidthStr::width(text.as_str()) as u16;
    cells.div_ceil(width).max(1)
}

/// The pane row and column the caret of the field being edited occupies.
///
/// A view reports its caret as `(line index, column)` in the body
/// [`center_lines`] returns, and this turns that into a screen position. Two
/// corrections are needed: the scroll offset the paragraph is drawn with, and
/// the paragraph's own wrapping — a line wider than the pane occupies more
/// than one row, so the rows above the caret are counted exactly the way the
/// sidebar counts them ([`wrapped_rows`]).
///
/// The caret is *reported* and then handed to the terminal's cursor rather
/// than painted as a reversed cell. A cell of its own would push a full-width
/// row one column past the pane and wrap it onto the line below, and a cursor
/// blinks — which is most of what makes one recognisable as a cursor.
fn center_caret(
    app: &App,
    lines: &[Line<'static>],
    width: u16,
    offset: u16,
    center: Rect,
) -> Option<(u16, u16)> {
    let (index, column) = match app.view {
        View::Transfer => super::transfer_view::render_lines_at(app, width).1?,
        View::Network => super::network_view::render_lines_at(app).1?,
        _ => return None,
    };
    if index >= lines.len() {
        return None;
    }
    let above: u16 = lines[..index]
        .iter()
        .map(|line| wrapped_rows(line, width))
        .sum();
    let row = above.saturating_sub(offset);
    if row >= center.height {
        return None;
    }
    // A column at or past the pane's width would park the cursor one cell
    // beyond the last one, which every terminal wraps to the next line —
    // the cursor then appears somewhere the text is not. The last cell
    // inside the pane is the closest honest answer.
    let column = u16::try_from(column)
        .unwrap_or(u16::MAX)
        .min(center.width.saturating_sub(1));
    Some((row, column))
}

/// Symbols conhost paints two columns wide while the layout counted one — so
/// each of them shoves the rest of the row a column right, and a full status
/// line runs off the end of its pane and lands in the sidebar. Columns as
/// measured on a cp936 console, beside what unicode-width (and therefore this
/// layout) counts for the same glyph:
///
/// ```text
/// original  conhost  counted   substitute  conhost  counted
///    · U+00B7    2       1      ∙ U+2219     1       1
///    … U+2026    2       1      ⋯ U+22EF     1       1
///    ↑ U+2191    2       1      ▴ U+25B4     1       1
///    ↓ U+2193    2       1      ▾ U+25BE     1       1
///    ← U+2190    2       1      ◂ U+25C2     1       1
///    → U+2192    2       1      ▸ U+25B8     1       1
///    – U+2013    2       1      -  U+002D    1       1
///    — U+2014    2       1      -  U+002D    1       1
///    ● U+25CF    2       1      • U+2022     1       1
///    ○ U+25CB    2       1      ◦ U+25E6     1       1
///    ◐ U+25D0    2       1      ◒ U+25D2     1       1
///    ◆ U+25C6    2       1      ▪ U+25AA     1       1
/// ```
///
/// Every substitute is safe on both counts: conhost paints it one column wide
/// *and* unicode-width counts it one, so the layout moves nowhere — on Windows
/// or anywhere else. (Turning on a CJK width table was ruled out for the
/// opposite reason: conhost draws `│ ─ ┆` one column wide, where a CJK table
/// counts two and would break every separator on screen.)
///
/// Conhost exists only on Windows, so the pass is gated with `cfg!` rather
/// than `cfg`: on other platforms the branch folds to a constant, and the
/// function still compiles, is still exercised by its test, and still costs
/// nothing.
const NARROW_WIDE: &[(&str, &str)] = &[
    ("·", "∙"),
    ("…", "⋯"),
    ("↑", "▴"),
    ("↓", "▾"),
    ("←", "◂"),
    ("→", "▸"),
    ("–", "-"),
    ("—", "-"),
    ("●", "•"),
    ("○", "◦"),
    ("◐", "◒"),
    ("◆", "▪"),
];

/// Rewrite every cell conhost would paint one column too wide — see
/// [`NARROW_WIDE`] for the measurements. Runs over the finished frame,
/// overlays included, so nothing the palette or a dialog draws escapes it.
fn narrow_wide_symbols(buffer: &mut Buffer) {
    let area = buffer.area;
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let Some(cell) = buffer.cell_mut((x, y)) else {
                continue;
            };
            // Every symbol in the table is multi-byte, so the common case —
            // a screen full of ASCII — is one length check and out.
            if cell.symbol().len() < 2 {
                continue;
            }
            let narrow = NARROW_WIDE
                .iter()
                .find(|(wide, _)| *wide == cell.symbol())
                .map(|(_, narrow)| *narrow);
            if let Some(narrow) = narrow {
                cell.set_symbol(narrow);
            }
        }
    }
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
        // Two ways to lose a row, and they used to be a choice: a row wider
        // than the column either wraps — the sidebar has no scroll and a
        // fixed height, so the extra physical lines push Quick-send and Watch
        // out of view — or it is cut to an ellipsis and the text is simply
        // gone. So the pane decides: wrap while the wrapped height still
        // fits, and cut only what no longer fits. The 32-star token mask (44
        // columns with its label, 46 in 中文) and the 46-column LAN-token hint
        // wrap on a window with room, and a short one keeps the sections it
        // can still show.
        let budget = rect.width.saturating_sub(1);
        let rendered = super::sidebar::render_lines(app);
        let rows: u16 = rendered.iter().map(|line| wrapped_rows(line, budget)).sum();
        let lines: Vec<Line<'static>> = if rows <= rect.height {
            rendered
        } else {
            rendered
                .into_iter()
                .map(|line| clip_line(line, budget))
                .collect()
        };
        // The text column stops one short of the pane: the last column is the
        // separator's, and a wrapped row that ran into it would overwrite the
        // rule. Wrapping inside this pane also makes `.wrap()` break where
        // `wrapped_rows` counted, so the two never disagree about how tall the
        // sidebar just became.
        let text = Rect {
            x: rect.x,
            y: rect.y,
            width: budget,
            height: rect.height,
        };
        frame.render_widget(
            ratatui::widgets::Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: false }),
            text,
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
    // Measured before the paragraph takes `lines`: the caret's row depends on
    // the same wrapping the paragraph is about to apply.
    let caret = center_caret(app, &lines, center.width, offset, center);
    frame.render_widget(
        ratatui::widgets::Paragraph::new(lines)
            .scroll((offset, 0))
            .wrap(ratatui::widgets::Wrap { trim: false }),
        center,
    );
    if let Some((row, column)) = caret {
        frame.set_cursor_position((center.x + column, center.y + row));
    }

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

    // Last, after every overlay has had its turn: the substitution is a
    // whole-buffer rewrite, and anything drawn after it would be missed.
    if cfg!(windows) {
        narrow_wide_symbols(frame.buffer_mut());
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

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Modifier;

    /// A pane the same size as the grid, so rendered rows and grid rows line
    /// up exactly and a test can name the row it means. The grid is built at
    /// that size rather than resized into it: shrinking one pushes the rows it
    /// drops into the scrollback, which would move every row a test names.
    fn app_with_pane(width: u16, height: u16) -> crate::tui::state::App {
        let mut app = crate::tui::test_app();
        app.center_width = width;
        app.center_height = height;
        let (cols, rows) =
            super::super::terminal_view::grid_dims(width, height, app.settings.font_size);
        app.terminal.grid = super::super::terminal_view::TermGrid::new(cols, rows);
        app.terminal.dims = (cols, rows);
        app
    }

    /// The text of a rendered row, and which of its spans are reversed.
    fn plain(line: &Line<'static>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    fn reversed(line: &Line<'static>) -> Vec<String> {
        line.spans
            .iter()
            .filter(|span| span.style.add_modifier.contains(Modifier::REVERSED))
            .map(|span| span.content.to_string())
            .collect()
    }

    fn runs<'a>(line: &'a Line<'static>) -> Vec<(&'a str, bool, bool)> {
        line.spans
            .iter()
            .map(|span| {
                (
                    span.content.as_ref(),
                    span.style.add_modifier.contains(Modifier::BOLD),
                    span.style.add_modifier.contains(Modifier::REVERSED),
                )
            })
            .collect()
    }

    /// The pane used to draw no caret at all: the grid tracks it,
    /// `GridView::cursor` carries it out, and nothing read it — so a shell on
    /// the device gave no feedback about where it was about to type.
    #[test]
    fn the_caret_marks_exactly_one_cell() {
        let mut line = Line::from("prompt>");

        mark_cursor(&mut line, 3);

        let marked: Vec<&str> = line
            .spans
            .iter()
            .filter(|span| {
                span.style
                    .add_modifier
                    .contains(ratatui::style::Modifier::REVERSED)
            })
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(marked, vec!["m"], "one cell, not the whole run: {marked:?}");
        let text: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(text, "prompt>", "the row still reads the same");
    }

    /// `cells_to_line` drops trailing blanks, so the cell the caret sits on at
    /// the end of a short row does not exist until it is put back.
    #[test]
    fn a_caret_past_the_end_of_the_row_gets_its_cell_back() {
        let mut line = Line::from("ab");

        mark_cursor(&mut line, 4);

        let text: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(
            text, "ab   ",
            "cols 2 and 3 are blank, the caret owns col 4"
        );
        assert_eq!(
            line.spans
                .last()
                .filter(|span| {
                    span.style
                        .add_modifier
                        .contains(ratatui::style::Modifier::REVERSED)
                })
                .map(|span| span.content.as_ref()),
            Some("   "),
            "…and the whole run back there is the reversed one"
        );
    }

    /// Splitting the run to place the caret must not lose the style the run
    /// carried: the rest of the row keeps its colour and its attributes.
    #[test]
    fn the_caret_splits_the_run_without_touching_the_rest_of_it() {
        let mut line = Line::from(vec![
            Span::styled(">", Style::default().fg(Color::Blue)),
            Span::styled(
                "ls",
                Style::default().add_modifier(ratatui::style::Modifier::BOLD),
            ),
        ]);

        mark_cursor(&mut line, 0);

        let (first, rest) = line.spans.split_first().expect("the caret cell");
        assert_eq!(first.content.as_ref(), ">");
        assert!(first
            .style
            .add_modifier
            .contains(ratatui::style::Modifier::REVERSED));
        assert_eq!(first.style.fg, Some(Color::Blue), "the colour survives");
        assert_eq!(
            rest.iter()
                .map(|span| span.content.as_ref())
                .collect::<String>(),
            "ls"
        );
        assert!(rest.iter().all(|span| !span
            .style
            .add_modifier
            .contains(ratatui::style::Modifier::REVERSED)));
        assert!(rest[0]
            .style
            .add_modifier
            .contains(ratatui::style::Modifier::BOLD));
    }

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

    /// One span merged two differently styled runs, and a selection has to
    /// reverse the stretch it covers without touching what is outside it.
    #[test]
    fn a_block_cuts_a_styled_run_apart_and_reverses_only_what_it_covers() {
        let mut line = Line::from(vec![
            Span::styled("> ", Style::default().fg(Color::Blue)),
            Span::styled("ls -la", Style::default().add_modifier(Modifier::BOLD)),
        ]);

        invert_columns(&mut line, 3, 5);

        assert_eq!(
            runs(&line),
            vec![
                ("> ", false, false),
                ("l", true, false),
                ("s ", true, true),
                ("-la", true, false),
            ]
        );
        assert_eq!(plain(&line), "> ls -la", "the text never changes");
    }

    #[test]
    fn an_edge_inside_a_wide_glyph_reverses_the_whole_glyph() {
        let mut line = Line::from("wide 中x");

        // Column 6 is the *second* cell of 中: a drag that lands on it picks
        // the character up whole, the way it was read.
        invert_columns(&mut line, 6, 7);

        assert_eq!(plain(&line), "wide 中x");
        assert_eq!(reversed(&line), vec!["中"]);
    }

    #[test]
    fn the_selection_is_reversed_in_the_rows_it_covers() {
        let mut app = app_with_pane(40, 10);
        app.terminal.grid.feed(b"first\r\nsecond\r\n");
        app.terminal.begin_selection(0, 2);
        app.terminal.extend_selection(1, 5);

        let lines = terminal_lines(&app, 40);

        assert_eq!(plain(&lines[0]), "first");
        assert_eq!(reversed(&lines[0]), vec!["rst"], "from the press on");
        assert_eq!(reversed(&lines[1]), vec!["second"], "up to the release");
        assert!(
            reversed(&lines[2]).is_empty(),
            "a row below the selection stays untouched"
        );
    }

    #[test]
    fn a_blank_row_inside_the_block_gets_the_panes_width() {
        let mut app = app_with_pane(40, 6);
        app.terminal.grid.feed(b"aaa\r\n\r\nbbb\r\n");
        app.terminal.begin_selection(0, 0);
        app.terminal.extend_selection(2, 0);

        let lines = terminal_lines(&app, 40);

        assert_eq!(lines[1].spans.len(), 1, "{:?}", lines[1]);
        assert_eq!(plain(&lines[1]), " ".repeat(40));
        assert_eq!(reversed(&lines[1]), vec![" ".repeat(40)]);
    }

    /// A caret inside the block would be inverted twice — back to normal video
    /// — and a hole reads as "this cell is not selected" while it would still
    /// be copied. So the caret stands down while a selection is up.
    #[test]
    fn a_selection_hides_the_caret_instead_of_punching_a_hole_in_the_block() {
        let mut app = app_with_pane(40, 6);
        app.terminal.grid.feed(b"abcdef");

        let bare = terminal_lines(&app, 40);
        assert_eq!(reversed(&bare[0]), vec![" "], "the caret marks its cell");

        app.terminal.begin_selection(0, 0);
        app.terminal.extend_selection(0, 2);

        let selected = terminal_lines(&app, 40);
        assert_eq!(reversed(&selected[0]), vec!["abc"], "one unbroken block");
    }

    #[test]
    fn an_empty_selection_draws_nothing() {
        let mut app = app_with_pane(40, 6);
        app.terminal.grid.feed(b"abcdef");
        app.terminal.begin_selection(0, 3);

        let lines = terminal_lines(&app, 40);

        assert_eq!(
            reversed(&lines[0]),
            vec![" "],
            "a click that never moved is the caret, not a selection"
        );
    }

    /// What keeps a status line from running into the sidebar on a console
    /// that counts these glyphs two columns wide. Both halves matter: the
    /// wide glyphs must go, and the ASCII a screen is mostly made of must
    /// stay exactly where it was — a substitution that drifted would be a
    /// new misalignment rather than the old one.
    #[test]
    fn wide_symbols_are_swapped_for_ones_conhost_draws_narrow() {
        let last = NARROW_WIDE.len() as u16;
        let mut buffer = Buffer::empty(Rect::new(0, 0, last + 1, 1));
        for (i, (wide, _)) in NARROW_WIDE.iter().enumerate() {
            buffer[(i as u16, 0)].set_symbol(wide);
        }
        // A glyph nobody asked about, standing beside them.
        buffer[(last, 0)].set_symbol("A");

        narrow_wide_symbols(&mut buffer);

        for (i, (wide, narrow)) in NARROW_WIDE.iter().enumerate() {
            assert_eq!(
                buffer[(i as u16, 0)].symbol(),
                *narrow,
                "{wide} must be drawn as {narrow}"
            );
        }
        assert_eq!(
            buffer[(last, 0)].symbol(),
            "A",
            "everything outside the table stays put"
        );
    }
}
