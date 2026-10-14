//! Transfer view: what has been sent to and taken from the target, over the
//! ZMODEM session of `src/transfer/zmodem.rs`.
//!
//! The model is the session layer's list of transfer records; the painter is a
//! pure function from those records plus a width. Two sections, matching the
//! direction of each transfer, plus the command hints for starting one. A
//! transfer that is still running is headed by a banner naming its direction and
//! its destination, and its file is an ordinary record whose counter moves.

use ratatui::text::{Line, Span};

use crate::app::App;
use crate::text;
use crate::theme::Theme;

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
    pub state: TransferState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferState {
    Pending,
    Saved,
    Unverified,
    Failed,
}

/// Everything the transfer view knows.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TransferState_ {
    pub records: Vec<TransferRecord>,
}

/// How many records the view keeps, oldest dropped. A session that moved a
/// hundred files should not grow the frame for ever, and the log dialog is where
/// a longer history belongs.
pub const MAX_RECORDS: usize = 100;

impl TransferState_ {
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
            state: TransferState::Pending,
        });
    }

    /// Record the end of a file, matching the record that began it.
    ///
    /// A file that never announced itself is still recorded: the fact that it
    /// went is worth more than the tidiness of a one-to-one pairing, and the
    /// alternative — dropping it — would be a transfer that happened and is not
    /// on screen.
    pub fn ended(&mut self, name: &str, upload: bool, state: TransferState, bytes: Option<u64>) {
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
            if record.state == TransferState::Pending {
                record.state = TransferState::Failed;
            }
        }
    }

    /// The newest unfinished record for a file. Newest, because a name may be
    /// sent twice in one session and it is the later one that is in flight.
    fn pending(&mut self, name: &str, upload: bool) -> Option<&mut TransferRecord> {
        self.records.iter_mut().rev().find(|record| {
            record.upload == upload && record.name == name && record.state == TransferState::Pending
        })
    }

    fn push(&mut self, record: TransferRecord) {
        self.records.push(record);
        while self.records.len() > MAX_RECORDS {
            self.records.remove(0);
        }
    }
}

/// The word for a record's end state, in the interface language.
fn state_word(state: TransferState, app: &App) -> &'static str {
    let s = app.strings();
    match state {
        TransferState::Pending => s.xfer_record_pending,
        TransferState::Saved => s.xfer_record_saved,
        TransferState::Unverified => s.xfer_record_unverified,
        TransferState::Failed => s.xfer_record_failed,
    }
}

/// A human size: `1.2 KB`, `3.4 MB`, bytes below a kilobyte.
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

/// The banner over the two sections while a transfer owns the link.
///
/// The heading names the direction and where the files are going, because those
/// are the two things the operator asked for and the two things a half-finished
/// transfer can get wrong. The file in flight is a record like any other; this
/// only says which session it belongs to.
fn banner(app: &App, width: u16, theme: &Theme) -> Line<'static> {
    let s = app.strings();
    let live = app.live_transfer.as_ref().expect("checked by the caller");
    let (word, detail) = if live.upload {
        (
            s.xfer_started_out,
            format!("{} ({})", live.target, live.files),
        )
    } else {
        (s.xfer_started_in, live.target.clone())
    };
    Line::from(Span::styled(
        text::clip_to(&format!("  {word}{detail}"), width as usize),
        theme.accent_strong(),
    ))
}

