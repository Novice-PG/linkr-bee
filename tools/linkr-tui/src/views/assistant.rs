//! Assistant view: the agent's session, tasks and notes for this device.
//!
//! The model is the agent data layer's plain records; the painter is a pure
//! function from them plus a width. The LLM runtime itself is out of scope for
//! this crate — what is shown here is everything the web panel shows about the
//! conversation's durable state: the configured mode, the token usage, the task
//! history and the device notes.

use ratatui::text::{Line, Span};

use crate::agent::tasks::TaskRecord;
use crate::app::App;
use crate::exec::executor::Mode;
use crate::text;
use crate::theme::Theme;

/// Everything the assistant view knows.
#[derive(Debug, Clone, Default)]
pub struct AssistantState {
    /// Whether a model endpoint is configured.
    pub configured: bool,
    /// The execution mode in force.
    pub mode: Option<Mode>,
    /// Token totals for the conversation.
    pub usage: (u64, u64),
    /// This device's task history, newest first.
    pub tasks: Vec<TaskRecord>,
    /// This device's durable notes.
    pub notes: Vec<(String, String)>,
    /// A task awaiting its approval click, if any.
    pub awaiting: Option<String>,
}

/// The word for a mode, in the interface language.
fn mode_word(mode: Mode, app: &App) -> &'static str {
    let s = app.strings();
    match mode {
        Mode::Auto => s.asst_mode_auto,
        Mode::Manual => s.asst_mode_manual,
        Mode::FullAuto => s.asst_mode_full_auto,
    }
}

/// A section heading in the accent colour.
fn heading(text: &str, width: u16, theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(
        text::clip_to(text, width as usize),
        theme.section_header(),
    ))
}

/// A muted note line.
fn note(text: &str, width: u16, theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(
        text::clip_to(text, width as usize),
        theme.muted(),
    ))
}

/// One `label: value` body row. The label is clipped first, so a pane narrower
/// than the label still gets a row that fits.
fn field_row(label: &str, value: &str, width: u16, theme: &Theme) -> Line<'static> {
    let budget = width as usize;
    let label = text::clip_to(label, budget);
    let label_cols = text::columns(&label);
    let mut spans = vec![Span::styled(label, theme.focus())];
    if label_cols < budget {
        spans.push(Span::styled(
            text::clip_to(value, budget - label_cols),
            theme.text(),
        ));
    }
    Line::from(spans)
}

/// A task row: `- goal — status`, goal in body text, status muted.
fn task_row(task: &TaskRecord, width: u16, theme: &Theme) -> Line<'static> {
    let row = format!("  {} — {}", task.goal, task.status);
    Line::from(Span::styled(
        text::clip_to(&row, width as usize),
        theme.text(),
    ))
}

/// The whole body: session (mode, usage), tasks, notes.
pub fn lines(app: &App, theme: &Theme, width: u16) -> Vec<Line<'static>> {
    let s = app.strings();
    let state = &app.assistant;
    let mut lines = vec![
        Line::from(""),
        heading(
            app.lang.view_name(crate::event::View::Assistant),
            width,
            theme,
        ),
        Line::from(""),
        heading(s.asst_session_section, width, theme),
    ];

    if !state.configured {
        lines.push(note(&format!("  {}", s.asst_not_configured), width, theme));
    } else {
        let mode = state
            .mode
            .map(|m| mode_word(m, app).to_string())
            .unwrap_or_else(|| s.asst_mode_auto.to_string());
        lines.push(field_row(
            &format!("  {}{}", s.asst_mode_label, s.label_separator),
            &mode,
            width,
            theme,
        ));
        let usage = format_tokens(state.usage.0 + state.usage.1);
        lines.push(field_row(
            &format!("  {}{}", s.asst_usage_label, s.label_separator),
            &usage,
            width,
            theme,
        ));
        if let Some(goal) = &state.awaiting {
            lines.push(Line::from(Span::styled(
                text::clip_to(&format!("  ⚠ {goal} — {}", s.asst_awaiting), width as usize),
                theme.warn(),
            )));
        }
    }

    lines.push(Line::from(""));
    lines.push(heading(s.asst_tasks_section, width, theme));
    if state.tasks.is_empty() {
        lines.push(note(&format!("  {}", s.asst_no_tasks), width, theme));
    } else {
        for task in &state.tasks {
            lines.push(task_row(task, width, theme));
        }
    }

    lines.push(Line::from(""));
    lines.push(heading(s.asst_notes_section, width, theme));
    if state.notes.is_empty() {
        lines.push(note(&format!("  {}", s.asst_no_notes), width, theme));
    } else {
        for (text, evidence) in &state.notes {
            let row = if evidence.is_empty() {
                format!("  {text}")
            } else {
                format!("  {text} ({evidence})")
            };
            lines.push(Line::from(Span::styled(
                text::clip_to(&row, width as usize),
                theme.text(),
            )));
        }
    }

    lines
}

/// Thousands-separated token count, matching the agent usage formatter.
fn format_tokens(value: u64) -> String {
    crate::agent::usage::format_tokens(value)
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
    fn an_unconfigured_agent_says_so() {
        let theme = Theme::DARK;
        let a = app(Lang::En);
        let text = body_text(&lines(&a, &theme, 70));
        assert!(text.contains("Session"), "{text}");
        assert!(text.contains("No model endpoint configured"), "{text}");
        assert!(text.contains("Tasks"), "{text}");
        assert!(text.contains("No tasks recorded"), "{text}");
    }

    #[test]
    fn a_configured_agent_shows_mode_usage_and_history() {
        let theme = Theme::DARK;
        let mut a = app(Lang::En);
        a.assistant.configured = true;
        a.assistant.mode = Some(Mode::FullAuto);
        a.assistant.usage = (1200, 3400);
        a.assistant.tasks = vec![TaskRecord {
            id: "t1".into(),
            device_key: "k".into(),
            updated_at: 0,
            goal: "diagnose boot loop".into(),
            summary: String::new(),
            status: "done".into(),
        }];
        a.assistant.notes = vec![(
            "curl is missing; wget is present".to_string(),
            String::new(),
        )];
        let text = body_text(&lines(&a, &theme, 90));
        assert!(text.contains("Execution mode"), "{text}");
        assert!(text.contains("full auto"), "{text}");
        assert!(text.contains("4,600"), "{text}");
        assert!(text.contains("diagnose boot loop"), "{text}");
        assert!(text.contains("wget is present"), "{text}");
    }

    #[test]
    fn an_awaiting_approval_is_warned() {
        let theme = Theme::DARK;
        let mut a = app(Lang::En);
        a.assistant.configured = true;
        a.assistant.awaiting = Some("sudo reboot".into());
        let painted = lines(&a, &theme, 70);
        let flat: Vec<(String, Option<ratatui::style::Color>)> = painted
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| (s.content.to_string(), s.style.fg))
            .collect();
        let warn = flat
            .iter()
            .find(|(t, _)| t.contains("sudo reboot"))
            .unwrap();
        assert_eq!(warn.1, Some(theme.warn));
    }

    #[test]
    fn every_row_fits_the_pane() {
        let theme = Theme::DARK;
        for width in [10u16, 24, 40, 70, 100] {
            for lang in [Lang::Zh, Lang::En] {
                let mut a = app(lang);
                a.assistant.configured = true;
                a.assistant.mode = Some(Mode::Manual);
                a.assistant.tasks = vec![TaskRecord {
                    id: "t".into(),
                    device_key: "k".into(),
                    updated_at: 0,
                    goal: "一个很长很长很长很长的任务目标确实很长".into(),
                    summary: String::new(),
                    status: "completed".into(),
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
