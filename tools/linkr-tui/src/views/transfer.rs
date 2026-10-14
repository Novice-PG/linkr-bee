//! Transfer view: one form, one precheck, two channels.
//!
//! The model is the form plus what the session layer recorded; the painter is a
//! pure function from those plus a width. The form is four rows the keyboard
//! walks — which way the file goes, which two paths it names, and the three
//! actions — and above the history of what has moved.
//!
//! **The precheck decides the channel, and nothing is typed before it
//! answers.** `web/target_files.js` never asks: a browser has no `sz` to offer,
//! so the web client always pages a file with `dd | base64`. This program is
//! not a browser and can be one end of a ZMODEM session, so the question is
//! real and the answer comes from the device — see [`crate::transfer::probe`].
//! A board with `lrzsz` gets a block session; a busybox-only board gets the
//! pager; a board with neither cannot take a file at all, and the view says so
//! rather than starting a run that can only fail.
//!
//! The engine underneath is [`crate::transfer::zmodem`]; the painter never
//! talks to it directly, which is why a whole view can be pinned to a made-up
//! state without a link, a clock or a disk.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::{Line, Span};

use crate::app::{App, Notice};
use crate::event::NoticeLevel;
use crate::text;
use crate::theme::Theme;
use crate::transfer::probe::{Channel, ProbeReport};

/// One transfer, as the session layer records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferRecord {
    /// The file's name, as shown (target path for downloads, source for
    /// uploads).
    pub name: String,
    /// Bytes moved so far, when known.
    pub bytes: Option<u64>,
    /// The total size, when known.
    pub total: Option<u64>,
    /// Whether this record is an upload (to the device).
    pub upload: bool,
    /// How the transfer ended.
    pub state: RecordState,
}

/// How a recorded transfer ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordState {
    Pending,
    Saved,
    Unverified,
    Failed,
}

/// Which way the file goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Direction {
    /// This host offers a file to the device.
    #[default]
    Send,
    /// This host takes whatever the device sends.
    Recv,
}

/// One row the form can be stood on, in the order the keyboard walks them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TransferRow {
    /// Which way the file goes.
    #[default]
    Direction,
    /// The path on this machine: a file to send, a directory to receive into.
    Local,
    /// The path on the device.
    Target,
    /// The three actions.
    Actions,
}

/// Every row, in the order the view draws them.
pub const ROWS: [TransferRow; 4] = [
    TransferRow::Direction,
    TransferRow::Local,
    TransferRow::Target,
    TransferRow::Actions,
];

impl TransferRow {
    /// The next row down, wrapping.
    fn next(self) -> TransferRow {
        self.step(1)
    }

    /// The next row up, wrapping.
    fn previous(self) -> TransferRow {
        self.step(-1)
    }

    fn step(self, direction: isize) -> TransferRow {
        let here = ROWS
            .iter()
            .position(|row| *row == self)
            .expect("every row is in the table");
        let count = ROWS.len() as isize;
        ROWS[(here as isize + direction).rem_euclid(count) as usize]
    }
}

/// The three actions, left to right.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    /// Ask the device what it has, and pick the channel off the answer.
    Check,
    /// Start the run the form describes.
    Start,
    /// Stop the run in flight.
    Abort,
}

/// Every action, in the order the row draws them.
pub const ACTS: [Act; 3] = [Act::Check, Act::Start, Act::Abort];

impl Act {
    /// The action's name in the interface language.
    fn label(self, app: &App) -> &'static str {
        let s = app.strings();
        match self {
            Act::Check => s.xfer_check,
            Act::Start => s.xfer_start,
            Act::Abort => s.xfer_abort,
        }
    }

    /// Whether pressing it could do anything.
    ///
    /// An action the form would refuse is drawn as one it would refuse: the row
    /// never promises a start that [`App::start_transfer_from_form`] would turn
    /// straight back into a notice.
    fn enabled(self, app: &App) -> bool {
        match self {
            Act::Check => app.connected() && !app.transferring() && !app.transfer.probing,
            Act::Start => {
                app.connected()
                    && !app.transferring()
                    && app.transfer.probe.is_some()
                    && app
                        .transfer
                        .probe
                        .as_ref()
                        .is_some_and(|p| p.channel().is_some())
            }
            Act::Abort => app.transferring(),
        }
    }
}

