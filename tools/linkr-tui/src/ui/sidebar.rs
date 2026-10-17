//! The sidebar: connection block, view list, quick sends and the watch list.
//!
//! Three layers, deliberately. [`rows`] builds the plain content and marks what
//! each row *is* and how far it is indented; [`lines`] colours those rows from
//! the theme and fits them to the column. Tests assert on `rows` for structure
//! and on `lines` for colour, so a layout change cannot hide behind a style
//! change.
//!
//! **One plan, every language.** The row plan is a function of the session
//! state only: the same number of rows, in the same order, at the same indents,
//! whatever language is in force. A row is never wrapped — it is fitted to the
//! column, and the strings that go into it are held short enough by the tests
//! here. Wrapping is what the design did with `Enter sends · reboot asks first`
//! (31 columns in a 29-column column), and that extra row pushed `Watch` — and
//! everything after it — one row down, so the English sidebar no longer lined
//! up with the Chinese one.

use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use crate::app::{App, Transport};
use crate::dialogs::{self, ConfirmAction};
use crate::event::View;
use crate::text::clip_to;
use crate::theme::Theme;

/// How far a section header sits from the left edge. The hint under `Quick
/// send` shares it: it belongs to the header, not to the list under it.
const HEADER_INDENT: u8 = 0;

/// How far a row inside a section sits: one marker's worth of room, so a label
/// lines up under the text of the row above it rather than under its marker.
const ITEM_INDENT: u8 = 2;

/// The watch section sits one column deeper, as the design has it.
const WATCH_INDENT: u8 = 3;

/// The cursor's marker: a small triangle, so the accent chip reading as a
/// button still shows which row the keyboard is on.
pub const CURSOR_MARKER: &str = "▸ ";

/// What follows a shortcut that asks before it runs. The design marks `reboot`
/// with it, and it is the difference between a red row and a row the user can
/// see is red *because it is the dangerous one*.
///
/// An ASCII `!` rather than the ⚠ the design previews drew. U+26A0 is
/// emoji-capable, and a terminal that resolves it to the emoji form draws it
/// two columns wide — twice the column the row reserved for it, which is why
/// its right half came back clipped on exactly those terminals. The
/// text-presentation selector only helps fonts that honour it; the colour of
/// the row is what actually carries the warning, so the glyph only has to
/// survive a monospace font, and `!` is in all of them.
pub const DANGER_MARKER: &str = "!";

/// What a sidebar row means, which decides how it is painted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowStyle {
    /// A section header (`Connection`, `Views`, …).
    Header,
    /// A blank row between two sections. Part of the plan, not an accident:
    /// the sections land on the designed rows because the gaps count.
    Gap,
    /// The session's state and its link, under the connection header.
    State,
    /// A plain label (`Device: …`).
    Label,
    /// A dimmed hint line.
    Hint,
    /// An action the user can run, not currently selected.
    Action,
    /// The action the sidebar cursor is on: the accent chip.
    Selected,
    /// The view that is showing in the centre column.
    ActiveView,
    /// A shortcut that asks for confirmation first.
    Dangerous,
}

/// One sidebar row: what it says, what it means, and where it sits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The row's own text — its label, its action, its view name.
    pub text: String,
    pub style: RowStyle,
    /// Leading columns, before `text`.
    pub indent: u8,
    /// A dimmed tail after the text (` · ─`), which only the state row has.
    pub tail: Option<String>,
    /// A marker after the text, in the danger colour: `reboot !`. Only the
    /// shortcuts that ask before running carry it.
    pub marker: Option<&'static str>,
}

impl Row {
    fn at(text: impl Into<String>, style: RowStyle, indent: u8) -> Row {
        Row {
            text: text.into(),
            style,
            indent,
            tail: None,
            marker: None,
        }
    }

    /// The same, with a dimmed tail.
    fn trailing(mut self, tail: impl Into<String>) -> Row {
        self.tail = Some(tail.into());
        self
    }

    /// The same, marked as a row that asks before it runs.
    fn marked(mut self, marker: &'static str) -> Row {
        self.marker = Some(marker);
        self
    }

    /// The row laid out, less its marker: the indent, then the text. The marker
    /// is painted separately, because it carries a colour of its own.
    fn laid_out(&self) -> String {
        format!("{}{}", " ".repeat(self.indent as usize), self.text)
    }

    /// Whether the cursor can land on this row.
    pub fn selectable(&self) -> bool {
        matches!(
            self.style,
            RowStyle::Action | RowStyle::Selected | RowStyle::ActiveView | RowStyle::Dangerous
        )
    }
}

/// The selectable rows, in cursor order. The sidebar cursor indexes this list,
/// so highlighting and navigation can never disagree with what is drawn.
pub fn targets(app: &App) -> Vec<String> {
    let s = app.strings();
    let mut out = vec![labelled(
        s.transport_label,
        &app.transport_text(),
        s.label_separator,
    )];
    // A LAN dial is described by two rows the keyboard owns: the address it
    // goes to and the token it carries. A Bluetooth dial names its device
    // once a link is up, and that row reports rather than chooses — which is
    // what keeps it out of the cursor's list.
    let lan = app.transport == Transport::Lan;
    if lan {
        out.push(labelled(
            s.lan_host_label,
            &app.lan_host_text(),
            s.label_separator,
        ));
        out.push(labelled(
            s.lan_token_label,
            &app.lan_token_text(),
            s.label_separator,
        ));
    }
    // One row that says `Connect` when there is nothing to connect to and
    // `Disconnect` when there is: the label is the state, so the row under the
    // cursor always names what Enter will do.
    out.push(
        if app.session.is_live() {
            s.disconnect
        } else {
            s.connect
        }
        .to_string(),
    );
    // Switching devices is looking for a board on the radio; a LAN session
    // has no radios to look through, so the row is not there to press.
    if !lan {
        out.push(s.switch_device.to_string());
    }
    for view in View::ALL {
        out.push(app.lang.view_name(view).to_string());
    }
    for quick in &app.quick_send {
        out.push(quick.command.clone());
    }
    out
}

