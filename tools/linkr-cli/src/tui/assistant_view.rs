//! Assistant panel (CONTRACTS.md section 5 / WEB_UX_SPEC section 7): message
//! log with tool-call blocks, execution-mode picker, composer, usage line and
//! the lazy agent runtime.
//!
//! The agent task is spawned on the first question through
//! [`crate::agent::spawn`], guarded with `catch_unwind` while that workstream
//! is still landing: a panic turns into a toast instead of killing the TUI.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::agent_settings;
use super::state::{AgentRuntime, App, TextField};
use crate::agent::{AgentEvent, ExecMode};
use crate::event::NoticeLevel;

/// Help text of the three modes (WEB_UX_SPEC section 7.2, English column).
pub const MODE_HELP: [(&str, &str); 3] = [
    (
        "Manual",
        "AI proposes commands; click Send to enter them on the target.",
    ),
    (
        "Auto · Recommended",
        "Low-risk queries run at a recognized shell prompt; other input needs approval. Destructive commands need approval in every mode.",
    ),
    (
        "Full Auto",
        "Commands run without confirmation. Recognized destructive or irreversible commands still need your approval.",
    ),
];

pub const COMPOSER_MAX: usize = 4000;
pub const STOP_MESSAGE: &str = "Stopped. Already sent input cannot be recalled; use Ctrl-C in the terminal to interrupt the target program.";

/// One rendered row of the chat log.
#[derive(Debug, Clone)]
pub enum Entry {
    User(String),
    Assistant(String),
    ToolStart {
        name: String,
        args: String,
    },
    ToolEnd {
        name: String,
        result: String,
        ok: bool,
    },
    System(String),
}

/// State of the assistant panel.
#[derive(Default)]
pub struct AssistantState {
    pub entries: Vec<Entry>,
    pub composer: TextField,
    /// Mode picker open (web `#agentModePicker`).
    pub picker_open: bool,
    pub picker_sel: usize,
    /// Accumulated streaming reply of the running turn.
    pub streaming: String,
    pub busy: bool,
    pub status: String,
    pub usage: Option<(u64, u64, u64, f64)>,
    pub scroll: usize,
}

impl AssistantState {
    pub fn mode_index(exec: ExecMode) -> usize {
        match exec {
            ExecMode::Manual => 0,
            ExecMode::Auto => 1,
            ExecMode::FullAuto => 2,
        }
    }

    pub fn exec_of(index: usize) -> ExecMode {
        match index {
            0 => ExecMode::Manual,
            2 => ExecMode::FullAuto,
            _ => ExecMode::Auto,
        }
    }

    /// `Tokens this conversation …` line (WEB_UX_SPEC section 7.5).
    pub fn usage_line(&self) -> Option<String> {
        let (input, output, cache, cost) = self.usage?;
        let total = input + output;
        let mut line = format!(
            "Tokens this conversation {} · ↑{} ↓{}",
            super::status::format_count(total),
            super::status::format_count(input),
            super::status::format_count(output),
        );
        if cache > 0 {
            line.push_str(&format!(" ⚡{}", super::status::format_count(cache)));
        }
        if cost > 0.0 {
            line.push_str(&format!(" · Estimated cost ~{}", format_cost(cost)));
        } else {
            line.push_str(" · Prices not set");
        }
        Some(line)
    }
}

/// Cost display rule of `formatCost()` in `agent_usage.js`: <0.01 → 4 dp,
/// <1 → 3 dp, otherwise 2 dp.
pub fn format_cost(cost: f64) -> String {
    if cost < 0.01 {
        format!("${cost:.4}")
    } else if cost < 1.0 {
        format!("${cost:.3}")
    } else {
        format!("${cost:.2}")
    }
}

// --- markdown ----------------------------------------------------------------

