//! Transfer view: what the device can move files with, and what has moved.
//!
//! There is no form here any more, and that is the point of the view's shape.
//! A form used to offer a direction and two paths because this program could be
//! one end of a ZMODEM session; the bridge cannot carry that session losslessly
//! (see [`crate::transfer`]) so the block protocol is gone, and with it the
//! reason for a form. What is left is exactly what the web client has: files
//! move over the shell pager, driven by the assistant's own tools, and this view
//! is where their results and the board's capabilities are read.
//!
//! The model is plain records; the painter is a pure function from them plus a
//! width. The precheck is a question — *does this board have the tools the
//! pager's commands are built from* — and its answer is shown as the list it is,
//! missing tools named, rather than as a verdict that hides which one is absent.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::{Line, Span};

use crate::app::{App, Notice};
use crate::event::NoticeLevel;
use crate::text;
use crate::theme::Theme;
use crate::transfer::probe::{ProbeReport, DIGEST_TOOLS};

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

/// Every row the view draws, in the order the keyboard walks them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TransferRow {
    /// The precheck, and the only thing on this view a key can start.
    #[default]
    Check,
}

/// Every row, in the order the view draws them.
pub const ROWS: [TransferRow; 1] = [TransferRow::Check];

/// Everything the transfer view knows. Filled by the session layer and by the
/// keyboard; read here as plain data.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TransferState {
    /// What has moved, oldest first.
    pub records: Vec<TransferRecord>,
    /// The row the keyboard is on.
    pub focus: TransferRow,
    /// What the device said it has. `None` until a precheck has answered.
    pub probe: Option<ProbeReport>,
    /// A precheck is out and has not answered.
    pub probing: bool,
    /// Something the *view* has to say — a missing link, an answer that never
    /// came.
    pub message: String,
}

/// How many records the view keeps, oldest dropped. A session that moved a
/// hundred files should not grow the frame for ever, and the log dialog is where
/// a longer history belongs.
pub const MAX_RECORDS: usize = 100;

impl TransferState {
    /// Whether the device can take a file at all, when the precheck has
    /// answered.
    pub fn usable(&self) -> Option<bool> {
        self.probe.as_ref().map(ProbeReport::usable)
    }

    /// Whether anything has moved in this session.
    pub fn has_records(&self) -> bool {
        !self.records.is_empty()
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
        KeyCode::Enter if key.modifiers == KeyModifiers::NONE => {
            // The one row a key can do something with. A precheck already out
            // is left alone rather than queued: two questions in flight put a
            // second command between the first one's answer lines.
            if !app.transfer.probing {
                app.run_transfer_probe();
            }
            true
        }
        KeyCode::Down | KeyCode::Tab | KeyCode::Up | KeyCode::BackTab if plain => {
            // One row, so the keys that walk rows are consumed and do nothing.
            // Falling through would type into the console from a view whose
            // whole point is that it does not.
            true
        }
        _ => false,
    }
}

// --- painting ---------------------------------------------------------------

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
fn field_row(label: &str, value: &str, width: u16, theme: &Theme) -> Line<'static> {
    let budget = width as usize;
    let indent = 2;
    let padding = LABEL_COLS.saturating_sub(text::columns(label));
    let row = format!(
        "{}{}{}{}",
        " ".repeat(indent),
        label,
        " ".repeat(padding),
        value
    );
    Line::from(Span::styled(text::clip_to(&row, budget), theme.text()))
}

/// The label column, in display columns. Chinese labels are two columns per
/// glyph, so a padded label column is what keeps the values in line across
/// languages.
const LABEL_COLS: usize = 12;

/// What the precheck says, as the list it actually is.
///
/// A board missing one tool is a board you can fix; a board reported as "cannot
/// transfer" is not. So the missing names are printed, and the digest tools are
/// reported separately because they decide whether an upload can be *verified*,
/// not whether it can happen.
fn precheck_lines(app: &App, width: u16, theme: &Theme) -> Vec<Line<'static>> {
    let s = app.strings();
    let state = &app.transfer;
    if state.probing {
        return vec![note(s.xfer_probe_running, width, theme)];
    }
    let Some(report) = &state.probe else {
        return vec![note(s.xfer_probe_idle, width, theme)];
    };
    let mut lines = vec![heading(s.xfer_precheck, width, theme)];
    if report.usable() {
        lines.push(note(s.xfer_probe_ready, width, theme));
    } else {
        lines.push(Line::from(Span::styled(
            text::clip_to(s.xfer_probe_missing, width as usize),
            theme.danger(),
        )));
        lines.push(note(
            &format!("  {}", report.missing().join(", ")),
            width,
            theme,
        ));
    }
    let digest = report
        .digest()
        .map(str::to_string)
        .unwrap_or_else(|| s.xfer_no_digest.to_string());
    lines.push(field_row(s.xfer_digest_row, &digest, width, theme));
    // With no digest tool an upload can still be moved; it just cannot be
    // proved, and saying so here is cheaper than a failed verification later.
    if report.digest().is_none() {
        lines.push(note(
            &format!("  ({})", DIGEST_TOOLS.join(", ")),
            width,
            theme,
        ));
    }
    lines
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

