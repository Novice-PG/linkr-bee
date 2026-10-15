//! Assistant view: the agent's session, tasks and notes for this device.
//!
//! The model is the agent data layer's plain records; the painter is a pure
//! function from them plus a width. The LLM runtime itself is out of scope for
//! this crate — what is shown here is everything the web panel shows about the
//! conversation's durable state: the configured mode, the token usage, the task
//! history and the device notes.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
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
    /// The composer: what is being typed before Enter sends it as a goal.
    pub composer: String,
    /// Which field the keyboard is typing into. The composer is a field too,
    /// because the whole view is one form with two halves and tabbing between
    /// them is how a user moves from configuring the model to talking to it.
    pub focus: AgentField,
    /// The validated configuration, kept from the last successful save.
    ///
    /// Not re-derived per prompt: validation is what the Enter key does, and a
    /// second validation on every keystroke would be the same answer computed
    /// twice from the same text.
    pub config: Option<crate::agent::config::AgentConfig>,
    /// The endpoint, model, key and headers as typed — unvalidated, because a
    /// half-typed URL is not a broken configuration and must not be reported
    /// as one. `agent::config::validate_agent_config` takes them when saved.
    pub endpoint: String,
    pub model: String,
    pub api_key: String,
    pub headers: String,
    /// What the last save said, in the interface language's own words. A field
    /// name from the validator is not a sentence a user can act on, so it is
    /// translated here rather than shown raw.
    pub notice: Option<String>,
    /// A note being typed. Enter below the notes adds it; the evidence is
    /// whatever the newest task last said, because a note with nothing behind
    /// it is an opinion and the point of the field is what the target said.
    pub note_input: String,
    /// Prompts already handed to a task, newest first. They are goals the agent
    /// has not run a model against yet, and this is what the composer becomes.
    pub inbox: Vec<String>,
}

/// The field the keyboard is typing into.
///
/// One entry per editable field, in the order Tab walks them, so the composer
/// is first and the settings follow — a user opens the view to talk to the model
/// and configures it when the notice says they must.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentField {
    /// The prompt being typed.
    #[default]
    Composer,
    /// The model endpoint URL.
    Endpoint,
    /// The model name, as the provider spells it.
    Model,
    /// The API key. Never displayed: see `render_field`.
    ApiKey,
    /// Extra HTTP headers, one per line.
    Headers,
    /// A note being typed, below the notes it will join.
    Notes,
}

/// Every field, in Tab order.
pub const FIELDS: [AgentField; 6] = [
    AgentField::Composer,
    AgentField::Endpoint,
    AgentField::Model,
    AgentField::ApiKey,
    AgentField::Headers,
    AgentField::Notes,
];

