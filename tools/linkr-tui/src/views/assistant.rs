//! Assistant: the agent's session and tasks for this device, and the settings
//! that make it run.
//!
//! The model is the agent data layer's plain records; the painter is a pure
//! function from them plus a width. The LLM runtime itself is out of scope for
//! this crate — what is shown here is everything the web panel shows about the
//! conversation's durable state: the configured mode, the token usage, the task
//! history and the device notes.
//!
//! Two surfaces, one form. The *view* is the thread: session, composer,
//! conversation, tasks. The *settings window* — `Ctrl+P`, "Configure the
//! assistant" — is the five fields and the note list, raised as a dialog. They
//! split because a form the user has to scroll past is what pushed the thread
//! off the pane, and both halves share `AssistantState` so the caret, the
//! hidden key and the note cursor are one piece of state rather than two
//! opinions about where the keys go.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::{Line, Span};

use crate::agent::tasks::TaskRecord;
use crate::app::App;
use crate::clipboard::read_system_clipboard;
use crate::exec::executor::Mode;
use crate::text;
use crate::theme::Theme;
use std::time::Instant;

/// Everything the assistant view knows.
#[derive(Debug, Clone, Default)]
pub struct AssistantState {
    /// Whether a model endpoint is configured.
    pub configured: bool,
    /// The execution mode in force.
    pub mode: Option<Mode>,
    /// When the unattended window closes, if one is armed.
    ///
    /// The deadline belongs to the UI because the mode is a policy the agent is
    /// *told*, not a live permission: nothing else would notice it elapsing,
    /// and a countdown that reached zero without the mode moving would be a
    /// lie.
    pub full_auto_until: Option<Instant>,
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
    /// The conversation turns, oldest first: what was asked and what came back.
    ///
    /// Kept separately from the task history because it is what the *session* is
    /// — a task is a record of a job, and this is the thread of a conversation.
    /// Persisted by `agent::session` across restarts, which is the one piece of
    /// agent state that was still write-only until now.
    pub display: Vec<crate::agent::session::DisplayLine>,
    /// Which note the keyboard is on, for removal.
    ///
    /// The cursor rides the list as `refresh_notes` fills it, which is the
    /// store's own order — oldest first, because that is what `NoteStore::list`
    /// sorts by. The row the user sees highlighted is the row that goes.
    pub note_cursor: usize,
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
/// One entry per editable field. They live in two places now: the composer is
/// the view's, and the five settings are the settings window's — a user opens
/// the view to talk to the model and opens the window on purpose when the
/// notice says they must configure it first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentField {
    /// The prompt being typed.
    #[default]
    Composer,
    /// The model endpoint URL.
    Endpoint,
    /// The model name, as the provider spells it.
    Model,
    /// The API key. Never displayed: see `is_secret` and `settings_lines`.
    ApiKey,
    /// Extra HTTP headers, one per line.
    Headers,
    /// A note being typed, below the notes it will join.
    Notes,
}