/// Minimal inline markdown: `**bold**` and `` `code` `` runs become spans.
pub fn markdown(text: &str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let bytes = text.as_bytes();
    let mut index = 0usize;
    let mut plain_start = 0usize;
    while index < bytes.len() {
        let marker = if bytes[index] == b'*' && bytes.get(index + 1) == Some(&b'*') {
            Some(2usize)
        } else if bytes[index] == b'`' {
            Some(1usize)
        } else {
            None
        };
        let Some(width) = marker else {
            index += 1;
            continue;
        };
        let token = &text[index..index + width];
        let close = text[index + width..].find(token);
        let Some(close) = close else {
            index += width;
            continue;
        };
        let start = index + width;
        let end = start + close;
        if plain_start < index {
            spans.push(Span::raw(text[plain_start..index].to_string()));
        }
        if width == 2 {
            spans.push(Span::styled(
                text[start..end].to_string(),
                Style::default().add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::styled(
                text[start..end].to_string(),
                Style::default().fg(Color::LightGreen),
            ));
        }
        index = end + width;
        plain_start = index;
    }
    if plain_start < text.len() {
        spans.push(Span::raw(text[plain_start..].to_string()));
    }
    spans
}

fn wrapped(text: &str, width: u16, style: Style) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    for raw in text.split('\n') {
        let spans = markdown(raw);
        let mut current: Vec<Span<'static>> = Vec::new();
        let mut len = 0usize;
        for span in spans {
            let w = unicode_width::UnicodeWidthStr::width(span.content.as_ref());
            if len + w > width as usize && !current.is_empty() {
                out.push(Line::from(std::mem::take(&mut current)));
                len = 0;
            }
            current.push(span.style(style));
            len += w;
        }
        out.push(Line::from(current));
    }
    out
}

// --- rendering ---------------------------------------------------------------