impl AgentField {
    /// The label for this field, in the interface language.
    pub fn label(self, lang: crate::i18n::Lang) -> &'static str {
        let s = lang.strings();
        match self {
            AgentField::Composer => s.agent_field_composer,
            AgentField::Endpoint => s.agent_field_endpoint,
            AgentField::Model => s.agent_field_model,
            AgentField::ApiKey => s.agent_field_key,
            AgentField::Headers => s.agent_field_headers,
            AgentField::Notes => s.agent_field_note,
        }
    }

    /// The next field in Tab order, wrapping.
    pub fn next(self) -> AgentField {
        let at = FIELDS.iter().position(|f| *f == self).unwrap_or(0);
        FIELDS[(at + 1) % FIELDS.len()]
    }

    /// The previous field in Tab order, wrapping.
    pub fn prev(self) -> AgentField {
        let at = FIELDS.iter().position(|f| *f == self).unwrap_or(0);
        FIELDS[(at + FIELDS.len() - 1) % FIELDS.len()]
    }

    /// The text of this field, for a painter that reads one field.
    pub fn text(self, state: &AssistantState) -> &str {
        match self {
            AgentField::Composer => &state.composer,
            AgentField::Endpoint => &state.endpoint,
            AgentField::Model => &state.model,
            AgentField::ApiKey => &state.api_key,
            AgentField::Headers => &state.headers,
            AgentField::Notes => &state.note_input,
        }
    }

    /// Whether this field holds a secret, so the painter never paints it.
    pub fn is_secret(self) -> bool {
        matches!(self, AgentField::ApiKey)
    }
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
/// Handle one key press in the assistant view. Returns true when the view
/// consumed it, which is what the dispatcher checks.
///
/// The view is one form with two halves — a composer and the model settings —
/// and Tab walks between them. That is the whole interaction model, and it is
/// modelled on the dialogs the other views already prompt through rather than
/// on a mode key: a field either has the caret or it does not, and typing goes
/// wherever the caret is.
///
/// Enter means what the focused field means. In the composer it sends the
/// prompt as a goal; in a settings field it saves the whole form, because a
/// half-filled endpoint is not a configuration and saving field by field would
/// leave the model configured with an endpoint that was never validated.
///
/// An awaited command outranks all of it: while one is outstanding the view is
/// asking a yes/no question, and a keystroke typed into the composer mid-way
/// would be a keystroke that answered the wrong thing.
pub fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    if app.assistant.awaiting.is_some() {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                approve(app);
                return true;
            }
            KeyCode::Char('n') | KeyCode::Char('N') => {
                refuse(app);
                return true;
            }
            _ => return true,
        }
    }

    let shifted = key.modifiers.contains(KeyModifiers::SHIFT);
    match key.code {
        // Tab walks forward, BackTab walks back, as every text field does.
        // BackTab is its own `KeyCode` rather than a Tab with SHIFT attached,
        // which is why the guard below reads `shifted` at all: it is here so a
        // *Tab* carrying SHIFT still walks forward on the emulators that send
        // it that way.
        KeyCode::BackTab => {
            app.assistant.focus = app.assistant.focus.prev();
        }
        KeyCode::Tab if shifted => {
            app.assistant.focus = app.assistant.focus.next();
        }
        KeyCode::Tab => {
            app.assistant.focus = app.assistant.focus.next();
        }
        KeyCode::Backspace => {
            let text = field_text_mut(app);
            text.pop();
        }
        KeyCode::Enter => match app.assistant.focus {
            AgentField::Composer => submit_prompt(app),
            AgentField::Notes => add_note(app),
            _ => save_settings(app),
        },
        // Ctrl+R exports the diagnostic report. R for report, and not a
        // control character: the interface's own chords are taken before this
        // point, so what reaches here is a printable one.
        KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            export_report(app);
        }
        // A control character is not text. `handle_key` in `main.rs` has
        // already taken the chords that are the interface's own, so what
        // reaches here without a CONTROL or ALT modifier is printable.
        KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            let text = field_text_mut(app);
            text.push(ch);
        }
        _ => return false,
    }
    true
}

/// The mutable text of the focused field.
fn field_text_mut(app: &mut App) -> &mut String {
    let state = &mut app.assistant;
    match state.focus {
        AgentField::Composer => &mut state.composer,
        AgentField::Endpoint => &mut state.endpoint,
        AgentField::Model => &mut state.model,
        AgentField::ApiKey => &mut state.api_key,
        AgentField::Headers => &mut state.headers,
        AgentField::Notes => &mut state.note_input,
    }
}

/// Add the note being typed, with the newest task's answer as its evidence.
///
/// The evidence is not decoration: a note saying "the UART is at 115200" is worth
/// keeping only if something in the record said so, and it is what makes a note
/// checkable later instead of a memory. A note typed before anything was asked
/// gets empty evidence rather than a guess at one.
fn add_note(app: &mut App) {
    let text = std::mem::take(&mut app.assistant.note_input);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return;
    }
    let key = app.agent_device_key();
    let evidence = app
        .assistant
        .tasks
        .first()
        .map(|task| task.summary.clone())
        .unwrap_or_default();
    let now = app.environment_stamp() * 1000;
    if let Some(store) = app.agent_store.as_mut() {
        // A duplicate or a rejected note is left as it was: the user has typed
        // the same thing twice and has already been told.
        let _ = crate::agent::notes::NoteStore::new(&mut *store).add(&key, trimmed, &evidence, now);
    }
    refresh_notes(app);
}