/// `Transport: BLE` — a label, the language's separator, then the value.
pub fn labelled(label: &str, value: &str, separator: &str) -> String {
    format!("{label}{separator}{value}")
}

/// What the row under the cursor does when it is activated.
///
/// The index is the one [`targets`] produces — the transport, the two
/// connection actions, one row per view, then the quick sends — so a row the
/// cursor can point at is a row that can be used, and a test holds the two
/// together.
///
/// The guarded quick sends raise a confirmation rather than firing: `reboot`
/// over a UART is not undoable, and one Enter's worth of cursor slip must not
/// be enough to reboot the bridge. The focus is deliberately left where it is —
/// `Tab` is the documented way to move it, and a row activation that moved it
/// too would make that key mean two things.
pub fn activate(app: &mut App) {
    // The list grows and shrinks with the transport, which trades the
    // switch-device row for the LAN host and token rows; a cursor left past
    // the end would light no row and swallow Enter, so it is pulled back
    // before anything reads it.
    app.sidebar_cursor = app.sidebar_cursor.min(app.sidebar_rows().saturating_sub(1));
    let cursor = app.sidebar_cursor;
    let lan = app.transport == Transport::Lan;
    // Walked in the order `rows` draws them rather than matched on numbers: a
    // row plan that changes shape (LAN's two rows, the switch-device row only
    // on a radio) cannot then disagree with what Enter does.
    let mut at = 0usize;
    let mut next = || {
        let here = at;
        at += 1;
        here
    };
    if cursor == next() {
        let notice = app.toggle_transport();
        app.push_notice(notice);
        return;
    }
    if lan && cursor == next() {
        dialogs::prompt_lan_host(app);
        return;
    }
    if lan && cursor == next() {
        dialogs::prompt_lan_token(app);
        return;
    }
    if cursor == next() {
        // The row is `Disconnect` while a link is up, so pressing it hangs up
        // — the same toggle the web client has. When there is nothing live it
        // is `Connect` again and re-dials.
        if app.session.is_live() {
            app.disconnect();
        } else {
            app.reconnect();
        }
        return;
    }
    if !lan && cursor == next() {
        if app.session.is_live() {
            let text = app.strings().switch_needs_disconnect.to_string();
            app.push_notice(crate::app::Notice {
                text,
                level: crate::event::NoticeLevel::Warn,
            });
            return;
        }
        // Switching device IS looking for one. There used to be a
        // separate palette action for the scan and a warning here,
        // which left the sidebar row — the one place a user looks to
        // change who they are talking to — as the only action in the
        // interface that answered "there is no other device" without
        // ever having asked the radio.
        if let Err(why) = app.start_ble_scan() {
            let text = app.strings().ble_scan_failed.to_string();
            app.push_notice(crate::app::Notice {
                text: format!("{text}: {why}"),
                level: crate::event::NoticeLevel::Error,
            });
        }
        return;
    }
    let views = View::ALL.len();
    if cursor < at + views {
        app.set_view(View::ALL[cursor - at]);
        return;
    }
    let Some(quick) = app.quick_send.get(cursor - (at + views)).cloned() else {
        return;
    };
    if quick.dangerous {
        dialogs::ask(app, ConfirmAction::Send(quick.command));
    } else {
        app.send_command(&quick.command);
    }
}