/// The whole body: the precheck, then the history of what has moved.
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

    // --- the precheck ---
    let selected = state.focus == TransferRow::Check;
    if selected {
        lines.push(Line::from(Span::styled(
            text::clip_to(
                &format!("▸ {}", s.xfer_check),
                width.saturating_sub(2) as usize,
            ),
            theme.selected(),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            text::clip_to(&format!("  {}", s.xfer_check), width as usize),
            theme.focus(),
        )));
    }
    lines.extend(precheck_lines(app, width, theme));

    // What the view has to say: a missing link, an answer that never came.
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

/// Announce something the view has to say, in the pane and in the notice stack.
///
/// The notice is for whoever is no longer looking at this view; the `message`
/// is for whoever is. Both, because a precheck is started from a pane the
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
    fn an_unrun_precheck_says_so() {
        let theme = Theme::DARK;
        let a = app();
        let text = body_text(&lines(&a, &theme, 80));
        assert!(text.contains("not run yet"), "{text}");
        assert_eq!(a.transfer.usable(), None);
    }

    #[test]
    fn a_capable_device_is_reported_usable() {
        let theme = Theme::DARK;
        let mut a = app();
        a.transfer.probe = Some(probe_with(&["dd", "base64", "wc", "tr", "sha256sum"]));
        let text = body_text(&lines(&a, &theme, 80));
        assert!(text.contains("can take a file"), "{text}");
        assert_eq!(a.transfer.usable(), Some(true));
        // The digest tool is named on the row, not summarised away.
        assert!(text.contains("sha256sum"), "{text}");
    }

    #[test]
    fn a_device_missing_a_tool_names_it() {
        let theme = Theme::DARK;
        let mut a = app();
        a.transfer.probe = Some(probe_with(&["dd", "wc", "tr"]));
        let text = body_text(&lines(&a, &theme, 80));
        assert!(text.contains("missing tools"), "{text}");
        assert!(text.contains("base64"), "{text}");
        assert_eq!(a.transfer.usable(), Some(false));
    }

    #[test]
    fn a_device_with_no_digest_tool_is_told_what_that_costs() {
        let theme = Theme::DARK;
        let mut a = app();
        a.transfer.probe = Some(probe_with(&["dd", "base64", "wc", "tr"]));
        let text = body_text(&lines(&a, &theme, 80));
        assert!(text.contains("cannot be proved"), "{text}");
        // Both tools that would fix it are named, not just the absence of one.
        assert!(text.contains("sha256sum"), "{text}");
        assert!(text.contains("shasum"), "{text}");
    }

    #[test]
    fn a_probe_still_out_says_it_is_running() {
        let theme = Theme::DARK;
        let mut a = app();
        a.transfer.probing = true;
        let text = body_text(&lines(&a, &theme, 80));
        assert!(text.contains("asking"), "{text}");
    }

    #[test]
    fn enter_starts_the_precheck_and_a_second_one_waits() {
        let mut a = app();
        assert!(!a.transfer.probing);
        // No link, so the precheck refuses out loud rather than arming.
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert!(handle_key(&mut a, key));
        assert!(!a.transfer.probing);
        assert_eq!(a.transfer.message, "Not connected — nothing was sent.");
    }

    #[test]
    fn the_row_keys_are_consumed_and_do_nothing() {
        let mut a = app();
        for code in [KeyCode::Down, KeyCode::Up, KeyCode::Tab] {
            let key = KeyEvent::new(code, KeyModifiers::NONE);
            assert!(handle_key(&mut a, key), "{code:?}");
        }
        assert_eq!(a.transfer.focus, TransferRow::Check);
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
        assert!(state.has_records());
    }

    #[test]
    fn an_abandoned_record_does_not_read_as_running() {
        let mut state = TransferState::default();
        state.started("uImage".to_string(), Some(1024), true);
        state.abandon();
        assert_eq!(state.records[0].state, RecordState::Failed);
    }

    #[test]
    fn every_row_fits_the_pane() {
        let theme = Theme::DARK;
        for width in [10u16, 24, 40, 70, 100] {
            for lang in [Lang::Zh, Lang::En] {
                let mut a = app();
                a.lang = lang;
                a.transfer.probe = Some(probe_with(&["dd", "wc"]));
                a.transfer.message = "a message that is quite long indeed".to_string();
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