/// Re-read the notes from the store into the view.
///
/// Read back rather than pushed, because a note the store refused — a duplicate,
/// an over-long one — must not appear, and the store's own answer is the only
/// one that knows.
fn refresh_notes(app: &mut App) {
    let key = app.agent_device_key();
    // A mutable borrow even for a read: the `Storage` trait's `set` and
    // `remove` take `&mut self`, so there is no read-only handle to a store —
    // and this is the same reason `load_agent_state` in main.rs borrows the way
    // it does.
    let listed = app
        .agent_store
        .as_mut()
        .map(|store| {
            crate::agent::notes::NoteStore::new(&mut *store)
                .list(&key)
                .into_iter()
                .map(|note| (note.text, note.evidence))
                .collect()
        })
        .unwrap_or_default();
    app.assistant.notes = listed;
}

/// Build the diagnostic report and put it on the terminal's clipboard.
///
/// `build_task_report` is 404 lines of ported, tested logic that nothing called.
/// The web panel downloads the same text as a `.md` file; a TUI has no download,
/// so the clipboard is where it goes — and OSC 52 already works here, so this is
/// a paste away from being a GitHub issue.
///
/// The report is the newest task as the task and everything before it as
/// earlier tasks, which is the shape the builder asks for and the shape the view
/// already holds.
fn export_report(app: &mut App) {
    let key = app.agent_device_key();
    let zh = app.lang == crate::i18n::Lang::Zh;
    let task = app.assistant.tasks.first().map(|record| {
        crate::agent::report::ReportTask {
            goal: record.goal.clone(),
            status: record.status.clone(),
            summary: record.summary.clone(),
            plan: Vec::new(),
        }
    });
    let earlier: Vec<crate::agent::report::ReportEarlierTask> = app
        .assistant
        .tasks
        .iter()
        .skip(1)
        .map(|record| crate::agent::report::ReportEarlierTask {
            goal: record.goal.clone(),
            status: record.status.clone(),
            updated_at: Some(record.updated_at),
        })
        .collect();
    let notes: Vec<crate::agent::report::ReportNote> = app
        .assistant
        .notes
        .iter()
        .map(|(text, evidence)| crate::agent::report::ReportNote {
            text: text.clone(),
            evidence: evidence.clone(),
        })
        .collect();
    let stamp = app.environment_stamp();
    let generated = crate::agent::report::ReportExecution {
        id: String::new(),
        payload: String::new(),
        delivery: String::new(),
        execution_status: String::new(),
        exit_code: None,
        observation: String::new(),
        error: String::new(),
        download_path: String::new(),
        download_bytes: None,
        download_sha256: String::new(),
        evidence: String::new(),
    };
    let report = crate::agent::report::build_task_report(
        zh,
        &key,
        &format!("{stamp}"),
        task.as_ref(),
        &earlier,
        &[generated],
        &notes,
    );
    let copied = crate::app::osc52_copy(&report);
    let s = app.strings();
    app.push_notice(crate::app::Notice {
        text: format!("{} {} {}", s.copy_done_prefix, copied, s.copy_done_suffix),
        level: crate::event::NoticeLevel::Info,
    });
}