/// The whole sidebar, top to bottom, exactly as many rows as the design draws.
pub fn rows(app: &App) -> Vec<Row> {
    let s = app.strings();
    let mut rows = Vec::new();
    let mut cursor = 0usize;
    let mut next_cursor = || {
        let here = cursor;
        cursor += 1;
        here
    };
    let selected = |index: usize| app.sidebar_cursor == index;
    let action_style = |index: usize| {
        if selected(index) {
            RowStyle::Selected
        } else {
            RowStyle::Action
        }
    };

    // --- Connection -------------------------------------------------------
    rows.push(Row::at(
        s.section_connection,
        RowStyle::Header,
        HEADER_INDENT,
    ));
    // The state row is read, not chosen: it is not a cursor target, which is
    // why it takes no cursor index.
    rows.push(Row::at(app.state_name(), RowStyle::State, HEADER_INDENT));

    let transport = next_cursor();
    rows.push(Row::at(
        labelled(s.transport_label, &app.transport_text(), s.label_separator),
        action_style(transport),
        ITEM_INDENT,
    ));
    if app.transport == Transport::Lan {
        // What a LAN dial is built from, in two rows the keyboard owns: the
        // address and the token. While a link is up it is that link's own
        // address being shown, with the token's presence riding along as the
        // trailer — the row reports the truth, and what Enter edits is the
        // next dial.
        let host = next_cursor();
        let mut host_row = Row::at(
            labelled(s.lan_host_label, &app.lan_host_text(), s.label_separator),
            action_style(host),
            ITEM_INDENT,
        );
        if app.session.is_live() {
            host_row = host_row.trailing(if app.session.lan_secured() {
                s.lan_token_yes
            } else {
                s.lan_token_no
            });
        }
        rows.push(host_row);
        let token = next_cursor();
        rows.push(Row::at(
            labelled(s.lan_token_label, &app.lan_token_text(), s.label_separator),
            action_style(token),
            ITEM_INDENT,
        ));
    } else {
        // The device names the link once one is up; it is read, not chosen,
        // which is why it takes no cursor index.
        rows.push(Row::at(
            labelled(s.device_label, &app.device_text(), s.label_separator),
            RowStyle::Label,
            ITEM_INDENT,
        ));
    }
    let connect = next_cursor();
    rows.push(Row::at(
        if app.session.is_live() {
            s.disconnect
        } else {
            s.connect
        },
        action_style(connect),
        ITEM_INDENT,
    ));
    // Switching devices is looking for a board on the radio; a LAN session has
    // no radios to look through, so the row is not there to press. Connected,
    // it goes dim instead: a session is one link, and picking another while
    // this one holds the target would mean two things claiming the same
    // console. Disconnecting is what opens the choice, so the connect row
    // stays live as `Disconnect`. The cursor keeps its chip on the dim row
    // rather than vanishing — a row the cursor cannot be on is a row that
    // cannot explain itself.
    if app.transport != Transport::Lan {
        let index = next_cursor();
        let style = if app.session.is_live() {
            if selected(index) {
                RowStyle::Selected
            } else {
                RowStyle::Hint
            }
        } else {
            action_style(index)
        };
        rows.push(Row::at(s.switch_device, style, ITEM_INDENT));
    }

    // --- Views ------------------------------------------------------------
    rows.push(Row::at("", RowStyle::Gap, HEADER_INDENT));
    rows.push(Row::at(s.section_views, RowStyle::Header, HEADER_INDENT));
    for view in View::ALL {
        let index = next_cursor();
        let style = if selected(index) {
            RowStyle::Selected
        } else if view == app.view {
            RowStyle::ActiveView
        } else {
            RowStyle::Action
        };
        rows.push(Row::at(app.lang.view_name(view), style, ITEM_INDENT));
    }

    // --- Quick send -------------------------------------------------------
    rows.push(Row::at("", RowStyle::Gap, HEADER_INDENT));
    rows.push(Row::at(
        s.section_quick_send,
        RowStyle::Header,
        HEADER_INDENT,
    ));
    rows.push(Row::at(s.quick_send_hint, RowStyle::Hint, HEADER_INDENT));
    for quick in &app.quick_send {
        let index = next_cursor();
        let style = if selected(index) {
            RowStyle::Selected
        } else if quick.dangerous {
            RowStyle::Dangerous
        } else {
            RowStyle::Action
        };
        let row = Row::at(quick.command.clone(), style, ITEM_INDENT);
        // The marker belongs to the command, not to the cursor: `reboot` is
        // dangerous whether or not the keyboard happens to be sitting on it.
        rows.push(if quick.dangerous {
            row.marked(DANGER_MARKER)
        } else {
            row
        });
    }

    // --- Watch ------------------------------------------------------------
    rows.push(Row::at("", RowStyle::Gap, HEADER_INDENT));
    rows.push(Row::at(s.section_watch, RowStyle::Header, HEADER_INDENT));
    if app.watch.is_empty() {
        rows.push(Row::at(s.watch_empty, RowStyle::Hint, WATCH_INDENT));
    } else {
        for finding in &app.watch {
            rows.push(Row::at(finding.clone(), RowStyle::Action, WATCH_INDENT));
        }
    }

    rows
}

/// The sidebar painted for a column `width` columns wide.
///
/// Every row is fitted to that width: a row is never wrapped and never spills,
/// because a wrapped row shifts every row below it. The selected row is an
/// accent chip that hugs its text, as the design's does — padding it out to the
/// full column would make it read as a bar rather than as the row it marks.
pub fn lines(app: &App, theme: &Theme, width: u16) -> Vec<Line<'static>> {
    let width = width as usize;
    rows(app)
        .into_iter()
        .map(|row| match row.style {
            RowStyle::Gap => Line::from(""),
            RowStyle::State => {
                let glyph = Span::styled(
                    format!("{} ", app.state_glyph()),
                    theme.state_dot(app.state),
                );
                let room = width.saturating_sub(2);
                let mut spans = vec![glyph, Span::styled(clip_to(&row.text, room), theme.text())];
                if let Some(tail) = row.tail {
                    let used = crate::text::columns(&row.text);
                    let left = room.saturating_sub(used);
                    spans.push(Span::styled(clip_to(&tail, left), theme.muted()));
                }
                Line::from(spans)
            }
            RowStyle::Selected => {
                // The chip hugs the whole row, marker included, so a selected
                // dangerous shortcut stays one unbroken chip — which means the
                // marker's columns are reserved *before* the text is fitted:
                // clip first and append after and the clip eats exactly the
                // warning that made the row worth selecting.
                let tail = row.marker.map(|m| format!(" {m}")).unwrap_or_default();
                let room = width.saturating_sub(crate::text::columns(&tail));
                let mut chip = clip_to(&format!("{CURSOR_MARKER}{}", row.text), room);
                chip.push_str(&tail);
                Line::from(Span::styled(chip, theme.selected()))
            }
            other => {
                // A marker is budgeted for before the text is fitted, so a row
                // that runs long loses its tail rather than its warning.
                let tail = row.marker.map(|m| format!(" {m}")).unwrap_or_default();
                let room = width.saturating_sub(crate::text::columns(&tail));
                let mut spans = vec![Span::styled(
                    clip_to(&row.laid_out(), room),
                    style_for(other, theme),
                )];
                if !tail.is_empty() {
                    spans.push(Span::styled(tail, theme.danger()));
                }
                Line::from(spans)
            }
        })
        .collect()
}