/// Everything the transfer view knows. Filled by the session layer and by the
/// keyboard; read here as plain data.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TransferState {
    /// What has moved, oldest first.
    pub records: Vec<TransferRecord>,
    /// Which way the next run goes.
    pub direction: Direction,
    /// The path on this machine, as typed.
    pub local: String,
    /// The path on the device, as typed.
    pub target: String,
    /// The row the keyboard is on.
    pub focus: TransferRow,
    /// The cursor on the action row.
    pub action: usize,
    /// What the device said it has. `None` until a precheck has answered.
    pub probe: Option<ProbeReport>,
    /// A precheck is out and has not answered.
    pub probing: bool,
    /// Something the *form* has to say — a refusal, an abort, a missing link.
    pub message: String,
}

/// How many records the view keeps, oldest dropped. A session that moved a
/// hundred files should not grow the frame for ever, and the log dialog is where
/// a longer history belongs.
pub const MAX_RECORDS: usize = 100;

impl TransferState {
    /// The channel a run would take, when the precheck has answered.
    pub fn channel(&self) -> Option<Channel> {
        self.probe.as_ref()?.channel()
    }

    /// Whether anything has moved in this session.
    pub fn has_records(&self) -> bool {
        !self.records.is_empty()
    }

    /// Whether an upload is the direction the form is set to.
    pub fn uploading(&self) -> bool {
        self.direction == Direction::Send
    }

    /// Move the focus down one row, wrapping.
    pub fn focus_next(&mut self) {
        self.focus = self.focus.next();
    }

    /// Move the focus up one row, wrapping.
    pub fn focus_previous(&mut self) {
        self.focus = self.focus.previous();
    }

    /// Turn the direction row over.
    pub fn toggle_direction(&mut self) {
        self.direction = match self.direction {
            Direction::Send => Direction::Recv,
            Direction::Recv => Direction::Send,
        };
    }

    /// Move the action cursor, wrapping.
    pub fn move_action(&mut self, forward: bool) {
        let len = ACTS.len();
        self.action = if forward {
            (self.action + 1) % len
        } else {
            (self.action + len - 1) % len
        };
    }

    /// The action the cursor is on.
    pub fn act(&self) -> Act {
        ACTS.get(self.action).copied().unwrap_or(Act::Check)
    }

    /// Record a file that has begun. One record per file per direction, in the
    /// order the far ends started them.
    pub fn started(&mut self, name: String, total: Option<u64>, upload: bool) {
        self.push(TransferRecord {
            name,
            bytes: Some(0),
            total,
            upload,
            state: RecordState::Pending,
        });
    }

    /// Record the end of a file, matching the record that began it.
    ///
    /// A file that never announced itself is still recorded: the fact that it
    /// went is worth more than the tidiness of a one-to-one pairing, and the
    /// alternative — dropping it — would be a transfer that happened and is not
    /// on screen.
    pub fn ended(&mut self, name: &str, upload: bool, state: RecordState, bytes: Option<u64>) {
        if let Some(record) = self.pending(name, upload) {
            record.state = state;
            if let Some(bytes) = bytes {
                record.bytes = Some(bytes);
                // A file whose size the far end never declared still has one:
                // it is what arrived.
                record.total.get_or_insert(bytes);
            }
            return;
        }
        self.push(TransferRecord {
            name: name.to_string(),
            bytes,
            total: bytes,
            upload,
            state,
        });
    }

    /// Move the counter of the file in flight. Byte counts are not events, so
    /// they do not go through [`Self::ended`] — a record would be the wrong
    /// shape for a value that changes every kilobyte.
    pub fn counted(&mut self, name: &str, upload: bool, bytes: u64, total: Option<u64>) {
        if let Some(record) = self.pending(name, upload) {
            record.bytes = Some(bytes);
            if total.is_some() {
                record.total = total;
            }
        }
    }

    /// Everything still in flight is over, and did not finish. Used when the
    /// link goes away: a row left reading "running" over a session that has
    /// ended is a row that lies.
    pub fn abandon(&mut self) {
        for record in &mut self.records {
            if record.state == RecordState::Pending {
                record.state = RecordState::Failed;
            }
        }
    }