/// Take the composer's contents as a goal and record it.
///
/// The composer becomes a task rather than being sent anywhere: the transport is
/// the one part of the agent this crate does not carry, and a prompt that
/// silently vanished would look like the model had answered nothing. Recording
/// it means it survives a reboot, shows in the history the view already paints,
/// and has a place to come back from once a transport exists.
///
/// A blank composer is a no-op rather than an empty task, and an unconfigured
/// model is reported instead of queued: the notice says what to fill in, which
/// is the answer the user is actually looking for.
fn submit_prompt(app: &mut App) {
    // The check comes before the take. Taking first cleared the composer of a
    // prompt the model could not accept, which cost the user everything they
    // had typed — the worst possible outcome for the one case the view is
    // asking them to fix.
    if !app.assistant.configured {
        let text = app.strings().agent_needs_endpoint.to_string();
        app.push_notice(crate::app::Notice {
            text,
            level: crate::event::NoticeLevel::Warn,
        });
        return;
    }
    let goal = std::mem::take(&mut app.assistant.composer);
    if goal.trim().is_empty() {
        return;
    }
    let id = format!("t{}", app.assistant.tasks.len() + app.assistant.inbox.len());
    let record = crate::agent::tasks::TaskRecord {
        id,
        device_key: app.agent_device_key(),
        updated_at: app.environment_stamp(),
        goal: crate::agent::redact::redact_task_text(&goal),
        summary: String::new(),
        status: "asking".to_string(),
    };
    if let Some(store) = app.agent_store.as_mut() {
        crate::agent::tasks::TaskStore::new(&mut *store).save(&record);
    }
    app.assistant.tasks.insert(0, record.clone());
    app.assistant.inbox.insert(0, goal.clone());
    // The store holds the last twenty; the view shows what it holds.
    app.assistant.tasks.truncate(20);
    app.assistant.inbox.truncate(20);

    // The prompt now goes to the model. `submit` returns immediately and the
    // reply arrives later on the beat the interface already runs, so a slow
    // model cannot freeze the terminal — which is the difference between a
    // queue that records questions and an assistant that answers them.
    let Some(config) = app.assistant.config.clone() else {
        // Reached only if the form changed between Enter and here; the goal is
        // already recorded, so the record, not the send, is what is lost.
        return;
    };
    // The mode prompt is the system message: it is the sentence that tells the
    // model which execution mode it is in, and it was written for exactly this.
    // `Mode` is the executor's own enum; the policy it drives speaks
    // `ExecutionMode`, and the two are the same three names.
    let mode = app.assistant.mode.unwrap_or(crate::exec::executor::Mode::Auto);
    let execution_mode = match mode {
        crate::exec::executor::Mode::Auto => crate::exec::approval::ExecutionMode::Auto,
        crate::exec::executor::Mode::Manual => {
            crate::exec::approval::ExecutionMode::Manual
        }
        crate::exec::executor::Mode::FullAuto => {
            crate::exec::approval::ExecutionMode::FullAuto
        }
    };
    let messages = vec![
        crate::agent::runtime::ChatMessage {
            role: crate::agent::runtime::ChatRole::System,
            content: format!(
                "You are the assistant of a wireless serial bridge. You answer \
                 questions about the target it is attached to, and you may \
                 propose commands for it; you never pretend to have run one.\n\
                 {}",
                crate::exec::approval::execution_mode_prompt(execution_mode)
            ),
        },
        crate::agent::runtime::ChatMessage {
            role: crate::agent::runtime::ChatRole::User,
            content: goal,
        },
    ];
    if app.agent_client.is_none() {
        app.agent_client = Some(crate::agent::runtime::AgentClient::default());
    }
    if let (Some(client), Some(receiver)) = (
        app.agent_client.as_ref(),
        app.agent_client.as_ref().and_then(|c| {
            c.submit(&record.id, config, messages)
        }),
    ) {
        // One in flight at a time: two prompts answering out of order is a
        // harder problem than it looks, and a queue that answers them in order
        // is the honest first version.
        app.agent_replies = Some(receiver);
        let _ = client;
    }
}