/// Body of the Assistant view.
pub fn render_lines(app: &App, width: u16) -> Vec<Line<'static>> {
    let state = &app.assistant;
    let width = width.max(20);
    let mut lines: Vec<Line<'static>> = Vec::new();

    // Header: mode button + actions.
    let (mode_label, mode_help) = MODE_HELP[AssistantState::mode_index(app.exec_mode)];
    lines.push(Line::from(vec![
        Span::styled(
            "Agent",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  ", Style::default()),
        Span::styled(
            format!("[{mode_label}]"),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "  Settings · New chat · Exit",
            Style::default().fg(Color::DarkGray),
        ),
    ]));
    lines.push(Line::from(Span::styled(
        mode_help.to_string(),
        Style::default().fg(Color::DarkGray),
    )));

    if state.picker_open {
        lines.push(Line::from(Span::styled(
            "Execution mode",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
        for (index, (label, help)) in MODE_HELP.iter().enumerate() {
            let selected = index == state.picker_sel;
            lines.push(Line::from(vec![
                Span::styled(
                    if selected { "▸ " } else { "  " },
                    Style::default().fg(Color::Yellow),
                ),
                Span::styled(
                    if app.exec_mode == AssistantState::exec_of(index) {
                        "● "
                    } else {
                        "○ "
                    },
                    Style::default().fg(Color::Green),
                ),
                Span::styled(
                    (*label).to_string(),
                    Style::default()
                        .fg(if selected { Color::White } else { Color::Gray })
                        .add_modifier(if selected {
                            Modifier::BOLD
                        } else {
                            Modifier::empty()
                        }),
                ),
            ]));
            if selected {
                for line in wrapped(
                    help,
                    width.saturating_sub(4),
                    Style::default().fg(Color::DarkGray),
                ) {
                    lines.push(Line::from(
                        line.spans
                            .into_iter()
                            .map(|s| s.style(Style::default().fg(Color::DarkGray)))
                            .collect::<Vec<_>>(),
                    ));
                }
            }
        }
        lines.push(Line::from(Span::styled(
            "↑/↓ pick · Enter engage · Esc close",
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::from(""));
    }

    if state.entries.is_empty() && state.streaming.is_empty() {
        lines.push(Line::from(Span::styled(
            "Ask a question about the device; the assistant reads the serial journal.",
            Style::default().fg(Color::DarkGray),
        )));
    }

    for entry in &state.entries {
        match entry {
            Entry::User(text) => {
                lines.push(Line::from(Span::styled(
                    "you › ",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )));
                lines.extend(wrapped(
                    text,
                    width.saturating_sub(6),
                    Style::default().fg(Color::White),
                ));
            }
            Entry::Assistant(text) => {
                lines.push(Line::from(Span::styled(
                    "ai › ",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                )));
                lines.extend(wrapped(
                    text,
                    width.saturating_sub(5),
                    Style::default().fg(Color::Gray),
                ));
            }
            Entry::ToolStart { name, args } => {
                lines.push(Line::from(vec![
                    Span::styled("  ⚙ ", Style::default().fg(Color::Magenta)),
                    Span::styled(
                        name.clone(),
                        Style::default()
                            .fg(Color::Magenta)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!(" {args}"), Style::default().fg(Color::DarkGray)),
                ]));
            }
            Entry::ToolEnd { name, result, ok } => {
                let color = if *ok {
                    Color::DarkGray
                } else {
                    Color::LightRed
                };
                let clip: String = result.chars().take(width as usize).collect();
                lines.push(Line::from(vec![
                    Span::styled(
                        if *ok { "  ✓ " } else { "  ✗ " },
                        Style::default().fg(color),
                    ),
                    Span::styled(name.clone(), Style::default().fg(color)),
                    Span::styled(format!(" {clip}"), Style::default().fg(color)),
                ]));
            }
            Entry::System(text) => {
                lines.extend(wrapped(text, width, Style::default().fg(Color::DarkGray)));
            }
        }
    }

    if !state.streaming.is_empty() {
        lines.extend(wrapped(
            &state.streaming,
            width.saturating_sub(5),
            Style::default().fg(Color::Gray),
        ));
    }
    if state.busy {
        lines.push(Line::from(Span::styled(
            "· · ·",
            Style::default().fg(Color::Yellow),
        )));
    }

    lines.push(Line::from(""));
    if let Some(usage) = state.usage_line() {
        lines.push(Line::from(Span::styled(
            usage,
            Style::default().fg(Color::DarkGray),
        )));
    }
    if !state.status.is_empty() {
        lines.push(Line::from(Span::styled(
            state.status.clone(),
            Style::default().fg(Color::LightYellow),
        )));
    }

    // Composer.
    lines.push(Line::from(Span::styled(
        "Describe the problem",
        Style::default().fg(Color::DarkGray),
    )));
    let (text, _) = state.composer.display(None);
    let shown = if text.is_empty() { String::new() } else { text };
    let (head, tail) = if shown.chars().count() > width as usize {
        let cut: String = shown
            .chars()
            .rev()
            .take(width as usize - 1)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        ("…".to_string(), cut)
    } else {
        (String::new(), shown)
    };
    lines.push(Line::from(vec![
        Span::styled(
            "› ",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(head, Style::default().fg(Color::DarkGray)),
        Span::styled(
            tail,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ]));
    lines.push(Line::from(Span::styled(
        "Enter newline · Ctrl+Enter send · Ctrl+Shift+M mode · Ctrl+Shift+S settings",
        Style::default().fg(Color::DarkGray),
    )));
    lines
}

// --- keys --------------------------------------------------------------------

/// Keys of the Assistant view / focused composer.
pub fn handle_key(app: &mut App, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);

    if app.assistant.picker_open {
        match key.code {
            KeyCode::Esc => {
                app.assistant.picker_open = false;
                return;
            }
            KeyCode::Up | KeyCode::Left => {
                app.assistant.picker_sel = app.assistant.picker_sel.saturating_sub(1);
                return;
            }
            KeyCode::Down | KeyCode::Right => {
                app.assistant.picker_sel = (app.assistant.picker_sel + 1).min(MODE_HELP.len() - 1);
                return;
            }
            KeyCode::Home => {
                app.assistant.picker_sel = 0;
                return;
            }
            KeyCode::End => {
                app.assistant.picker_sel = MODE_HELP.len() - 1;
                return;
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                let index = app.assistant.picker_sel;
                app.assistant.picker_open = false;
                set_mode(app, AssistantState::exec_of(index));
                return;
            }
            _ => return,
        }
    }

    match key.code {
        KeyCode::Char('m') if ctrl && shift => {
            app.assistant.picker_open = true;
            app.assistant.picker_sel = AssistantState::mode_index(app.exec_mode);
        }
        KeyCode::Char('s') if ctrl && shift => open_settings(app),
        KeyCode::Char('n') if ctrl && shift => {
            app.assistant = super::assistant_view::AssistantState::default();
            app.assistant.status = "New chat.".to_string();
        }
        KeyCode::Enter if ctrl => submit(app),
        KeyCode::Enter => app.assistant.composer.insert_char('\n'),
        KeyCode::Char(c) if !ctrl => {
            let len = app.assistant.composer.text.chars().count();
            if len < COMPOSER_MAX {
                app.assistant.composer.insert_char(c);
            }
        }
        KeyCode::Backspace => app.assistant.composer.backspace(),
        KeyCode::Delete => app.assistant.composer.delete(),
        KeyCode::Left => app.assistant.composer.left(),
        KeyCode::Right => app.assistant.composer.right(),
        KeyCode::Up if !shift => app.assistant.composer.home(),
        KeyCode::Down if !shift => app.assistant.composer.end(),
        KeyCode::Home => app.assistant.composer.home(),
        KeyCode::End => app.assistant.composer.end(),
        KeyCode::PageUp => app.assistant.scroll = app.assistant.scroll.saturating_add(5),
        KeyCode::PageDown => app.assistant.scroll = app.assistant.scroll.saturating_sub(5),
        _ => {}
    }
}

/// Open the AI configuration dialog.
pub fn open_settings(app: &mut App) {
    if app.assistant.busy {
        app.toast(NoticeLevel::Warn, agent_settings::BUSY);
        return;
    }
    app.dialog = Some(super::dialogs::Dialog::Settings(
        super::agent_settings::AgentSettingsState::load(),
    ));
}

/// Engage an execution mode (the running turn is stopped first, web
/// `stop("modeChanged")`).
pub fn set_mode(app: &mut App, mode: ExecMode) {
    if app.exec_mode == mode {
        return;
    }
    if app.assistant.busy {
        push_system(
            app,
            "Mode changed; conversation retained. This run stopped and pending input was cancelled; sent input cannot be recalled. Ask again to continue.",
        );
        if let Some(agent) = &app.agent {
            agent.handle.stop();
        }
        app.assistant.busy = false;
    }
    app.exec_mode = mode;
    app.assistant.status = format!("Mode: {}", MODE_HELP[AssistantState::mode_index(mode)].0);
}

fn push_system(app: &mut App, text: &str) {
    app.assistant.entries.push(Entry::System(text.to_string()));
}

/// Spawn the runtime on the first question (guarded: `agent::spawn` may still
/// be a `todo!()` while workstream A lands).
fn ensure_agent(app: &mut App) -> bool {
    if app.agent.is_some() {
        return true;
    }
    let config = agent_settings::runtime_config();
    if config.is_none() {
        app.toast(
            NoticeLevel::Warn,
            "Set the AI configuration first (Ctrl+P → agent.settings).",
        );
        app.assistant.status = "No AI configuration saved.".to_string();
        return false;
    }
    let broker = Arc::new(app.broker.clone()) as Arc<dyn crate::agent::ApprovalBroker>;
    let session = app.session.clone();
    let bus = app.bus.clone();
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        crate::agent::spawn(config, broker, session, bus)
    }));
    match outcome {
        Ok(handle) => {
            let events = handle.subscribe();
            app.agent = Some(AgentRuntime {
                handle,
                events,
                last_error: None,
            });
            true
        }
        Err(_) => {
            app.toast(
                NoticeLevel::Error,
                "The assistant runtime is not available in this build.",
            );
            app.assistant.status = "Assistant unavailable.".to_string();
            false
        }
    }
}

/// Send the composed question (Ctrl+Enter).
pub fn submit(app: &mut App) {
    let question = app.assistant.composer.text.trim().to_string();
    if question.is_empty() || app.assistant.busy {
        return;
    }
    if !ensure_agent(app) {
        return;
    }
    app.assistant.composer.clear();
    app.assistant.entries.push(Entry::User(question.clone()));
    app.assistant.streaming.clear();
    app.assistant.busy = true;
    app.assistant.status = String::new();
    if let Some(agent) = &app.agent {
        agent.handle.ask(question);
    }
}

/// Drain pending agent events (once per loop tick).
pub fn poll(app: &mut App) {
    let mut batch = Vec::new();
    if let Some(agent) = &mut app.agent {
        loop {
            match agent.events.try_recv() {
                Ok(event) => batch.push(event),
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::TryRecvError::Closed) => break,
            }
        }
    }
    for event in batch {
        apply_event(app, event);
    }
}

/// Fold one [`AgentEvent`] into the panel.
pub fn apply_event(app: &mut App, event: AgentEvent) {
    match event {
        AgentEvent::MessageStart { role } => {
            if role == "assistant" {
                app.assistant.streaming.clear();
            }
        }
        AgentEvent::AssistantDelta(text) => app.assistant.streaming.push_str(&text),
        AgentEvent::ToolStart { name, args } => {
            if !app.assistant.streaming.is_empty() {
                let text = std::mem::take(&mut app.assistant.streaming);
                app.assistant.entries.push(Entry::Assistant(text));
            }
            app.assistant.entries.push(Entry::ToolStart { name, args });
        }
        AgentEvent::ToolEnd { name, result, ok } => {
            let result = super::replies::redact_secrets(&result);
            app.assistant
                .entries
                .push(Entry::ToolEnd { name, result, ok });
        }
        AgentEvent::Usage {
            input,
            output,
            cache_read,
            cost,
            ..
        } => {
            app.assistant.usage = Some((input, output, cache_read, cost));
        }
        AgentEvent::RunFinished { reason } => {
            if !app.assistant.streaming.is_empty() {
                let text = std::mem::take(&mut app.assistant.streaming);
                app.assistant.entries.push(Entry::Assistant(text));
            }
            app.assistant.busy = false;
            app.assistant.status = reason;
        }
        AgentEvent::Error(message) => {
            if !app.assistant.streaming.is_empty() {
                let text = std::mem::take(&mut app.assistant.streaming);
                app.assistant.entries.push(Entry::Assistant(text));
            }
            app.assistant.busy = false;
            app.assistant.status = message.clone();
            app.toast(NoticeLevel::Error, super::replies::redact_secrets(&message));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_labels_and_help_match_the_spec() {
        assert_eq!(MODE_HELP[0].0, "Manual");
        assert_eq!(MODE_HELP[1].0, "Auto · Recommended");
        assert_eq!(MODE_HELP[2].0, "Full Auto");
        assert_eq!(
            MODE_HELP[1].1,
            "Low-risk queries run at a recognized shell prompt; other input needs approval. Destructive commands need approval in every mode."
        );
    }

    #[test]
    fn mode_index_round_trips() {
        for index in 0..3 {
            let mode = AssistantState::exec_of(index);
            assert_eq!(AssistantState::mode_index(mode), index);
        }
        assert_eq!(AssistantState::exec_of(9), ExecMode::Auto);
    }

    #[test]
    fn cost_formatting_follows_the_spec_thresholds() {
        assert_eq!(format_cost(1.5), "$1.50");
        assert_eq!(format_cost(0.5), "$0.500");
        assert_eq!(format_cost(0.005), "$0.0050");
        assert_eq!(format_cost(0.02), "$0.020");
    }

    #[test]
    fn markdown_marks_bold_and_code_runs() {
        let spans = markdown("run `ls` and **verify** it");
        let joined: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(joined, "run ls and verify it");
        assert!(
            spans[1].style.add_modifier.contains(Modifier::BOLD) || spans[1].style.fg.is_some()
        );
    }

    #[test]
    fn usage_line_uses_grouped_counts() {
        let mut state = AssistantState {
            usage: Some((1234, 56, 7, 0.0)),
            ..AssistantState::default()
        };
        let line = state.usage_line().unwrap();
        assert_eq!(
            line,
            "Tokens this conversation 1,290 · ↑1,234 ↓56 ⚡7 · Prices not set"
        );
        state.usage = Some((100, 50, 0, 0.5));
        let line = state.usage_line().unwrap();
        assert!(line.ends_with("· Estimated cost ~$0.500"), "{line}");
    }
}