    /// The newest unfinished record for a file. Newest, because a name may be
    /// sent twice in one session and it is the later one that is in flight.
    fn pending(&mut self, name: &str, upload: bool) -> Option<&mut TransferRecord> {
        self.records.iter_mut().rev().find(|record| {
            record.upload == upload && record.name == name && record.state == RecordState::Pending
        })
    }

    fn push(&mut self, record: TransferRecord) {
        self.records.push(record);
        while self.records.len() > MAX_RECORDS {
            self.records.remove(0);
        }
    }
}

// --- keys -------------------------------------------------------------------

/// One key press inside the transfer view. Returns whether the key was
/// consumed: `Esc` never arrives here, the global router resolves it first.
pub fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    let plain = key.modifiers == KeyModifiers::NONE || key.modifiers == KeyModifiers::SHIFT;
    match key.code {
        KeyCode::Down | KeyCode::Tab if plain => {
            app.transfer.focus_next();
            true
        }
        KeyCode::Up | KeyCode::BackTab if plain => {
            app.transfer.focus_previous();
            true
        }
        KeyCode::Enter if key.modifiers == KeyModifiers::NONE => {
            activate(app);
            true
        }
        KeyCode::Left | KeyCode::Right => {
            let forward = key.code == KeyCode::Right;
            match app.transfer.focus {
                TransferRow::Direction => app.transfer.toggle_direction(),
                TransferRow::Actions => app.transfer.move_action(forward),
                // In a path row the arrow keys do nothing: the row is a button
                // that opens a prompt, not a field with a cursor in it.
                TransferRow::Local | TransferRow::Target => {}
            }
            true
        }
        _ => false,
    }
}

/// Do what the focused row says.
fn activate(app: &mut App) {
    match app.transfer.focus {
        TransferRow::Direction => app.transfer.toggle_direction(),
        // A path is typed in a prompt rather than into the row: the row is one
        // line of a form, and a one-line field that owns every printable key
        // would take the keys the view needs for its own shortcuts.
        TransferRow::Local => crate::dialogs::prompt_transfer_local(app),
        TransferRow::Target => crate::dialogs::prompt_transfer_target(app),
        TransferRow::Actions => match app.transfer.act() {
            // Abort is the one action whose handler does not re-check what the
            // form can do: `check` and `start` each revalidate and answer with
            // the reason, but an abort that stopped a run which is not there
            // would repaint an untouched view as "Transfer aborted". Drawn
            // disabled, it must act disabled.
            Act::Abort if !Act::Abort.enabled(app) => {}
            Act::Abort => {
                app.cancel_transfer();
            }
            Act::Check => {
                app.run_transfer_probe();
            }
            Act::Start => {
                // A refusal is announced as a notice and written into the
                // form's own line, so a click that did nothing is not silent.
                app.start_transfer_from_form();
            }
        },
    }
}

// --- painting ---------------------------------------------------------------

/// The label column, in display columns. Chinese labels are two columns per
/// glyph, so a padded label column is what keeps the values in line across
/// languages.
const LABEL_COLS: usize = 12;

/// A section heading in the accent colour.
fn heading(text: &str, width: u16, theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(
        text::clip_to(text, width as usize),
        theme.section_header(),
    ))
}

/// A muted line, clipped to the pane.
fn note(text: &str, width: u16, theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(
        text::clip_to(text, width as usize),
        theme.muted(),
    ))
}

/// One `label  value` row of the form, with the label padded in display columns
/// so the value column starts in the same place in both languages.
fn field_row(label: &str, value: &str, selected: bool, width: u16, theme: &Theme) -> Line<'static> {
    let budget = width as usize;
    let indent = 2;
    let padding = LABEL_COLS.saturating_sub(text::columns(label));
    let text = format!(
        "{}{}{}{}",
        " ".repeat(indent),
        label,
        " ".repeat(padding),
        value
    );
    if selected {
        // The design paints the selected row as a full chip, marker included.
        let chip = format!("▸ {}", text::clip_to(&text, budget.saturating_sub(2)));
        return Line::from(Span::styled(chip, theme.selected()));
    }
    Line::from(Span::styled(text::clip_to(&text, budget), theme.text()))
}