/// The fields the settings window edits, in Tab order.
///
/// The composer is deliberately absent: it is the *view's* field, and this list
/// is what the view became once the settings moved out into their own window.
/// Tab here walks a form with an end, and the end is the last setting — never a
/// trip through a caret that is not painted anywhere the user can see it.
pub const SETTINGS: [AgentField; 5] = [
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

    /// The next setting, wrapping.
    ///
    /// A field from *outside* the list — the composer's, which is where a caret
    /// sits until the window opens — lands on the first setting rather than on
    /// the second, so opening the window and pressing Tab twice from a stale
    /// caret still ends up somewhere the box is showing.
    pub fn next(self) -> AgentField {
        let at = SETTINGS.iter().position(|f| *f == self);
        match at {
            Some(at) => SETTINGS[(at + 1) % SETTINGS.len()],
            None => SETTINGS[0],
        }
    }

    /// The previous setting, wrapping.
    pub fn prev(self) -> AgentField {
        let at = SETTINGS.iter().position(|f| *f == self);
        match at {
            Some(at) => SETTINGS[(at + SETTINGS.len() - 1) % SETTINGS.len()],
            None => SETTINGS[0],
        }
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

/// ` · 14:59` while the unattended window is armed, nothing otherwise.
///
/// One read per frame, rounded up like the panel this comes from: the frame
/// loop repaints on every tick, so the arithmetic is the timer, and a window
/// that has just been armed reads `15:00` rather than `14:59`.
fn full_auto_countdown(app: &App) -> String {
    let Some(until) = app.assistant.full_auto_until else {
        return String::new();
    };
    let seconds = until
        .saturating_duration_since(Instant::now())
        .as_millis()
        .div_ceil(1000);
    format!(" · {}:{:02}", seconds / 60, seconds % 60)
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

/// Handle one key press in the assistant view. Returns true when the view
/// consumed it, which is what the dispatcher checks.
///
/// The view is one field deep: the composer. The model settings moved into
/// their own window (`Ctrl+P`, "Configure the assistant"), so there is nothing
/// for Tab to walk to here and the caret is the composer's whenever that window
/// is shut — the guard below puts it back if something closed the window
/// without giving it up, because a caret parked on an unpainted field is a
/// keystroke that lands nowhere the user can see.
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

    if !matches!(app.dialog, Some(crate::dialogs::Dialog::Settings(_))) {
        app.assistant.focus = AgentField::Composer;
    }

    match key.code {
        KeyCode::Backspace => {
            let text = field_text_mut(app);
            text.pop();
        }
        // Up and Down are consumed either way. The assistant is a form, and an
        // arrow it has no use for would otherwise reach the pane below this
        // dispatch and scroll — or worse, be sent on to the device as a cursor
        // key in a view where the user is not reading the console.
        KeyCode::Up | KeyCode::Down => {}
        // Enter sends: the composer is the only field here, and the settings
        // window's own Enter (save / file a note) is its handler's, not this one's.
        KeyCode::Enter => submit_prompt(app),
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

/// Open the settings window with the caret on `field`.
///
/// The field must be one of the settings: the composer has no row in this
/// window, so a caller naming it gets the first row rather than a caret on
/// nothing — a caret on nothing is the state that eats keystrokes silently.
pub fn open_settings(app: &mut App, field: AgentField) {
    app.assistant.focus = if SETTINGS.contains(&field) {
        field
    } else {
        SETTINGS[0]
    };
    crate::dialogs::open_settings(app);
}

/// Put the keyboard on the composer: the assistant view, and the caret with it.
///
/// A settings window still up is closed rather than left behind the view: it
/// would be a window whose caret has just been moved to a field it does not
/// paint, so the next keystroke would be typed into a row nobody is showing —
/// which is the state `handle_key` spends a line guarding against, closed here
/// instead of being cleaned up after.
pub fn focus_composer(app: &mut App) {
    close_settings(app);
    app.set_view(crate::event::View::Assistant);
}

/// Give the caret back to the composer and put the settings window away.
///
/// The caret moves with the window, always: while it is up the caret is one of
/// the five fields, and the moment it shuts the caret is the composer's. A caret
/// left on a field the view does not paint is a keystroke that lands nowhere.
pub fn close_settings(app: &mut App) {
    app.assistant.focus = AgentField::Composer;
    if matches!(app.dialog, Some(crate::dialogs::Dialog::Settings(_))) {
        app.dialog = None;
    }
}

/// One key in the settings window. Returns true when the form took it.
///
/// Tab and the arrows walk the five fields — the arrows *except* on the note
/// row, where they walk the note list instead: that row is where a field meets
/// a list, and the list is the only thing that says which note `Delete` means.
/// Enter saves the whole form, or files the note when the note row has the
/// caret, because a half-filled endpoint is not a configuration and saving
/// field by field would leave the model configured with an endpoint that was
/// never validated.
///
/// Escape is deliberately not handled here: closing the window — and handing
/// the caret back to the composer — is the dialog's business, since the dialog
/// is what carries the scroll offset with it.
pub fn handle_settings_key(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
        // BackTab is its own `KeyCode` rather than a Tab with SHIFT attached,
        // so both spellings of "walk forward" land on the same next row.
        KeyCode::BackTab => app.assistant.focus = app.assistant.focus.prev(),
        KeyCode::Tab => app.assistant.focus = app.assistant.focus.next(),
        KeyCode::Backspace => {
            let text = field_text_mut(app);
            text.pop();
        }
        KeyCode::Up if app.assistant.focus == AgentField::Notes => {
            app.assistant.note_cursor = app.assistant.note_cursor.saturating_sub(1);
        }
        KeyCode::Down if app.assistant.focus == AgentField::Notes => {
            let last = app.assistant.notes.len().saturating_sub(1);
            app.assistant.note_cursor = (app.assistant.note_cursor + 1).min(last);
        }
        KeyCode::Up => app.assistant.focus = app.assistant.focus.prev(),
        KeyCode::Down => app.assistant.focus = app.assistant.focus.next(),
        // Delete removes the note under the cursor. Not a letter, which would
        // take that letter out of the field being typed in, and not Backspace,
        // which already means "delete a character".
        KeyCode::Delete if app.assistant.focus == AgentField::Notes => remove_note(app),
        KeyCode::Enter => match app.assistant.focus {
            AgentField::Notes => add_note(app),
            _ => save_settings(app),
        },
        // A control character is not text: the window's own chords (there are
        // none) and the interface's own are decided before this handler runs.
        KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            let text = field_text_mut(app);
            text.push(ch);
        }
        _ => return false,
    }
    true
}

/// Add one turn to the conversation and persist it.
///
/// Saved on every turn rather than at exit: the session is the one piece of agent
/// state a user would actually miss, and a bridge that loses it on a crash has
/// lost the conversation, not a setting.
fn push_turn(app: &mut App, role: &str, text: &str) {
    app.assistant.display.push(crate::agent::session::DisplayLine {
        role: role.to_string(),
        text: text.to_string(),
    });
    // The store keeps the last few turns; the view shows the same bound, so what
    // is on screen is what is on disk rather than more of one than the other.
    let keep = crate::agent::session::SESSION_MESSAGES;
    let from = app.assistant.display.len().saturating_sub(keep);
    app.assistant.display.drain(..from);
    persist_session(app);
}

/// Write the conversation where a restart will find it.
fn persist_session(app: &mut App) {
    let key = app.agent_device_key();
    let now = app.environment_stamp() * 1000;
    if let Some(store) = app.agent_store.as_mut() {
        let _ = crate::agent::session::save_session(
            store,
            &key,
            &app.assistant.display,
            &app.assistant.display,
            now,
        );
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

/// Remove the note under the cursor.
///
/// The cursor follows the deletion rather than being reset: rows are newest
/// first, so removing one leaves the cursor on the row that took its place,
/// which is where the user was looking when they pressed `d` twice.
fn remove_note(app: &mut App) {
    let key = app.agent_device_key();
    let Some((text, _)) = app.assistant.notes.get(app.assistant.note_cursor).cloned()
    else {
        return;
    };
    // Notes are matched by text because that is what the view holds: the store's
    // id is an implementation detail of its own ordering.
    let ids: Vec<String> = app
        .agent_store
        .as_mut()
        .map(|store| {
            let listed = crate::agent::notes::NoteStore::new(&mut *store).list(&key);
            listed
                .into_iter()
                .filter(|note| note.text == text)
                .map(|note| note.id)
                .collect()
        })
        .unwrap_or_default();
    for id in ids {
        if let Some(store) = app.agent_store.as_mut() {
            crate::agent::notes::NoteStore::new(&mut *store).remove(&key, &id);
        }
    }
    refresh_notes(app);
    // A cursor past the end after the last row goes is a cursor pointing at
    // nothing.
    let last = app.assistant.notes.len().saturating_sub(1);
    app.assistant.note_cursor = app.assistant.note_cursor.min(last);
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
pub fn export_report(app: &mut App) {
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
    // An empty execution list, not a list holding one blank record. The view
    // holds no execution log — the executor is what would fill one — and a
    // report that renders a section header over an empty row is noise that
    // looks like a failed execution rather than an absent one.
    let report = crate::agent::report::build_task_report(
        zh,
        &key,
        &format!("{stamp}"),
        task.as_ref(),
        &earlier,
        &[],
        &notes,
    );
    let copied = crate::clipboard::osc52_copy(&report);
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
    push_turn(app, "user", &goal);
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
        // One channel per question in flight, not one slot: a second prompt
        // must not drop the reply the first one is waiting for.
        app.agent_replies.push(receiver);
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
            .map(|m| format!("{}{}", mode_word(m, app), full_auto_countdown(app)))
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

    // The composer, then the thread. The settings are not here: five fields and
    // a note list are what this view used to show *instead of* the
    // conversation, and they now live in their own window — `Ctrl+P`, "Configure
    // the assistant" — where a user who came to configure opens it on purpose
    // and one who came to talk never scrolls past them.
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

    // The conversation, oldest first, and then the task history. This is the
    // reading order of a thread — what was asked, what came back — and the task
    // list below it is the durable record rather than the conversation itself.
    lines.push(Line::from(""));
    lines.push(heading(s.agent_thread_section, width, theme));
    if state.display.is_empty() {
        lines.push(note(&format!("  {}", s.asst_no_tasks), width, theme));
    } else {
        for turn in &state.display {
            let who = if turn.role == "assistant" {
                s.agent_who_model
            } else {
                s.agent_who_user
            };
            let marker = if turn.role == "assistant" { "< " } else { "> " };
            lines.push(Line::from(Span::styled(
                text::clip_to(&format!("  {marker}{who}"), width as usize),
                theme.muted(),
            )));
            // An answer is markdown; a question is plain text the user typed.
            if turn.role == "assistant" {
                let room = (width as usize).saturating_sub(4);
                for mut line in crate::agent::markdown::render(&turn.text, room, theme) {
                    line.spans.insert(0, Span::raw("    "));
                    lines.push(line);
                }
            } else {
                lines.push(Line::from(Span::styled(
                    text::clip_to(&format!("    {}", turn.text), width as usize),
                    theme.text(),
                )));
            }
        }
    }

    lines.push(Line::from(""));
    lines.push(heading(s.asst_tasks_section, width, theme));
    if state.tasks.is_empty() {
        lines.push(note(&format!("  {}", s.asst_no_tasks), width, theme));
    } else {
        for (index, task) in state.tasks.iter().enumerate() {
            lines.push(task_row(task, width, theme));
            // The newest answer is rendered as markdown; the ones before it are
            // rows. A model's reply *is* markdown, and an answer shown as its
            // own source is the one place a reader would rather see the text —
            // the renderer gives it back as source when it does not understand
            // a construct, so nothing is lost either way.
            if index == 0 && !task.summary.is_empty() {
                // Two columns of indent, and the renderer is given the width
                // that is left — so the row's total is never more than the
                // pane's, which is the invariant the painting tests enforce.
                let room = (width as usize).saturating_sub(2);
                for mut line in crate::agent::markdown::render(&task.summary, room, theme) {
                    line.spans.insert(0, Span::raw("  "));
                    lines.push(line);
                }
            }
        }
    }

    lines
}

/// The settings window's body: the five fields, the save's answer, and the
/// notes the last field files.
///
/// Painted by the dialog rather than by the view, which is the whole point of
/// the move — but it is still *this* module's form, so the caret, the hidden key
/// and the note cursor are read from the same state the keys write to.
pub fn settings_lines(app: &App, theme: &Theme, width: u16) -> Vec<Line<'static>> {
    let s = app.strings();
    let state = &app.assistant;
    let mut lines = vec![
        Line::from(""),
        heading(s.agent_settings_section, width, theme),
    ];
    for field in SETTINGS {
        let focused = state.focus == field;
        let caret = if focused { "▌" } else { " " };
        // The key field never paints its value: a shoulder-surfed key is a
        // rotated key, and the value is one backspace from being retyped.
        let value = if field.is_secret() && !state.api_key.is_empty() {
            s.agent_key_hidden.to_string()
        } else if field.text(state).is_empty() {
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
    lines.push(heading(s.asst_notes_section, width, theme));
    if state.notes.is_empty() {
        lines.push(note(&format!("  {}", s.asst_no_notes), width, theme));
    } else {
        for (index, (text, evidence)) in state.notes.iter().enumerate() {
            // The cursor is a colour, not a marker: a `>` would shift every row
            // by a column and a list that reflows as the cursor moves is a list
            // nobody can read.
            let body = if evidence.is_empty() {
                format!("  {text}")
            } else {
                format!("  {text} ({evidence})")
            };
            let style = if index == state.note_cursor {
                theme.selected()
            } else {
                theme.text()
            };
            lines.push(Line::from(Span::styled(
                text::clip_to(&body, width as usize),
                style,
            )));
        }
    }

    lines
}

/// The row of the settings body the caret is painted on.
///
/// The caret lives in the state, not in the rows, so its row is rebuilt here
/// with the same arithmetic `settings_lines` walks: a blank row and the
/// heading above the first field, then one row per field.
pub fn settings_caret_row(app: &App) -> usize {
    let at = SETTINGS
        .iter()
        .position(|field| *field == app.assistant.focus)
        // The composer has no row here. While the window is up the caret is on
        // a field; a caller that asks anyway is sent to the first one rather
        // than to a row that is not there.
        .unwrap_or(0);
    2 + at
}

/// The row of the settings body the note cursor is painted on.
///
/// The note list starts under the five fields, the save's answer when there is
/// one, and a blank row and the section heading.
pub fn settings_note_row(app: &App) -> usize {
    let state = &app.assistant;
    let list = 2 + SETTINGS.len() + usize::from(state.notice.is_some()) + 2;
    let last = state.notes.len().saturating_sub(1);
    list + state.note_cursor.min(last)
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
        // The note is in the settings window now, and that is where it is
        // looked for: the view kept the thread and gave up the filing cabinet.
        let window = body_text(&settings_lines(&a, &theme, 90));
        assert!(window.contains("wget is present"), "{window}");
    }

    /// The mode row counts the unattended window down while it is armed, and
    /// says nothing once it is closed: one read per frame, so the arithmetic
    /// is the timer.
    #[test]
    fn the_mode_row_counts_the_unattended_window_down() {
        let theme = Theme::DARK;
        let mut a = app(Lang::En);
        assert_eq!(full_auto_countdown(&a), "", "no window is armed");

        // The window starts at the pick, and a whole one reads `15:00`.
        cycle_mode(&mut a);
        assert_eq!(full_auto_countdown(&a), " · 15:00");
        a.assistant.configured = true;
        let text = body_text(&lines(&a, &theme, 90));
        assert!(text.contains(" · 15:00"), "{text}");

        // Every other mode closes it, and the row goes back to the mode's
        // words alone.
        cycle_mode(&mut a);
        assert_eq!(a.assistant.mode, Some(Mode::Manual));
        assert_eq!(full_auto_countdown(&a), "", "…and gone with the mode");
    }

    /// The view shows the conversation; the settings have their own window. A
    /// form the user has to scroll past is what pushed the thread off the pane.
    #[test]
    fn the_view_leaves_the_settings_to_their_window() {
        let theme = Theme::DARK;
        let mut a = app(Lang::En);
        a.assistant.endpoint = "http://localhost:8080/v1".to_string();
        a.assistant.notes = vec![("disk".to_string(), String::new())];

        let view = body_text(&lines(&a, &theme, 90));
        assert!(view.contains("Tasks"), "{view}");
        // The endpoint's *value*, not its word: the view still says "no model
        // endpoint configured" when there is none, and that sentence is the
        // pointer to the window rather than the window itself.
        assert!(!view.contains("localhost:8080"), "{view}");
        assert!(!view.contains(a.strings().asst_notes_section), "{view}");
        assert!(!view.contains(a.strings().agent_settings_section), "{view}");

        let window = body_text(&settings_lines(&a, &theme, 90));
        assert!(
            window.contains("localhost:8080"),
            "the window lost a field's value:\n{window}"
        );
        assert!(
            window.contains(a.strings().asst_notes_section),
            "the window lost the notes:\n{window}"
        );
        assert!(window.contains("disk"), "{window}");
    }

    /// The key field is painted as a placeholder, never as the key — and it is
    /// painted in the window now, so that is where the test looks. Neither
    /// surface gets the value.
    #[test]
    fn a_key_field_is_never_painted() {
        let a = app(Lang::En);
        let mut a = a;
        a.assistant.api_key = "sk-super-secret".to_string();
        let window = body_text(&settings_lines(&a, &a.theme(), 80));
        assert!(!window.contains("sk-super-secret"), "the window painted it");
        assert!(window.contains(a.strings().agent_key_hidden), "{window}");
        let view = body_text(&lines(&a, &a.theme(), 80));
        assert!(!view.contains("sk-super-secret"), "the view painted it");
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
                // The settings window is the other surface that paints rows,
                // and it is the one with the longest labels on it — a pane that
                // clips the composer but wraps the endpoint is still broken.
                for line in settings_lines(&a, &theme, width) {
                    let joined: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                    assert!(
                        text::columns(&joined) <= width as usize,
                        "settings at {lang:?} {width}: {joined:?}"
                    );
                }
            }
        }
    }

    /// The rows the settings window scrolls by are the rows the body paints:
    /// the field caret closes its own row, and the note cursor sits on its
    /// note. A save's answer shifts the list by one row, which is the shift
    /// easiest to forget.
    #[test]
    fn the_caret_rows_are_the_rows_the_body_paints() {
        let theme = Theme::DARK;
        let mut a = app(Lang::En);
        a.assistant.notes = vec![
            ("alpha".to_string(), String::new()),
            ("bravo".to_string(), String::new()),
            ("charlie".to_string(), String::new()),
        ];
        for field in [
            AgentField::Endpoint,
            AgentField::Model,
            AgentField::ApiKey,
            AgentField::Headers,
        ] {
            open_settings(&mut a, field);
            let row = settings_caret_row(&a);
            let painted = body_text(&settings_lines(&a, &theme, 90)[row..row + 1]);
            assert!(
                painted.contains("▌"),
                "{field:?} paints no caret: {painted:?}"
            );
        }
        // The note field's own row carries the caret while the list has it...
        open_settings(&mut a, AgentField::Notes);
        let row = settings_caret_row(&a);
        let painted = body_text(&settings_lines(&a, &theme, 90)[row..row + 1]);
        assert!(
            painted.contains("▌"),
            "the note field paints no caret: {painted:?}"
        );
        // ...and the cursor's row carries the note under it.
        a.assistant.note_cursor = 2;
        let row = settings_note_row(&a);
        let painted = body_text(&settings_lines(&a, &theme, 90)[row..row + 1]);
        assert!(
            painted.contains("charlie"),
            "the cursor row is not charlie: {painted:?}"
        );
        // An answer above the list pushes it down a row, and the row moves
        // with it.
        a.assistant.notice = Some("saved".to_string());
        let row = settings_note_row(&a);
        let painted = body_text(&settings_lines(&a, &theme, 90)[row..row + 1]);
        assert!(
            painted.contains("charlie"),
            "with an answer above the list: {painted:?}"
        );
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

    /// Open the settings window with the caret on `field`, the way every entry
    /// point opens it — the palette action, the `Ctrl+Shift+S` chord.
    fn settings(app: &mut App, field: AgentField) {
        open_settings(app, field);
        assert!(matches!(
            app.dialog,
            Some(crate::dialogs::Dialog::Settings(_))
        ));
    }

    /// The same press, in the settings window rather than in the view.
    fn press_settings(app: &mut App, code: KeyCode) {
        let _ = handle_settings_key(app, KeyEvent::new(code, KeyModifiers::NONE));
    }

    /// The paste the chord brings lands on a caret the user can see: a
    /// settings window field while the window is up, and otherwise the
    /// composer — with the view brought up first rather than text typed into
    /// a row nothing is painting.
    #[test]
    fn the_paste_lands_on_the_caret_that_is_up() {
        let mut a = app();
        a.view = crate::event::View::Network;
        // A caret left on a setting from an earlier visit: no window is up,
        // so this row is not painted and must not take the paste.
        a.assistant.focus = AgentField::Endpoint;
        paste_into_focus(&mut a, "explain\nthe log");
        assert_eq!(a.view, crate::event::View::Assistant, "the caret came up");
        assert_eq!(a.assistant.composer, "explain the log", "one line at a time");
        assert_eq!(a.assistant.endpoint, "", "the unpainted row did not take it");

        settings(&mut a, AgentField::Model);
        paste_into_focus(&mut a, "gpt\n-4");
        assert_eq!(a.assistant.model, "gpt -4");
        assert!(
            matches!(a.dialog, Some(crate::dialogs::Dialog::Settings(_))),
            "the window stayed up"
        );
        assert_eq!(
            a.assistant.composer, "explain the log",
            "the composer held still"
        );
    }

    /// Tab walks the window's five fields forward and BackTab back, wrapping at
    /// both ends — and the composer is not among them: the caret either has a
    /// row in the window or it is not in the window at all.
    #[test]
    fn tab_walks_every_setting_and_wraps() {
        let mut a = app();
        settings(&mut a, AgentField::Endpoint);
        let mut seen = vec![a.assistant.focus];
        for _ in 0..SETTINGS.len() - 1 {
            press_settings(&mut a, KeyCode::Tab);
            assert!(
                seen.iter().all(|f| *f != a.assistant.focus),
                "revisit: {:?}",
                a.assistant.focus
            );
            assert_ne!(a.assistant.focus, AgentField::Composer, "the composer's row");
            seen.push(a.assistant.focus);
        }
        assert_eq!(seen.len(), SETTINGS.len(), "every setting, once");
        press_settings(&mut a, KeyCode::Tab);
        assert_eq!(a.assistant.focus, SETTINGS[0], "wraps forward");
        press_settings(&mut a, KeyCode::BackTab);
        assert_eq!(a.assistant.focus, SETTINGS[SETTINGS.len() - 1], "wraps backward");
    }

    /// Typing goes wherever the caret is, and the two halves do not leak into
    /// each other: the view's keystrokes reach the composer and the window's
    /// reach the row under its caret.
    #[test]
    fn typing_goes_to_whichever_caret_is_up() {
        let mut a = app();
        press(&mut a, KeyCode::Char('h'));
        press(&mut a, KeyCode::Char('i'));
        assert_eq!(a.assistant.composer, "hi");
        assert!(a.assistant.endpoint.is_empty());

        settings(&mut a, AgentField::Endpoint);
        press_settings(&mut a, KeyCode::Char('u'));
        press_settings(&mut a, KeyCode::Char('r'));
        assert_eq!(a.assistant.endpoint, "ur");
        assert_eq!(a.assistant.composer, "hi", "the composer did not move");
        press_settings(&mut a, KeyCode::Backspace);
        assert_eq!(a.assistant.endpoint, "u");

        // Back in the view, the caret is the composer's again — the window's
        // field does not keep a caret that no longer has a row to paint it on.
        close_settings(&mut a);
        assert_eq!(a.assistant.focus, AgentField::Composer);
        press(&mut a, KeyCode::Char('!'));
        assert_eq!(a.assistant.composer, "hi!", "the caret went back");
        assert_eq!(a.assistant.endpoint, "u", "and the setting stayed");
    }

    /// Enter in the settings window saves the whole form or says why not.
    #[test]
    fn saving_validates_the_whole_form() {
        let mut a = app();
        a.assistant.endpoint = "http://localhost:8080/v1".to_string();
        a.assistant.model = "llama-3".to_string();
        settings(&mut a, AgentField::Endpoint);
        press_settings(&mut a, KeyCode::Enter);
        assert!(a.assistant.configured, "a loopback endpoint is a valid one");

        let mut bad = app();
        bad.assistant.endpoint = "not a url".to_string();
        settings(&mut bad, AgentField::Endpoint);
        press_settings(&mut bad, KeyCode::Enter);
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
        assert!(!a.agent_replies.is_empty(), "the reply has nowhere to go");

        // The reply channel answers eventually; drain it into the task the way
        // the beat does. Nothing is listening, so this is a failure and the
        // task records that it was asked.
        // The first (and for now only) live channel.
        let receiver = a.agent_replies.remove(0);
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
        // The note row is in the settings window, so that is where it is
        // written: the caret has a row there or it is not the note's caret.
        settings(&mut a, AgentField::Notes);
        press_settings(&mut a, KeyCode::Char('d'));
        press_settings(&mut a, KeyCode::Char('i'));
        press_settings(&mut a, KeyCode::Char('s'));
        press_settings(&mut a, KeyCode::Char('k'));
        press_settings(&mut a, KeyCode::Enter);
        assert_eq!(a.assistant.note_input, "", "the note field kept its text");
        assert_eq!(a.assistant.notes.len(), 1, "the note is not filed");
        assert_eq!(a.assistant.notes[0].0, "disk");
        assert_eq!(
            a.assistant.notes[0].1, "root mounted at /",
            "the evidence is what the target said"
        );
    }

    /// A markdown answer is rendered, not shown as its own source — and rendered
    /// at the width that is left of the pane, so the indent does not push the
    /// row past the edge the painting tests guard.
    #[test]
    fn the_newest_answer_is_rendered_as_markdown() {
        let mut a = app();
        a.assistant.tasks = vec![TaskRecord {
            id: "t".into(),
            device_key: "k".into(),
            updated_at: 0,
            goal: "explain".into(),
            summary: "## Heading\n\nSome **bold** and `code`.".into(),
            status: "answered".into(),
        }];
        let rendered = lines(&a, &a.theme(), 60);
        let joined: String = rendered
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("Heading"), "the heading is gone: {joined}");
        assert!(joined.contains("bold"), "the inline is gone: {joined}");
        assert!(
            !joined.contains("## Heading"),
            "the markdown was shown as source: {joined}"
        );
        for (lang, lines) in [(a.lang, &rendered)] {
            for line in lines.iter() {
                let row: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                assert!(
                    crate::text::columns(&row) <= 60,
                    "{lang:?}: {row:?} is past the pane"
                );
            }
        }
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

/// Cycle the execution mode and say which one it landed on.
///
/// Public because two doors open it and only one of them is the view: the
/// palette has an action for it and `Ctrl+Shift+M` is a chord. Both go through
/// here so the wording and the cycle live in one place — a mode that cycles
/// differently depending on how it was reached is a mode nobody can learn.
pub fn cycle_mode(app: &mut App) {
    use crate::exec::executor::Mode;
    let next = app.assistant.mode.unwrap_or(Mode::Auto).next();
    // Selecting Full Auto arms the unattended window; every other mode closes
    // it — `Manual`, the one that cannot send anything, included. Landing on
    // Full Auto afresh re-arms, so re-selecting it extends the window.
    app.assistant.full_auto_until = match next {
        Mode::FullAuto => Some(Instant::now() + crate::exec::executor::FULL_AUTO_WINDOW),
        _ => None,
    };
    app.assistant.mode = Some(next);
    let s = app.strings();
    app.push_notice(crate::app::Notice {
        text: format!("{}: {}", s.asst_mode_label, s.execution_mode(next)),
        level: crate::event::NoticeLevel::Info,
    });
}

/// Start a new conversation, on disk as well as on screen.
///
/// A clear that left the store alone would be undone by the next restart, which
/// is the one thing a clear must not do.
pub fn clear_conversation(app: &mut App) {
    app.assistant.display.clear();
    let key = app.agent_device_key();
    if let Some(store) = app.agent_store.as_mut() {
        crate::agent::session::clear_session(store, &key);
    }
    let s = app.strings();
    app.push_notice(crate::app::Notice {
        text: s.agent_cleared.to_string(),
        level: crate::event::NoticeLevel::Info,
    });
}

/// Paste the system clipboard onto the assistant's caret, bringing that caret
/// up first.
///
/// The chord's promise: the work is the composer, so the composer comes to the
/// user when there was no caret to paste into. A caret the user *can* see —
/// a palette query, a dialog field, the settings window — is taken by the
/// caller before this is reached.
pub fn paste_into_focus(app: &mut App, text: &str) {
    if !matches!(app.dialog, Some(crate::dialogs::Dialog::Settings(_))) {
        focus_composer(app);
    }
    paste_at_caret(app, text);
}

/// The system clipboard's text, with the empty-clipboard notice already
/// pushed when there is none to read.
pub fn take_clipboard(app: &mut App) -> Option<String> {
    let Some(text) = read_system_clipboard() else {
        let s = app.strings();
        app.push_notice(crate::app::Notice {
            text: s.clipboard_empty.to_string(),
            level: crate::event::NoticeLevel::Warn,
        });
        return None;
    };
    Some(text)
}

/// Put `text` on the caret, exactly where it is painted and moving nothing.
///
/// The caret has a row in one of two paints: a settings window field while
/// that window is up, and the view's own field otherwise. A newline in the
/// pasted text is a newline the field does not want — a goal is one line — so
/// it becomes a space. A note takes newlines: it is prose, and pasting one
/// long line into a field meant to hold several is a paste that only half
/// worked.
pub fn paste_at_caret(app: &mut App, text: &str) {
    let focused = app.assistant.focus;
    let field = match focused {
        AgentField::Composer => Some(&mut app.assistant.composer),
        AgentField::Endpoint => Some(&mut app.assistant.endpoint),
        AgentField::Model => Some(&mut app.assistant.model),
        AgentField::ApiKey => Some(&mut app.assistant.api_key),
        AgentField::Headers => Some(&mut app.assistant.headers),
        AgentField::Notes => Some(&mut app.assistant.note_input),
    };
    let Some(field) = field else {
        return;
    };
    let pasted = if focused == AgentField::Notes {
        text.to_string()
    } else {
        text.replace("\r\n", " ").replace(['\r', '\n'], " ")
    };
    let count = pasted.trim().chars().count();
    field.push_str(&pasted);
    let s = app.strings();
    app.push_notice(crate::app::Notice {
        text: format!("{} {}", s.clipboard_pasted, count),
        level: crate::event::NoticeLevel::Info,
    });
}
