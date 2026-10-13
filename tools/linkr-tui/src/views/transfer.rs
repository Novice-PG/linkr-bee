//! Transfer view: what has been sent to and taken from the target, over the
//! base64 shell protocol of `src/transfer/files.rs`.
//!
//! The model is the session layer's list of transfer records; the painter is a
//! pure function from those records plus a width. Two sections, matching the
//! direction of each transfer, plus the command hints for starting one.

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

impl TransferState_ {
    pub fn has_records(&self) -> bool {
        !self.records.is_empty()
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
    let fmt = |b: u64| -> String {
        if b >= 1_048_576 {
            format!("{:.1} MB", b as f64 / 1_048_576.0)
        } else if b >= 1024 {
            format!("{:.1} KB", b as f64 / 1024.0)
        } else {
            format!("{b} B")
        }
    };
    match (bytes, total) {
        (Some(done), Some(total)) if total > 0 => format!("{} / {}", fmt(done), fmt(total)),
        (Some(done), _) => fmt(done),
        (None, Some(total)) => fmt(total),
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
        Line::from(""),
    ];

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