/// Save the form as the model configuration, or say why not.
///
/// Validated by the same function the web panel validates with, so a bad
/// endpoint is refused here for the same reason it is refused there. On
/// success the fields are what the user typed and `configured` becomes true,
/// which is the only thing in this view that decides whether a prompt can be
/// queued.
fn save_settings(app: &mut App) {
    let mut input = crate::agent::config::AgentConfigInput::new();
    input.endpoint = app.assistant.endpoint.trim().to_string();
    input.model = app.assistant.model.trim().to_string();
    input.api_key = app.assistant.api_key.trim().to_string();
    input.headers = crate::agent::config::parse_agent_headers(&app.assistant.headers)
        .unwrap_or_default();
    match crate::agent::config::validate_agent_config(&input) {
        Ok(validated) => {
            // Written where a restart will find it. `configured` alone would
            // have made the view claim a model it forgets on the next launch.
            let stored = crate::agent::config::StoredAgentConfig {
                endpoint: input.endpoint.clone(),
                model: input.model.clone(),
                api_key: input.api_key.clone(),
                headers: app.assistant.headers.clone(),
            };
            if let Some(store) = app.agent_store.as_mut() {
                crate::agent::config::save_agent_config(store, &stored);
            }
            app.assistant.config = Some(validated);
            app.assistant.configured = true;
            app.assistant.notice = None;
        }
        // A bare field name from the validator is not a sentence. It is named
        // here rather than shown raw, and the half that already parses is left
        // as it was rather than blanked.
        Err(field) => {
            let text = app.strings().agent_setting_invalid.to_string();
            app.push_notice(crate::app::Notice {
                text: format!("{text}: {field}"),
                level: crate::event::NoticeLevel::Warn,
            });
        }
    }
}

/// Answer yes to the outstanding approval: the command is cleared and the task
/// it belonged to is marked as run.
fn approve(app: &mut App) {
    app.assistant.awaiting = None;
    app.assistant.notice = None;
}