/// The colour a row type is painted in — the whole of the theme's say in the
/// sidebar's appearance.
fn style_for(style: RowStyle, theme: &Theme) -> ratatui::style::Style {
    match style {
        RowStyle::Header => theme.section_header(),
        RowStyle::Hint => theme.muted(),
        RowStyle::Dangerous => theme.danger(),
        RowStyle::ActiveView => theme.text().add_modifier(Modifier::BOLD),
        _ => theme.text(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Lang;
    use crate::text::columns;
    use crate::ui::SIDEBAR_TEXT_WIDTH;

    fn app_in(lang: Lang) -> App {
        App {
            lang,
            ..App::default()
        }
    }

    fn app_en() -> App {
        app_in(Lang::En)
    }

    /// The text of a painted row, marker and indent included.
    fn text_of(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// An app with a link up. The loopback is live the moment it is dialled —
    /// `is_live` is the handle, not the state — so the connected rows can be
    /// read without pumping a frame into place first.
    fn app_live_with(lang: Lang) -> App {
        use crate::session::Dial;
        let mut app = app_in(lang);
        app.dial(&Dial::Stdio("-".into()))
            .expect("the loopback opens");
        assert!(app.session.is_live(), "the link is up");
        app
    }

    fn app_live() -> App {
        app_live_with(Lang::En)
    }

    /// The rows appear in the designed order: four sections, each header
    /// followed by its content, with the connection block first.
    #[test]
    fn the_sections_appear_in_design_order() {
        let app = app_en();
        let rows = rows(&app);
        let headers: Vec<&str> = rows
            .iter()
            .filter(|r| r.style == RowStyle::Header)
            .map(|r| r.text.as_str())
            .collect();
        assert_eq!(headers, vec!["Connection", "Views", "Quick send", "Watch"]);
        assert_eq!(rows[0].style, RowStyle::Header);
    }

    /// The connection block opens with the session's state, which is read
    /// rather than chosen: the cursor never lands on it.
    #[test]
    fn the_connection_block_opens_with_the_state_row() {
        let app = app_en();
        let rows = rows(&app);
        let state = &rows[1];
        assert_eq!(state.style, RowStyle::State);
        assert_eq!(state.text, "disconnected");
        assert_eq!(state.tail, None, "the state row names only the state");
        assert!(!state.selectable(), "the state row is a cursor target");
        assert!(
            !targets(&app).iter().any(|t| t == "disconnected"),
            "the state row is in the cursor list"
        );
    }

    /// Every language lays the sidebar out the same: the same number of rows,
    /// the same kinds, at the same indents. That is what makes the frames line
    /// up — the bug this pins is the design's, where the English hint wrapped
    /// onto a second row and pushed `Watch` one row below the Chinese one.
    #[test]
    fn the_row_plan_is_the_same_in_every_language() {
        let zh = rows(&app_in(Lang::Zh));
        let en = rows(&app_en());
        assert_eq!(zh.len(), en.len(), "the two languages plan different rows");
        for (index, (zh, en)) in zh.iter().zip(&en).enumerate() {
            assert_eq!(zh.style, en.style, "row {index} differs in kind");
            assert_eq!(zh.indent, en.indent, "row {index} differs in indent");
            assert_eq!(
                zh.tail.is_some(),
                en.tail.is_some(),
                "row {index} has a tail in one language only"
            );
        }
    }

    /// Nothing in the sidebar has to be cut off, in either language: if a row
    /// needs clipping, the plan has grown a row that is too long for its
    /// column, and the string is what has to change.
    #[test]
    fn no_row_is_wider_than_its_column_in_either_language() {
        for lang in [Lang::Zh, Lang::En] {
            let app = app_in(lang);
            for row in rows(&app) {
                let budget = SIDEBAR_TEXT_WIDTH as usize;
                let used = row.indent as usize
                    + columns(&row.text)
                    + row.tail.as_ref().map(|t| columns(t)).unwrap_or(0)
                    + if row.style == RowStyle::Selected {
                        columns(CURSOR_MARKER)
                    } else if row.style == RowStyle::State {
                        2
                    } else {
                        0
                    };
                assert!(
                    used <= budget,
                    "{lang:?} row {:?} needs {used} of {budget} columns",
                    row.text
                );
            }
        }
    }

    /// The painted rows all fit the column, in both languages and at every
    /// width: the property the layout depends on.
    #[test]
    fn painted_rows_never_exceed_the_column() {
        for lang in [Lang::Zh, Lang::En] {
            let app = app_in(lang);
            for width in [4u16, 12, SIDEBAR_TEXT_WIDTH, 60] {
                for line in lines(&app, &Theme::DARK, width) {
                    let text = text_of(&line);
                    assert!(
                        columns(&text) <= width as usize,
                        "{lang:?} at {width}: {text:?} is {} columns",
                        columns(&text)
                    );
                }
            }
        }
    }

    /// Every view is listed, under the Views header, and the active one is
    /// marked so the sidebar and the centre column agree.
    #[test]
    fn the_active_view_is_marked() {
        let mut app = app_en();
        app.view = View::Network;
        let rows = rows(&app);
        let active: Vec<&str> = rows
            .iter()
            .filter(|r| r.style == RowStyle::ActiveView)
            .map(|r| r.text.as_str())
            .collect();
        assert_eq!(active, vec!["Network"]);
        for view in View::ALL {
            assert!(
                rows.iter().any(|r| r.text == app.lang.view_name(view)),
                "{view:?} missing from the sidebar"
            );
        }
    }

    /// The cursor lands on exactly one row, and only on a selectable one — a
    /// header, a gap or the state row can never be the cursor's target.
    #[test]
    fn the_cursor_marks_exactly_one_selectable_row() {
        let mut app = app_en();
        let selectable = targets(&app).len();
        for index in 0..selectable {
            app.sidebar_cursor = index;
            let rows = rows(&app);
            let selected: Vec<&Row> = rows
                .iter()
                .filter(|r| r.style == RowStyle::Selected)
                .collect();
            assert_eq!(
                selected.len(),
                1,
                "cursor {index} marked {} rows",
                selected.len()
            );
            assert_eq!(selected[0].text, targets(&app)[index]);
        }
    }

    /// A cursor past the end marks nothing rather than panicking: a stale
    /// cursor after the quick-send list shrinks must not crash a frame.
    #[test]
    fn a_cursor_past_the_end_marks_nothing() {
        let mut app = app_en();
        app.sidebar_cursor = targets(&app).len() + 7;
        let rows = rows(&app);
        assert_eq!(
            rows.iter()
                .filter(|r| r.style == RowStyle::Selected)
                .count(),
            0
        );
    }

    /// The default cursor is on the transport row — the chip the design shows
    /// in the first frame.
    #[test]
    fn the_default_cursor_is_on_the_transport_row() {
        let mut app = app_en();
        app.transport_locked = true;
        let rows = rows(&app);
        let selected = rows.iter().find(|r| r.style == RowStyle::Selected).unwrap();
        assert_eq!(selected.text, "Transport: BLE (locked)");
    }

    /// The watch section shows its empty state as a hint, and the findings
    /// replace it once there are any.
    #[test]
    fn the_watch_section_shows_either_findings_or_its_empty_state() {
        let mut app = app_en();
        assert!(rows(&app)
            .iter()
            .any(|r| r.text == "no findings yet" && r.style == RowStyle::Hint));
        app.watch = vec!["panic @ 0x400d1234".to_string()];
        let rows = rows(&app);
        assert!(!rows.iter().any(|r| r.text == "no findings yet"));
        assert!(rows.iter().any(|r| r.text == "panic @ 0x400d1234"));
    }

    /// The guarded shortcut is coloured as dangerous; the plain ones are not.
    #[test]
    fn only_guarded_shortcuts_are_dangerous() {
        let app = app_en();
        let rows = rows(&app);
        let dangerous: Vec<&str> = rows
            .iter()
            .filter(|r| r.style == RowStyle::Dangerous)
            .map(|r| r.text.as_str())
            .collect();
        assert_eq!(dangerous, vec!["reboot"]);
    }

    /// The painted rows carry the theme: headers are accent, hints muted, the
    /// cursor row is the accent chip. This is the "no literal colours in the
    /// view" rule, enforced.
    #[test]
    fn painted_rows_take_their_colours_from_the_theme() {
        let app = App {
            state: crate::event::ConnectionState::Connecting,
            ..app_en()
        };
        let theme = Theme::DARK;
        let painted = lines(&app, &theme, SIDEBAR_TEXT_WIDTH);
        let rows = rows(&app);
        assert_eq!(painted.len(), rows.len());
        for (row, line) in rows.iter().zip(&painted) {
            if row.style == RowStyle::Gap {
                assert!(text_of(line).is_empty());
                continue;
            }
            let first = &line.spans[0];
            match row.style {
                RowStyle::Header => assert_eq!(first.style.fg, Some(theme.accent)),
                RowStyle::Hint => assert_eq!(first.style.fg, Some(theme.muted)),
                RowStyle::Selected => {
                    assert_eq!(first.style.bg, Some(theme.accent));
                    assert_eq!(first.style.fg, Some(theme.accent_ink));
                }
                RowStyle::Dangerous => assert_eq!(first.style.fg, Some(theme.danger)),
                RowStyle::State => {
                    assert_eq!(
                        first.style.fg,
                        Some(theme.warn),
                        "the state glyph takes the state's colour"
                    );
                    assert_eq!(line.spans[1].style.fg, Some(theme.text));
                }
                _ => assert_eq!(first.style.fg, Some(theme.text)),
            }
        }
    }

    /// The other theme paints the same rows with its own values — the switch is
    /// a value change, not a code path.
    #[test]
    fn the_light_theme_paints_the_same_rows_differently() {
        let app = app_en();
        let dark = lines(&app, &Theme::DARK, SIDEBAR_TEXT_WIDTH);
        let light = lines(&app, &Theme::LIGHT, SIDEBAR_TEXT_WIDTH);
        assert_eq!(dark.len(), light.len());
        let header_dark = dark[0].spans[0].style.fg;
        let header_light = light[0].spans[0].style.fg;
        assert_eq!(header_dark, Some(Theme::DARK.accent));
        assert_eq!(header_light, Some(Theme::LIGHT.accent));
        assert_ne!(header_dark, header_light);
    }

    /// A selected row starts with the marker and the rest are indented to their
    /// section: the chip has to cover the cursor mark, so the mark is part of
    /// the row text.
    #[test]
    fn the_cursor_row_carries_the_marker() {
        let app = app_en();
        let painted = lines(&app, &Theme::DARK, SIDEBAR_TEXT_WIDTH);
        let marked: Vec<String> = painted
            .iter()
            .map(text_of)
            .filter(|t| t.starts_with(CURSOR_MARKER))
            .collect();
        assert_eq!(marked.len(), 1);
        assert!(marked[0].starts_with("▸ Transport:"), "{:?}", marked[0]);
    }

    /// Exactly the shortcuts that ask before running carry the warning marker,
    /// and the marker is the command's — not the cursor's — so moving the
    /// keyboard onto `reboot` does not change it.
    #[test]
    fn only_a_dangerous_shortcut_carries_the_warning_marker() {
        for lang in [Lang::Zh, Lang::En] {
            let app = app_in(lang);
            let marked: Vec<Row> = rows(&app)
                .into_iter()
                .filter(|r| r.marker.is_some())
                .collect();
            assert_eq!(marked.len(), 1, "{lang:?}");
            assert_eq!(marked[0].text, "reboot");
            assert_eq!(marked[0].marker, Some(DANGER_MARKER));
        }
    }

    /// The marker is its own span in the danger colour, so the row reads as a
    /// warning rather than as a label that happens to be red.
    #[test]
    fn the_warning_marker_takes_the_danger_colour() {
        let app = app_en();
        let theme = Theme::DARK;
        let line = lines(&app, &theme, SIDEBAR_TEXT_WIDTH)
            .into_iter()
            .find(|l| text_of(l).trim().starts_with("reboot"))
            .expect("the dangerous row");
        assert_eq!(text_of(&line), format!("  reboot {DANGER_MARKER}"));
        assert_eq!(line.spans.len(), 2, "the marker is not its own span");
        assert_eq!(line.spans[0].style.fg, Some(theme.danger));
        assert_eq!(line.spans[1].content, format!(" {DANGER_MARKER}"));
        assert_eq!(line.spans[1].style.fg, Some(theme.danger));
    }

    /// The marker must be one column: it is budgeted for inside the row's
    /// width, so a glyph the terminal draws wide would eat into the row.
    #[test]
    fn the_warning_marker_is_one_column() {
        assert_eq!(columns(DANGER_MARKER), 1);
    }

    /// A row that runs long loses its text, never its warning, and never wraps
    /// — the row count is what must hold.
    #[test]
    fn the_warning_marker_survives_a_row_that_runs_long() {
        let mut app = app_en();
        app.quick_send = vec![crate::app::QuickSend::guarded(
            "reboot into the bootloader and wait for the host",
        )];
        let painted = lines(&app, &Theme::DARK, SIDEBAR_TEXT_WIDTH);
        assert_eq!(painted.len(), rows(&app).len());
        let line = painted
            .iter()
            .find(|l| text_of(l).contains('…'))
            .expect("the cut row");
        assert!(
            text_of(line).ends_with(DANGER_MARKER),
            "the cut row lost its warning: {:?}",
            text_of(line)
        );
        assert!(columns(&text_of(line)) <= SIDEBAR_TEXT_WIDTH as usize);
    }

    /// Selecting a dangerous shortcut keeps its warning: the chip covers the
    /// marker too, in one unbroken span.
    #[test]
    fn a_selected_dangerous_shortcut_keeps_its_marker_in_the_chip() {
        let mut app = app_en();
        app.sidebar_cursor = rows(&app)
            .iter()
            .filter(|r| r.selectable())
            .position(|r| r.marker.is_some())
            .expect("the dangerous shortcut is selectable");
        let theme = Theme::DARK;
        let chip = lines(&app, &theme, SIDEBAR_TEXT_WIDTH)
            .into_iter()
            .find(|l| text_of(l).starts_with(CURSOR_MARKER))
            .expect("the chip");
        assert_eq!(
            text_of(&chip),
            format!("{CURSOR_MARKER}reboot {DANGER_MARKER}")
        );
        assert_eq!(chip.spans.len(), 1, "the chip is broken into two spans");
        assert_eq!(chip.spans[0].style, theme.selected());
    }

    /// And when the dangerous row is the *long* one — the row that has to be
    /// cut to fit — selecting it still keeps the warning. The marker's columns
    /// are reserved before the text is fitted, so the clip eats the tail of
    /// the text and not the thing that makes the row dangerous.
    #[test]
    fn a_selected_dangerous_row_that_runs_long_still_shows_its_warning() {
        let mut app = app_en();
        app.quick_send = vec![crate::app::QuickSend::guarded(
            "reboot into the bootloader and wait for the host",
        )];
        app.sidebar_cursor = rows(&app)
            .iter()
            .filter(|r| r.selectable())
            .position(|r| r.marker.is_some())
            .expect("the dangerous shortcut is selectable");
        let theme = Theme::DARK;
        let chip = lines(&app, &theme, SIDEBAR_TEXT_WIDTH)
            .into_iter()
            .find(|l| text_of(l).starts_with(CURSOR_MARKER))
            .expect("the chip");
        let text = text_of(&chip);
        assert!(
            text.ends_with(DANGER_MARKER),
            "the cut chip lost its warning: {text:?}"
        );
        assert!(
            columns(&text) <= SIDEBAR_TEXT_WIDTH as usize,
            "the chip outgrew the column: {text:?}"
        );
    }

    /// The chip hugs its text instead of spanning the column: the design's
    /// selection covers exactly the row it marks, and a chip padded out to the
    /// full column would read as a bar.
    #[test]
    fn the_selected_chip_hugs_its_text() {
        for lang in [Lang::Zh, Lang::En] {
            let app = app_in(lang);
            let painted = lines(&app, &Theme::DARK, SIDEBAR_TEXT_WIDTH);
            let chip = painted
                .iter()
                .map(text_of)
                .find(|t| t.starts_with(CURSOR_MARKER))
                .expect("a selected row");
            let row = rows(&app)
                .into_iter()
                .find(|r| r.style == RowStyle::Selected)
                .unwrap();
            assert_eq!(chip, format!("{CURSOR_MARKER}{}", row.text));
            assert!(
                columns(&chip) < SIDEBAR_TEXT_WIDTH as usize,
                "{lang:?} chip {chip:?} spans the whole column"
            );
            assert!(!chip.ends_with(' '), "{lang:?} chip {chip:?} is padded");
        }
    }

    /// A row too long for its column is cut, not wrapped and not spilled: the
    /// row count is what must not change, because a wrapped row would shift
    /// every row below it.
    #[test]
    fn an_overlong_row_is_cut_rather_than_wrapped() {
        let mut app = app_en();
        app.device_name = "Linkr BLE UART with a very long advertised name".to_string();
        let painted = lines(&app, &Theme::DARK, SIDEBAR_TEXT_WIDTH);
        assert_eq!(
            painted.len(),
            rows(&app).len(),
            "a long row changed the row count"
        );
        let device = painted
            .iter()
            .map(text_of)
            .find(|t| t.trim_start().starts_with("Device:"))
            .expect("the device row");
        assert!(device.contains('…'), "{device:?} was not marked as cut");
        assert!(columns(&device) <= SIDEBAR_TEXT_WIDTH as usize);
    }

    /// No row is empty except the gaps, in either language: a blank row in the
    /// middle of a section is a layout bug the design does not have.
    #[test]
    fn only_gap_rows_are_blank() {
        for lang in [Lang::Zh, Lang::En] {
            let app = app_in(lang);
            for row in rows(&app) {
                if row.style == RowStyle::Gap {
                    assert!(row.text.is_empty(), "a gap carries text");
                } else {
                    assert!(!row.text.trim().is_empty(), "blank row in {lang:?}");
                }
            }
        }
    }

    /// Wide (CJK) text keeps its indent and never overflows the column — the
    /// class of bug that shifted the old panel: a two-column glyph counted as
    /// one made the chip and every row after it sit wrong.
    #[test]
    fn wide_text_keeps_its_indent() {
        for lang in [Lang::Zh, Lang::En] {
            let app = app_in(lang);
            for (row, line) in rows(&app)
                .iter()
                .zip(lines(&app, &Theme::DARK, SIDEBAR_TEXT_WIDTH))
            {
                let text = text_of(&line);
                match row.style {
                    RowStyle::Gap => assert!(text.is_empty()),
                    // These two carry a marker instead of an indent: the state
                    // row's glyph, and the cursor's triangle on the chip.
                    RowStyle::State => assert!(
                        text.starts_with(app.state_glyph()),
                        "row {text:?} lost the state's glyph"
                    ),
                    RowStyle::Selected => {
                        assert!(text.starts_with(CURSOR_MARKER), "{text:?}");
                    }
                    _ => {
                        let indent = " ".repeat(row.indent as usize);
                        assert!(
                            text.starts_with(&indent),
                            "row {text:?} lost its {}-column indent",
                            row.indent
                        );
                        assert!(
                            !text.starts_with(&format!("{indent} ")),
                            "row {text:?} is indented past its section"
                        );
                    }
                }
            }
        }

        // The Chinese rows are genuinely wide, so the width accounting above is
        // exercised rather than passing on ASCII by accident.
        let zh = app_in(Lang::Zh);
        let painted = lines(&zh, &Theme::DARK, SIDEBAR_TEXT_WIDTH);
        let view_row = painted
            .iter()
            .map(text_of)
            .find(|t| t.contains("串口终端"))
            .expect("the serial terminal row");
        assert!(
            columns(&view_row) > view_row.chars().count(),
            "{view_row:?} carries no wide glyph"
        );
    }

    /// The connection rows tell the truth about the link: with one up the
    /// first reads `Disconnect` (and `断开连接` in Chinese) rather than a
    /// `Connect` that pressing would answer with silence.
    #[test]
    fn the_connect_row_reads_disconnect_while_a_link_is_up() {
        for lang in [Lang::Zh, Lang::En] {
            let mut app = app_in(lang);
            let idle = targets(&app);
            assert_eq!(idle[1], app.strings().connect, "{lang:?} idle");

            app = app_live_with(lang);
            let live = targets(&app);
            // `dial` pins the transport, so only the rows this is about are
            // compared: the connect row changes name, the switch row does not.
            assert_eq!(live[1], app.strings().disconnect, "{lang:?} live");
            assert_eq!(live[2], idle[2], "the switch row keeps its name");

            let painted = rows(&app);
            let connect: Vec<&Row> = painted
                .iter()
                .filter(|r| r.text == app.strings().disconnect)
                .collect();
            assert_eq!(connect.len(), 1, "{lang:?} rows: {painted:?}");
            // Live, not dimmed: it is the row that hangs the link up.
            assert_eq!(connect[0].style, RowStyle::Action, "{lang:?}");
        }
    }

    /// Connected, the switch-device row goes dim but keeps the cursor's chip:
    /// it is refused with a reason, never silently dead.
    #[test]
    fn the_switch_row_is_dim_but_still_cursable_while_a_link_is_up() {
        let mut app = app_live();
        let painted = rows(&app);
        let switch: Vec<&Row> = painted
            .iter()
            .filter(|r| r.text == app.strings().switch_device)
            .collect();
        assert_eq!(switch.len(), 1);
        assert_eq!(switch[0].style, RowStyle::Hint, "dimmed while live");

        app.sidebar_cursor = 2;
        let painted = rows(&app);
        let marked: Vec<&Row> = painted
            .iter()
            .filter(|r| r.style == RowStyle::Selected)
            .collect();
        assert_eq!(marked.len(), 1, "the cursor vanished from the dim row");
        assert_eq!(marked[0].text, app.strings().switch_device);
    }

    /// Enter on `Disconnect` hangs the link up; Enter on the dimmed switch row
    /// says why it will not, and does not open a scan.
    #[test]
    fn enter_disconnects_and_the_switch_row_refuses_with_a_reason() {
        let mut app = app_live();
        app.sidebar_cursor = 1;
        activate(&mut app);
        assert!(!app.session.is_live(), "the link is still up");

        let mut app = app_live();
        app.sidebar_cursor = 2;
        let before = app.notices.len();
        activate(&mut app);
        assert!(app.session.is_live(), "the refusal did not hang up");
        assert!(!app.ble_scan.is_running(), "and no scan was started");
        assert_eq!(app.notices.len(), before + 1, "the refusal was silent");
        assert_eq!(
            app.notices.last().map(|n| n.text.as_str()),
            Some(app.strings().switch_needs_disconnect)
        );
    }

    /// A LAN session's rows are the dial they build: an address and a token,
    /// both cursable, where a Bluetooth session has a device name to read and a
    /// switch-device row to press. The switch row is not there: there is no
    /// radio to look through.
    #[test]
    fn the_lan_rows_are_the_dial_they_build() {
        let mut app = app_en();
        app.transport = Transport::Lan;

        let planned = targets(&app);
        assert_eq!(
            planned.len(),
            app.sidebar_rows(),
            "the cursor can reach them all"
        );
        assert!(
            !planned.contains(&app.strings().switch_device.to_string()),
            "a LAN session has no device to switch to: {planned:?}"
        );
        assert!(planned[1].contains(app.strings().lan_host_label));
        assert!(planned[2].contains(app.strings().lan_token_label));
        assert_eq!(planned[3], app.strings().connect);

        // The board's own address and a token in hand fill the rows; the token
        // is stars, never the token.
        app.network.wifi = Some(crate::accessory::WifiStatus {
            state: "connected".into(),
            ssid: "Bench".into(),
            ip: "192.168.0.100".into(),
        });
        app.lan_status.token = "0123456789abcdef0123456789abcdef".into();
        let planned = targets(&app);
        assert!(planned[1].contains("192.168.0.100"), "{planned:?}");
        assert!(planned[2].contains("******"), "{planned:?}");
        assert!(
            !planned[2].contains("0123456789"),
            "the row spells the token out: {planned:?}"
        );

        // The painted rows carry the same words, and both are cursable.
        let painted = rows(&app);
        for text in [&planned[1], &planned[2]] {
            let row = painted
                .iter()
                .find(|r| &r.text == text)
                .unwrap_or_else(|| panic!("no row for {text:?}: {painted:?}"));
            assert!(row.selectable(), "{text:?} is not selectable");
        }
        assert!(
            !painted
                .iter()
                .any(|r| r.text == app.strings().switch_device),
            "the switch row is painted in LAN mode"
        );
    }

    /// Enter on the LAN rows is what sets them, and Enter on Connect builds the
    /// dial from them — or says what the rows are missing.
    #[test]
    fn enter_on_the_lan_rows_opens_their_prompts() {
        let mut app = app_en();
        app.transport = Transport::Lan;
        app.lan_host = "192.168.1.50".into();

        app.sidebar_cursor = 1;
        activate(&mut app);
        match &app.dialog {
            Some(crate::dialogs::Dialog::Prompt(prompt)) => {
                assert_eq!(prompt.action, crate::dialogs::PromptAction::LanHost);
                assert_eq!(
                    prompt.field.answer(),
                    "192.168.1.50",
                    "the row is the field"
                );
            }
            other => panic!("no host prompt: {other:?}"),
        }

        app.dialog = None;
        app.sidebar_cursor = 2;
        activate(&mut app);
        match &app.dialog {
            Some(crate::dialogs::Dialog::Prompt(prompt)) => {
                assert_eq!(prompt.action, crate::dialogs::PromptAction::LanToken);
                assert!(prompt.masked, "the token prompt opens masked");
            }
            other => panic!("no token prompt: {other:?}"),
        }

        // Connect with no address to go to says so rather than dialling
        // nowhere.
        app.dialog = None;
        app.lan_host.clear();
        app.sidebar_cursor = 3;
        activate(&mut app);
        assert_eq!(
            app.notices.last().map(|n| n.text.as_str()),
            Some(app.strings().lan_needs_host)
        );
        assert!(app.dialing.is_none(), "nothing was dialled");
    }

    /// The plan is the design's, row for row: the sections land where the
    /// screenshots put them, gaps included.
    #[test]
    fn the_plan_matches_the_designed_rows() {
        let app = app_en();
        let plan: Vec<(RowStyle, u8)> = rows(&app).iter().map(|r| (r.style, r.indent)).collect();
        use RowStyle::*;
        assert_eq!(
            plan,
            vec![
                (Header, 0),
                (State, 0),
                (Selected, 2),
                (Label, 2),
                (Action, 2),
                (Action, 2),
                (Gap, 0),
                (Header, 0),
                (ActiveView, 2),
                (Action, 2),
                (Action, 2),
                (Action, 2),
                (Gap, 0),
                (Header, 0),
                (Hint, 0),
                (Action, 2),
                (Action, 2),
                (Action, 2),
                (Action, 2),
                (Dangerous, 2),
                (Gap, 0),
                (Header, 0),
                (Hint, 3),
            ]
        );
    }
}