/// The action row: three chips, the cursor's one picked out, and an action the
/// form would refuse drawn as one.
fn action_row(app: &App, width: u16, theme: &Theme) -> Line<'static> {
    let cursor = app.transfer.action;
    let mut spans = vec![Span::styled("  ", theme.text())];
    for (index, act) in ACTS.iter().enumerate() {
        let enabled = act.enabled(app);
        let style = if !enabled {
            theme.muted()
        } else if index == cursor {
            theme.selected()
        } else {
            theme.focus()
        };
        if index > 0 {
            spans.push(Span::styled(" ", theme.text()));
        }
        spans.push(Span::styled(format!("[{}]", act.label(app)), style));
    }
    let mut line = Line::from(spans);
    // One row, whatever the pane: a wrapped action row would put the third
    // action on a line of its own and read as a fourth row of the form.
    let flat: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    if text::columns(&flat) > width as usize {
        line = Line::from(Span::styled(
            text::clip_to(&flat, width as usize),
            theme.text(),
        ));
    }
    line
}

/// What the precheck says, as one line.
fn precheck_line(app: &App, width: u16, theme: &Theme) -> Line<'static> {
    let s = app.strings();
    let state = &app.transfer;
    if state.probing {
        return note(s.xfer_probe_running, width, theme);
    }
    let Some(report) = &state.probe else {
        return note(s.xfer_probe_idle, width, theme);
    };
    let (text, style) = match report.channel() {
        Some(Channel::Zmodem) => (s.xfer_probe_zmodem, theme.accent_strong()),
        Some(Channel::Base64) => (s.xfer_probe_pager, theme.warn()),
        None => (s.xfer_probe_none, theme.danger()),
    };
    Line::from(Span::styled(text::clip_to(text, width as usize), style))
}

/// The record's state, as a word in the interface language.
fn state_word(state: RecordState, app: &App) -> &'static str {
    let s = app.strings();
    match state {
        RecordState::Pending => s.xfer_record_pending,
        RecordState::Saved => s.xfer_record_saved,
        RecordState::Unverified => s.xfer_record_unverified,
        RecordState::Failed => s.xfer_record_failed,
    }
}

/// A human size: `1.2 KB / 3.4 MB`, or whatever of the two is known.
fn size_text(bytes: Option<u64>, total: Option<u64>) -> String {
    match (bytes, total) {
        (Some(done), Some(total)) if total > 0 => {
            format!("{} / {}", text::human_bytes(done), text::human_bytes(total))
        }
        (Some(done), _) => text::human_bytes(done),
        (None, Some(total)) => text::human_bytes(total),
        (None, None) => String::new(),
    }
}

/// One record row: the file's name in body text, then ` · size · state` in the
/// state's colour. A name too wide for the pane carries the state's colour
/// itself, so the row never loses its outcome.
fn record_line(record: &TransferRecord, app: &App, width: u16, theme: &Theme) -> Line<'static> {
    let size = size_text(record.bytes, record.total);
    let word = state_word(record.state, app);
    let style = match record.state {
        RecordState::Saved => theme.accent_strong(),
        RecordState::Failed => theme.danger(),
        RecordState::Unverified => theme.warn(),
        RecordState::Pending => theme.muted(),
    };
    let name_cols = text::columns(&record.name) + 2;
    let budget = width as usize;
    let rest = if size.is_empty() {
        format!(" · {word}")
    } else {
        format!(" · {size} · {word}")
    };
    if name_cols < budget {
        Line::from(vec![
            Span::styled(format!("  {}", record.name), theme.text()),
            Span::styled(text::clip_to(&rest, budget - name_cols), style),
        ])
    } else {
        Line::from(Span::styled(
            text::clip_to(&format!("  {}", record.name), budget),
            style,
        ))
    }
}

/// The run in flight, as its direction, its file and its counter.
fn progress_line(app: &App, width: u16, theme: &Theme) -> Line<'static> {
    let s = app.strings();
    let Some(live) = app.live_transfer.as_ref() else {
        return Line::from("");
    };
    let word = if live.upload {
        s.xfer_sending
    } else {
        s.xfer_receiving
    };
    let detail = match live.progress() {
        Some(progress) => format!(
            "{} · {}",
            progress.name,
            size_text(Some(progress.bytes), progress.total)
        ),
        None => live.target.clone(),
    };
    Line::from(Span::styled(
        text::clip_to(&format!("{word} {detail}"), width as usize),
        theme.accent_strong(),
    ))
}