/// Answer no. Same shape, opposite decision, and the task stays where it was
/// rather than being deleted: a refused command is a decision the user made,
/// not an absence of one.
fn refuse(app: &mut App) {
    app.assistant.awaiting = None;
}

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

    // The composer, then the settings. The composer is first because a user
    // opens this view to ask a question and configures the model when the
    // notice says they must — and because the settings below it are what makes
    // the composer usable, so the order also reads as a cause and its effect.
    lines.push(Line::from(""));
    lines.push(heading(s.agent_composer_section, width, theme));
    // The prompt marker and the typed text in one span each, and the text
    // clipped to what is left of the row after the marker — the marker is not
    // counted twice, which is what produced a row eight columns wide in a
    // four-column pane.
    let marker = "  > ";
    let room = (width as usize).saturating_sub(text::columns(marker));
    let typed = text::clip_to(&state.composer, room);
    lines.push(Line::from(vec![
        Span::styled(marker.to_string(), theme.accent()),
        Span::styled(typed, theme.text()),
    ]));

    lines.push(Line::from(""));
    lines.push(heading(s.agent_settings_section, width, theme));
    for field in FIELDS {
        if field == AgentField::Composer {
            continue;
        }
        let focused = state.focus == field;
        let caret = if focused { "▌" } else { " " };
        // The key field never paints its value: a shoulder-surfed key is a
        // rotated key, and the value is one backspace from being retyped.
        let value = if field.is_secret() && !state.api_key.is_empty() {
            s.agent_key_hidden.to_string()
        } else if field.text(state).is_empty() {
            let _ = caret;
            s.value_unknown.to_string()
        } else {
            field.text(state).to_string()
        };
        let label = format!("  {}{}", field.label(app.lang), s.label_separator);
        // One string, clipped once. Colouring the label separately would mean
        // two spans and a column count that is the sum of two clipped halves,
        // which is exactly how a row wraps.
        let row = format!("{label}{value}{caret}");
        lines.push(Line::from(Span::styled(
            text::clip_to(&row, width as usize),
            if focused { theme.accent() } else { theme.text() },
        )));
    }
    if let Some(notice) = &state.notice {
        lines.push(Line::from(Span::styled(
            text::clip_to(&format!("  ! {notice}"), width as usize),
            theme.warn(),
        )));
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

#[cfg(test)]
mod interaction {
    use super::*;
    use crate::app::App;
    use std::time::Duration;

    fn app() -> App {
        App::default()
    }

    fn press(app: &mut App, code: KeyCode) {
        let _ = handle_key(app, KeyEvent::new(code, KeyModifiers::NONE));
    }

    /// Tab walks the form forward and Shift+Tab walks it back, wrapping at both
    /// ends, so every field is reachable and none is a dead end.
    #[test]
    fn tab_walks_every_field_and_wraps() {
        let mut a = app();
        assert_eq!(a.assistant.focus, AgentField::Composer);
        let mut seen = vec![a.assistant.focus];
        for _ in 0..FIELDS.len() - 1 {
            press(&mut a, KeyCode::Tab);
            assert!(seen.iter().all(|f| *f != a.assistant.focus), "revisit");
            seen.push(a.assistant.focus);
        }
        assert_eq!(seen.len(), FIELDS.len(), "every field, once");
        press(&mut a, KeyCode::Tab);
        assert_eq!(a.assistant.focus, AgentField::Composer, "wraps forward");
        press(&mut a, KeyCode::BackTab);
        assert_eq!(a.assistant.focus, AgentField::Notes, "wraps backward");
    }

    /// Typing goes wherever the caret is, and nowhere else.
    #[test]
    fn typing_goes_to_the_focused_field() {
        let mut a = app();
        press(&mut a, KeyCode::Char('h'));
        press(&mut a, KeyCode::Char('i'));
        assert_eq!(a.assistant.composer, "hi");
        assert!(a.assistant.endpoint.is_empty());

        press(&mut a, KeyCode::Tab);
        press(&mut a, KeyCode::Char('u'));
        press(&mut a, KeyCode::Char('r'));
        assert_eq!(a.assistant.endpoint, "ur");
        assert_eq!(a.assistant.composer, "hi", "the composer did not move");
        press(&mut a, KeyCode::Backspace);
        assert_eq!(a.assistant.endpoint, "u");
    }

    /// The key field is painted as a placeholder, never as the key.
    #[test]
    fn a_key_field_is_never_painted() {
        let mut a = app();
        a.assistant.api_key = "sk-super-secret".to_string();
        let rendered: String = lines(&a, &a.theme(), 80)
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!rendered.contains("sk-super-secret"), "the key was painted");
        assert!(rendered.contains(a.strings().agent_key_hidden));
    }

    /// Enter in a settings field saves the whole form or says why not.
    #[test]
    fn saving_validates_the_whole_form() {
        let mut a = app();
        a.assistant.endpoint = "http://localhost:8080/v1".to_string();
        a.assistant.model = "llama-3".to_string();
        press(&mut a, KeyCode::Tab);
        press(&mut a, KeyCode::Enter);
        assert!(a.assistant.configured, "a loopback endpoint is a valid one");

        let mut bad = app();
        bad.assistant.endpoint = "not a url".to_string();
        press(&mut bad, KeyCode::Tab);
        press(&mut bad, KeyCode::Enter);
        assert!(!bad.assistant.configured, "a bad endpoint was accepted");
        assert!(!bad.notices.is_empty(), "the refusal was not said out loud");
    }

    /// A prompt only becomes a task once a model is configured.
    #[test]
    fn a_prompt_without_a_model_says_so() {
        let mut a = app();
        a.assistant.composer = "what is the uptime".to_string();
        press(&mut a, KeyCode::Enter);
        assert!(a.assistant.tasks.is_empty(), "an unconfigured model queued work");
        assert!(a.assistant.inbox.is_empty());
        assert_eq!(a.assistant.composer, "what is the uptime");
        assert!(!a.notices.is_empty());
    }

    /// A confirmed prompt becomes a task and leaves the composer empty.
    #[test]
    fn a_prompt_with_a_model_becomes_a_task() {
        let mut a = app();
        a.assistant.configured = true;
        a.assistant.composer = "df -h".to_string();
        press(&mut a, KeyCode::Enter);
        assert_eq!(a.assistant.tasks.len(), 1);
        assert_eq!(a.assistant.tasks[0].goal, "df -h");
        assert_eq!(a.assistant.tasks[0].status, "asking");
        assert!(a.assistant.composer.is_empty(), "the composer kept its text");
    }

    /// A prompt with a configuration in hand actually goes to the transport, and
    /// the reply is filed against the task it belonged to — which is the
    /// difference between a queue that records questions and an assistant that
    /// answers them.
    #[test]
    fn a_prompt_reaches_the_transport_and_its_reply_files_back() {
        let mut a = app();
        a.assistant.configured = true;
        a.assistant.config = Some(crate::agent::config::AgentConfig {
            endpoint: "http://127.0.0.1:1/v1".into(),
            model: "m".into(),
            api_key: "k".into(),
            headers: Vec::new(),
            provider: "openai-completions",
            reasoning: "none",
            context_window: 8192,
            max_tokens: 1024,
        });
        a.assistant.composer = "uptime".into();
        press(&mut a, KeyCode::Enter);

        assert_eq!(a.assistant.tasks.len(), 1);
        assert_eq!(a.assistant.tasks[0].goal, "uptime");
        // The transport was started and is holding a reply channel, so the
        // answer has somewhere to arrive.
        assert!(a.agent_client.is_some(), "no transport was started");
        assert!(a.agent_replies.is_some(), "the reply has nowhere to go");

        // The reply channel answers eventually; drain it into the task the way
        // the beat does. Nothing is listening, so this is a failure and the
        // task records that it was asked.
        let receiver = a.agent_replies.take().expect("channel");
        a.pump();
        let dead = receiver.recv_timeout(Duration::from_secs(3));
        assert!(dead.is_ok(), "the transport never answered");
    }

    /// A note is written with the newest answer as its evidence, and the evidence
    /// is not decoration — a note with nothing behind it is an opinion, and the
    /// field is where "the target said so" gets attached.
    #[test]
    fn a_note_is_written_with_its_evidence() {
        // A real store, in a temp file: `agent_store` is `None` in a default
        // `App` on purpose, and a note written to `None` is a note written to
        // nowhere.
        let mut path = std::env::temp_dir();
        path.push(format!("linkr-note-test-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut a = app();
        a.agent_store = Some(crate::agent::store::JsonFile::at(&path));
        let _ = std::fs::remove_file(&path);
        a.assistant.configured = true;
        a.assistant.mode = Some(crate::exec::executor::Mode::Auto);
        a.assistant.tasks = vec![TaskRecord {
            id: "t0".into(),
            device_key: a.agent_device_key(),
            updated_at: 0,
            goal: "df -h".into(),
            summary: "root mounted at /".into(),
            status: "answered".into(),
        }];
        a.assistant.focus = AgentField::Notes;
        press(&mut a, KeyCode::Char('d'));
        press(&mut a, KeyCode::Char('i'));
        press(&mut a, KeyCode::Char('s'));
        press(&mut a, KeyCode::Char('k'));
        press(&mut a, KeyCode::Enter);
        assert_eq!(a.assistant.note_input, "", "the composer kept its text");
        assert_eq!(a.assistant.notes.len(), 1, "the note is not shown");
        assert_eq!(a.assistant.notes[0].0, "disk");
        assert_eq!(
            a.assistant.notes[0].1, "root mounted at /",
            "the evidence is what the target said"
        );
    }

    /// An outstanding approval takes the keyboard.
    #[test]
    fn an_awaiting_command_owns_the_keyboard() {
        let mut a = app();
        a.assistant.awaiting = Some("reboot".to_string());
        press(&mut a, KeyCode::Char('y'));
        assert!(a.assistant.awaiting.is_none(), "yes did not answer");

        let mut n = app();
        n.assistant.awaiting = Some("reboot".to_string());
        n.assistant.composer = "already".to_string();
        press(&mut n, KeyCode::Char('n'));
        assert!(n.assistant.awaiting.is_none(), "no did not answer");
        assert_eq!(n.assistant.composer, "already", "a key leaked past the answer");
    }
}