/// One record row: the file's name in body text, then ` · size · state` in the
/// state's colour. A name too wide for the pane carries the state's colour
/// itself, so the row never loses its outcome.
fn record_line(record: &TransferRecord, app: &App, width: u16, theme: &Theme) -> Line<'static> {
    let size = size_text(record.bytes, record.total);
    let word = state_word(record.state, app);
    let style = match record.state {
        TransferState::Saved => theme.accent_strong(),
        TransferState::Failed => theme.danger(),
        TransferState::Unverified => theme.warn(),
        TransferState::Pending => theme.muted(),
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

/// The whole body: heading, hints, then the two direction sections.
pub fn lines(app: &App, theme: &Theme, width: u16) -> Vec<Line<'static>> {
    let s = app.strings();
    let state = &app.transfer;
    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            text::clip_to(
                app.lang.view_name(crate::event::View::Transfer),
                width as usize,
            ),
            theme.section_header(),
        )),
        Line::from(""),
        Line::from(Span::styled(
            text::clip_to(&format!("  {}", s.xfer_upload_hint), width as usize),
            theme.muted(),
        )),
        Line::from(Span::styled(
            text::clip_to(&format!("  {}", s.xfer_download_hint), width as usize),
            theme.muted(),
        )),
        Line::from(Span::styled(
            text::clip_to(&format!("  {}", s.xfer_zmodem_hint), width as usize),
            theme.muted(),
        )),
        Line::from(""),
    ];

    // The session in flight, before the two sections: it is the newest thing
    // that has happened, and its record is at the bottom of its section.
    if app.live_transfer.is_some() {
        lines.push(banner(app, width, theme));
        lines.push(Line::from(""));
    }

    let uploads: Vec<&TransferRecord> = state.records.iter().filter(|r| r.upload).collect();
    let downloads: Vec<&TransferRecord> = state.records.iter().filter(|r| !r.upload).collect();

    lines.push(Line::from(Span::styled(
        text::clip_to(s.xfer_out_section, width as usize),
        theme.focus(),
    )));
    if uploads.is_empty() {
        lines.push(Line::from(Span::styled(
            text::clip_to(&format!("  {}", s.xfer_empty), width as usize),
            theme.muted(),
        )));
    } else {
        for record in uploads {
            lines.push(record_line(record, app, width, theme));
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        text::clip_to(s.xfer_in_section, width as usize),
        theme.focus(),
    )));
    if downloads.is_empty() {
        lines.push(Line::from(Span::styled(
            text::clip_to(&format!("  {}", s.xfer_empty), width as usize),
            theme.muted(),
        )));
    } else {
        for record in downloads {
            lines.push(record_line(record, app, width, theme));
        }
    }

    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Lang;

    fn app(lang: Lang) -> App {
        App {
            lang,
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

    #[test]
    fn an_empty_view_names_both_directions() {
        let theme = Theme::DARK;
        let a = app(Lang::En);
        let text = body_text(&lines(&a, &theme, 70));
        assert!(text.contains("To the device"), "{text}");
        assert!(text.contains("From the device"), "{text}");
        assert!(text.contains("put <local file>"), "{text}");
        assert!(text.contains("get <target path>"), "{text}");
        // Both sections show the empty note.
        assert_eq!(text.matches("No transfers yet.").count(), 2, "{text}");
    }

    #[test]
    fn records_split_by_direction_and_carry_their_state() {
        let theme = Theme::DARK;
        let mut a = app(Lang::En);
        a.transfer.records = vec![
            TransferRecord {
                name: "fw.bin".into(),
                bytes: Some(1024),
                total: Some(2048),
                upload: true,
                state: TransferState::Pending,
            },
            TransferRecord {
                name: "dmesg.log".into(),
                bytes: Some(4096),
                total: None,
                upload: false,
                state: TransferState::Saved,
            },
        ];
        let text = body_text(&lines(&a, &theme, 70));
        assert!(text.contains("fw.bin"), "{text}");
        assert!(text.contains("1.0 KB / 2.0 KB"), "{text}");
        assert!(text.contains("running"), "{text}");
        assert!(text.contains("dmesg.log"), "{text}");
        assert!(text.contains("saved"), "{text}");
    }

    #[test]
    fn sizes_are_human() {
        assert_eq!(size_text(Some(512), None), "512 B");
        assert_eq!(size_text(Some(1024), None), "1.0 KB");
        assert_eq!(size_text(Some(1_500_000), None), "1.4 MB");
        assert_eq!(size_text(None, Some(2048)), "2.0 KB");
        assert_eq!(size_text(None, None), "");
    }

    #[test]
    fn colours_track_the_state() {
        let theme = Theme::DARK;
        let mut a = app(Lang::En);
        a.transfer.records = vec![
            TransferRecord {
                name: "ok.bin".into(),
                bytes: None,
                total: None,
                upload: false,
                state: TransferState::Saved,
            },
            TransferRecord {
                name: "bad.bin".into(),
                bytes: None,
                total: None,
                upload: false,
                state: TransferState::Failed,
            },
        ];
        let painted = lines(&a, &theme, 70);
        let flat: Vec<(String, Option<ratatui::style::Color>)> = painted
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| (s.content.to_string(), s.style.fg))
            .collect();
        // The state word carries the state's colour; the file name is body text.
        let ok = flat.iter().find(|(t, _)| t.contains("saved")).unwrap();
        assert_eq!(ok.1, Some(theme.accent_strong));
        let bad = flat.iter().find(|(t, _)| t.contains("failed")).unwrap();
        assert_eq!(bad.1, Some(theme.danger));
        let name = flat.iter().find(|(t, _)| t.contains("ok.bin")).unwrap();
        assert_eq!(name.1, Some(theme.text));
    }

    #[test]
    fn every_row_fits_the_pane() {
        let theme = Theme::DARK;
        for width in [10u16, 24, 40, 70, 100] {
            for lang in [Lang::Zh, Lang::En] {
                let mut a = app(lang);
                a.transfer.records = vec![TransferRecord {
                    name: "a-very-long-file-name-that-must-not-overflow.bin".into(),
                    bytes: Some(123456),
                    total: Some(654321),
                    upload: true,
                    state: TransferState::Unverified,
                }];
                // The banner carries a path the operator typed, which is as long
                // as anything a terminal will ever see.
                a.live_transfer = Some(crate::transfer::zmodem::LiveTransfer::receive(
                    "/a/directory/that/is/deliberately/rather/deep/for/a/narrow/pane",
                ));
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

    /// A record is opened by the start of a file, moved by the counter while it
    /// is in flight, and closed by its end — the three shapes the engine reports,
    /// which are deliberately not the same shape.
    #[test]
    fn a_record_follows_a_file_from_start_to_end() {
        let mut state = TransferState_::default();
        state.started("fw.bin".into(), Some(2048), true);
        assert_eq!(state.records.len(), 1);
        assert_eq!(state.records[0].state, TransferState::Pending);
        assert_eq!(state.records[0].bytes, Some(0));

        state.counted("fw.bin", true, 1024, Some(2048));
        assert_eq!(state.records[0].bytes, Some(1024), "the counter moves");
        assert_eq!(state.records[0].total, Some(2048));

        state.ended("fw.bin", true, TransferState::Saved, Some(2048));
        assert_eq!(state.records[0].state, TransferState::Saved);
        assert_eq!(state.records[0].bytes, Some(2048));
        assert_eq!(state.records.len(), 1, "one file, one record");

        // A counter for a file that has finished is not a record of its own.
        state.counted("fw.bin", true, 10, None);
        assert_eq!(
            state.records[0].bytes,
            Some(2048),
            "a finished row stays put"
        );
    }

    /// A file the far end never declared a size for still ends up with one: it
    /// is what arrived, and a row with no denominator reads as a row that never
    /// finished.
    #[test]
    fn a_size_the_far_end_never_declared_is_filled_in_by_what_arrived() {
        let mut state = TransferState_::default();
        state.started("dmesg.log".into(), None, false);
        assert_eq!(state.records[0].total, None);
        state.ended("dmesg.log", false, TransferState::Saved, Some(4096));
        assert_eq!(state.records[0].total, Some(4096));
        assert_eq!(state.records[0].bytes, Some(4096));
    }

    /// The two directions do not share a name space: the same file going out and
    /// coming back is two records, and the end of one must not close the other.
    #[test]
    fn the_two_directions_do_not_close_each_others_records() {
        let mut state = TransferState_::default();
        state.started("payload.bin".into(), Some(10), true);
        state.started("payload.bin".into(), Some(10), false);
        state.ended("payload.bin", false, TransferState::Saved, Some(10));
        assert_eq!(state.records[0].state, TransferState::Pending, "the upload");
        assert_eq!(state.records[1].state, TransferState::Saved, "the download");
    }

    /// A file that ends without ever having begun is still recorded — a transfer
    /// that happened and is not on screen is worse than a row with no mate.
    #[test]
    fn a_file_that_never_announced_itself_is_still_recorded() {
        let mut state = TransferState_::default();
        state.ended("surprise.bin", false, TransferState::Failed, None);
        assert_eq!(state.records.len(), 1);
        assert_eq!(state.records[0].state, TransferState::Failed);
        assert_eq!(state.records[0].bytes, None);
    }

    /// Giving up marks what was in flight as failed and leaves the history
    /// alone: the flag going red is about *this* transfer, not the last one.
    #[test]
    fn giving_up_closes_only_what_is_in_flight() {
        let mut state = TransferState_::default();
        state.started("done.bin".into(), Some(4), false);
        state.ended("done.bin", false, TransferState::Saved, Some(4));
        state.started("half.bin".into(), Some(100), false);
        state.abandon();
        assert_eq!(state.records[0].state, TransferState::Saved);
        assert_eq!(state.records[1].state, TransferState::Failed);
    }

    /// The history is bounded, and it drops the oldest: a long session must not
    /// grow the frame without limit.
    #[test]
    fn the_history_is_bounded_and_drops_the_oldest() {
        let mut state = TransferState_::default();
        for index in 0..MAX_RECORDS + 5 {
            state.started(format!("f{index}.bin"), Some(1), true);
        }
        assert_eq!(state.records.len(), MAX_RECORDS);
        assert_eq!(
            state.records.first().map(|record| record.name.as_str()),
            Some("f5.bin"),
            "the oldest went first"
        );
    }

    /// The banner names the direction and where the files are going, and the row
    /// under it is the file in flight — the two halves of "what is happening".
    #[test]
    fn a_live_transfer_is_headed_by_its_direction() {
        let theme = Theme::DARK;
        let mut a = app(Lang::En);
        a.live_transfer = Some(crate::transfer::zmodem::LiveTransfer::receive("/tmp/inbox"));
        a.transfer.started("dmesg.log".into(), Some(4096), false);
        a.transfer.counted("dmesg.log", false, 2048, Some(4096));
        let text = body_text(&lines(&a, &theme, 70));
        assert!(text.contains("receiving into: /tmp/inbox"), "{text}");
        assert!(text.contains("dmesg.log"), "{text}");
        assert!(text.contains("2.0 KB / 4.0 KB"), "{text}");

        let mut a = app(Lang::Zh);
        a.live_transfer = Some(crate::transfer::zmodem::LiveTransfer::receive("/tmp/inbox"));
        let zh = body_text(&lines(&a, &theme, 70));
        assert!(zh.contains("/tmp/inbox"), "{zh}");
        assert!(!zh.contains("receiving into"), "{zh}");
    }

    /// With nothing in flight there is no banner: an empty session is not a
    /// session, and a heading over nothing reads as a stalled transfer.
    #[test]
    fn no_banner_without_a_transfer() {
        let theme = Theme::DARK;
        let a = app(Lang::En);
        let text = body_text(&lines(&a, &theme, 70));
        assert!(!text.contains("receiving into"), "{text}");
        assert!(!text.contains("sending to the device"), "{text}");
    }
}