/// The whole body: the form, the precheck, the run in flight, then the history
/// of what has moved.
pub fn lines(app: &App, theme: &Theme, width: u16) -> Vec<Line<'static>> {
    let s = app.strings();
    let state = &app.transfer;
    let mut lines = vec![
        Line::from(""),
        heading(
            app.lang.view_name(crate::event::View::Transfer),
            width,
            theme,
        ),
        Line::from(""),
    ];

    // --- the form ---
    let direction_word = match state.direction {
        Direction::Send => s.xfer_dir_send,
        Direction::Recv => s.xfer_dir_recv,
    };
    lines.push(field_row(
        s.xfer_direction,
        direction_word,
        state.focus == TransferRow::Direction,
        width,
        theme,
    ));
    let local_value = if state.local.is_empty() {
        "—"
    } else {
        &state.local
    };
    lines.push(field_row(
        s.xfer_local_row,
        local_value,
        state.focus == TransferRow::Local,
        width,
        theme,
    ));
    // ZMODEM's far end names the file itself, so an empty device path is not
    // missing information — it is the answer.
    let target_value = if state.target.is_empty() {
        s.xfer_target_by_device
    } else {
        &state.target
    };
    lines.push(field_row(
        s.xfer_target_row,
        target_value,
        state.focus == TransferRow::Target,
        width,
        theme,
    ));
    lines.push(action_row(app, width, theme));

    // --- the precheck ---
    lines.push(Line::from(""));
    lines.push(heading(s.xfer_precheck, width, theme));
    lines.push(precheck_line(app, width, theme));
    let channel = match state.channel() {
        Some(channel) => channel.as_str().to_string(),
        None => "—".to_string(),
    };
    lines.push(field_row(s.xfer_channel_row, &channel, false, width, theme));

    // --- the run in flight ---
    if app.transferring() {
        lines.push(Line::from(""));
        lines.push(heading(s.xfer_status_row, width, theme));
        lines.push(progress_line(app, width, theme));
        lines.push(note(s.xfer_console_locked, width, theme));
    }

    // What the form has to say: a refusal, an abort, a link that is not there.
    if !state.message.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            text::clip_to(&state.message, width as usize),
            theme.warn(),
        )));
    }

    // --- the history ---
    lines.push(Line::from(""));
    lines.push(heading(s.xfer_out_section, width, theme));
    let mut shown = false;
    for record in state.records.iter().filter(|r| r.upload) {
        lines.push(record_line(record, app, width, theme));
        shown = true;
    }
    if !shown {
        lines.push(note(&format!("  {}", s.xfer_empty), width, theme));
    }

    lines.push(Line::from(""));
    lines.push(heading(s.xfer_in_section, width, theme));
    let mut shown = false;
    for record in state.records.iter().filter(|r| !r.upload) {
        lines.push(record_line(record, app, width, theme));
        shown = true;
    }
    if !shown {
        lines.push(note(&format!("  {}", s.xfer_empty), width, theme));
    }

    lines.push(Line::from(""));
    lines.push(note(s.xfer_keys_hint, width, theme));
    lines
}

/// Announce something the form has to say, in the pane and in the notice stack.
///
/// The notice is for whoever is no longer looking at this view; the `message`
/// is for whoever is. Both, because a transfer is started from a pane the
/// operator then leaves to watch the console.
pub fn say(app: &mut App, text: &str, level: NoticeLevel) {
    app.transfer.message = text.to_string();
    app.push_notice(Notice {
        text: text.to_string(),
        level,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Lang;
    use crate::theme::Theme;

    fn app() -> App {
        App {
            lang: Lang::En,
            ..App::default()
        }
    }

    fn body_text(rows: &[Line<'static>]) -> String {
        rows.iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("")
    }

    fn probe_with(tools: &[&str]) -> ProbeReport {
        ProbeReport {
            tools: tools.iter().map(|t| (*t).to_string()).collect(),
        }
    }

    #[test]
    fn the_form_shows_all_four_rows() {
        let theme = Theme::DARK;
        let mut a = app();
        a.transfer.local = "/tmp/uImage".to_string();
        a.transfer.target = "/boot/uImage".to_string();
        let text = body_text(&lines(&a, &theme, 80));
        assert!(text.contains("Direction"), "{text}");
        assert!(text.contains("On this host"), "{text}");
        assert!(text.contains("On the device"), "{text}");
        assert!(text.contains("/tmp/uImage"), "{text}");
        assert!(text.contains("[Check]"), "{text}");
        assert!(text.contains("[Start]"), "{text}");
        assert!(text.contains("[Abort]"), "{text}");
    }

    #[test]
    fn an_unrun_precheck_says_so_and_start_is_disabled() {
        let theme = Theme::DARK;
        let a = app();
        let text = body_text(&lines(&a, &theme, 80));
        assert!(text.contains("not run yet"), "{text}");
        assert!(!Act::Start.enabled(&a));
    }

    #[test]
    fn a_device_with_lrzsz_gets_zmodem() {
        let theme = Theme::DARK;
        let mut a = app();
        a.transfer.probe = Some(probe_with(&["sz", "rz", "dd", "base64", "wc", "tr"]));
        let text = body_text(&lines(&a, &theme, 80));
        assert!(text.contains("ZMODEM"), "{text}");
        assert_eq!(a.transfer.channel(), Some(Channel::Zmodem));
    }

    #[test]
    fn a_device_with_only_the_pager_says_so() {
        let theme = Theme::DARK;
        let mut a = app();
        a.transfer.probe = Some(probe_with(&["dd", "base64", "wc", "tr"]));
        let text = body_text(&lines(&a, &theme, 80));
        assert!(text.contains("no lrzsz"), "{text}");
        assert_eq!(a.transfer.channel(), Some(Channel::Base64));
    }

    #[test]
    fn a_device_with_nothing_refuses_to_start() {
        let theme = Theme::DARK;
        let mut a = app();
        a.transfer.probe = Some(probe_with(&["sh"]));
        let text = body_text(&lines(&a, &theme, 80));
        assert!(text.contains("neither"), "{text}");
        assert_eq!(a.transfer.channel(), None);
        assert!(!Act::Start.enabled(&a));
    }

    #[test]
    fn the_keys_walk_the_form_and_wrap() {
        let mut a = app();
        let start = a.transfer.focus;
        for _ in 0..ROWS.len() {
            a.transfer.focus_next();
        }
        assert_eq!(a.transfer.focus, start);
        a.transfer.focus_previous();
        assert_eq!(a.transfer.focus, TransferRow::Actions);
    }

    #[test]
    fn the_direction_row_turns_over() {
        let mut a = app();
        assert!(a.transfer.uploading());
        a.transfer.toggle_direction();
        assert!(!a.transfer.uploading());
    }

    #[test]
    fn the_action_cursor_wraps_both_ways() {
        let mut a = app();
        assert_eq!(a.transfer.act(), Act::Check);
        a.transfer.move_action(false);
        assert_eq!(a.transfer.act(), Act::Abort);
        a.transfer.move_action(true);
        assert_eq!(a.transfer.act(), Act::Check);
    }

    #[test]
    fn records_are_kept_and_ended() {
        let mut state = TransferState::default();
        state.started("uImage".to_string(), Some(1024), true);
        state.counted("uImage", true, 512, Some(1024));
        state.ended("uImage", true, RecordState::Saved, Some(1024));
        assert_eq!(state.records.len(), 1);
        assert_eq!(state.records[0].bytes, Some(1024));
        assert_eq!(state.records[0].state, RecordState::Saved);
    }

    #[test]
    fn every_row_fits_the_pane() {
        let theme = Theme::DARK;
        for width in [10u16, 24, 40, 70, 100] {
            for lang in [Lang::Zh, Lang::En] {
                let mut a = app();
                a.lang = lang;
                a.transfer.local = "/tmp/a-very-long-path/that-keeps-going/uImage".to_string();
                a.transfer.target = "/boot/a-very-long-path/that-keeps-going/uImage".to_string();
                a.transfer.probe = Some(probe_with(&["sz", "rz"]));
                a.transfer
                    .started("一个很长很长很长的文件名".to_string(), Some(4096), true);
                for line in lines(&a, &theme, width) {
                    let joined: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                    assert!(
                        text::columns(&joined) <= width as usize,
                        "{lang:?} at {width}: {joined:?}"
                    );
                }
            }
        }
    }
}
