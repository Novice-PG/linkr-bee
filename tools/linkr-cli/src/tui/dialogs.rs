//! Modal dialogs: confirmations, assistant approvals, UART settings, the help
//! overlay and the notice log (CONTRACTS.md section 5).
//!
//! One dialog is open at a time; `Esc` always closes. Confirmations reuse the
//! exact web strings so a test can pin them.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use tokio::sync::oneshot;

use super::agent_settings::AgentSettingsState;
use super::replies::parse_uart_reply;
use super::state::{App, TextField};
use crate::agent::{ApprovalBroker, ApprovalDecision, ApprovalKind, ApprovalRequest};
use crate::event::NoticeLevel;

/// `confirmReboot` of the web client (WEB_UX_SPEC section 1.6).
pub const REBOOT_CONFIRM: &str = "Reboot the connected device now?";
pub const QUIT_CONFIRM: &str = "Quit the TUI now?";
pub const DISCONNECT_CONFIRM: &str = "Disconnect from the current device now?";

/// Web `#uartInput` default (`PYTHON_CLI_SPEC` / WEB_UX_SPEC section 1.3).
pub const DEFAULT_UART: &str = "115200,8,n,1,n";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmKind {
    Reboot,
    Disconnect,
    Quit,
}

/// One approval request parked by [`TuiBroker`] until the user answers.
pub struct PendingApproval {
    pub request: ApprovalRequest,
    tx: oneshot::Sender<ApprovalDecision>,
}

impl PendingApproval {
    /// Answer the agent. Dropping the dialog without answering rejects the
    /// request (the agent sees a closed channel).
    pub fn resolve(self, decision: ApprovalDecision) {
        let _ = self.tx.send(decision);
    }
}

/// Queue shared between the agent task (producer) and the TUI (consumer).
#[derive(Clone, Default)]
pub struct TuiBroker {
    queue: std::sync::Arc<std::sync::Mutex<VecDeque<PendingApproval>>>,
}

use std::collections::VecDeque;

impl TuiBroker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Take the oldest pending request, if any (called once per loop tick).
    pub fn take_pending(&self) -> Option<PendingApproval> {
        self.queue
            .lock()
            .map(|mut queue| queue.pop_front())
            .ok()
            .flatten()
    }
}

impl ApprovalBroker for TuiBroker {
    fn ask(&self, request: ApprovalRequest) -> oneshot::Receiver<ApprovalDecision> {
        let (tx, rx) = oneshot::channel();
        if let Ok(mut queue) = self.queue.lock() {
            queue.push_back(PendingApproval { request, tx });
        }
        // On a poisoned lock the sender is dropped: the agent sees a closed
        // channel, which reads as a rejection.
        rx
    }
}

/// Everything shown above the frame.
// The settings state is the fat variant; it lives for one dialog at a time,
// so it stays inline instead of paying an indirection per keystroke.
#[allow(clippy::large_enum_variant)]
pub enum Dialog {
    Confirm {
        kind: ConfirmKind,
        title: String,
        message: String,
    },
    Approval(Box<PendingApproval>),
    Uart {
        field: TextField,
        status: Option<String>,
        error: Option<String>,
        reply: Option<oneshot::Receiver<Result<crate::protocol::MgmtReply, String>>>,
    },
    Settings(AgentSettingsState),
    Help,
    Notices,
}

impl Dialog {
    pub fn title(&self) -> &'static str {
        match self {
            Dialog::Confirm { .. } => "Confirm",
            Dialog::Approval(_) => "Assistant approval",
            Dialog::Uart { .. } => "UART settings",
            Dialog::Settings(_) => "AI configuration",
            Dialog::Help => "Keyboard help",
            Dialog::Notices => "Notices",
        }
    }
}

// --- rendering ---------------------------------------------------------------

fn rule(width: u16) -> Line<'static> {
    Line::from(Span::styled(
        "─".repeat(width as usize),
        Style::default().fg(Color::DarkGray),
    ))
}

pub fn wrap_line(text: &str, width: u16) -> Vec<Line<'static>> {
    let width = width.max(1) as usize;
    let mut out = Vec::new();
    let mut current = String::new();
    let mut len = 0usize;
    for word in text.split(' ') {
        let w = unicode_width::UnicodeWidthStr::width(word);
        if !current.is_empty() && len + 1 + w > width {
            out.push(Line::from(std::mem::take(&mut current)));
            len = 0;
        }
        if !current.is_empty() {
            current.push(' ');
            len += 1;
        }
        current.push_str(word);
        len += w;
    }
    if !current.is_empty() {
        out.push(Line::from(current));
    }
    if out.is_empty() {
        out.push(Line::from(""));
    }
    out
}

/// Body lines of the open dialog. `width` is the inner width available.
pub fn render_lines(app: &App, width: u16) -> Vec<Line<'static>> {
    let Some(dialog) = &app.dialog else {
        return Vec::new();
    };
    match dialog {
        Dialog::Confirm { message, .. } => {
            let mut lines = wrap_line(message, width);
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled(
                    "y",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" confirm · ", Style::default().fg(Color::Gray)),
                Span::styled(
                    "n",
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                ),
                Span::styled(" / ", Style::default().fg(Color::Gray)),
                Span::styled("Esc", Style::default().fg(Color::Gray)),
                Span::styled(" cancel", Style::default().fg(Color::Gray)),
            ]));
            lines
        }
        Dialog::Approval(pending) => approval_lines(pending, width),
        Dialog::Uart {
            field,
            status,
            error,
            ..
        } => {
            let mut lines = vec![
                Line::from("Set the bridge UART: baud,data,parity,stop,flow"),
                Line::from(""),
                Line::from(vec![
                    Span::styled("> ", Style::default().fg(Color::Cyan)),
                    Span::styled(
                        field.text.clone(),
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]),
                Line::from(""),
            ];
            if let Some(error) = error {
                lines.extend(wrap_line(error, width).into_iter().map(|line| {
                    Line::from(
                        line.spans
                            .into_iter()
                            .map(|span| span.style(Style::default().fg(Color::Red)))
                            .collect::<Vec<_>>(),
                    )
                }));
            }
            if let Some(status) = status {
                lines.extend(wrap_line(status, width).into_iter().map(|line| {
                    Line::from(
                        line.spans
                            .into_iter()
                            .map(|span| span.style(Style::default().fg(Color::LightGreen)))
                            .collect::<Vec<_>>(),
                    )
                }));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Enter query & set · Ctrl+S send · Esc close",
                Style::default().fg(Color::DarkGray),
            )));
            lines
        }
        Dialog::Settings(state) => super::agent_settings::render_lines(state, width),
        Dialog::Help => help_lines(width),
        Dialog::Notices => {
            if app.notices.log.is_empty() {
                vec![Line::from(Span::styled(
                    "No notices yet.",
                    Style::default().fg(Color::DarkGray),
                ))]
            } else {
                app.notices
                    .log
                    .iter()
                    .rev()
                    .take(200)
                    .rev()
                    .map(|(level, text)| {
                        let color = match level {
                            NoticeLevel::Info => Color::Gray,
                            NoticeLevel::Warn => Color::LightYellow,
                            NoticeLevel::Error => Color::LightRed,
                        };
                        Line::from(Span::styled(text.clone(), Style::default().fg(color)))
                    })
                    .collect()
            }
        }
    }
}

fn approval_lines(pending: &PendingApproval, width: u16) -> Vec<Line<'static>> {
    let request = &pending.request;
    let mut lines = Vec::new();
    match &request.kind {
        ApprovalKind::RunCommand { command, mode } => {
            lines.push(Line::from(Span::styled(
                "Run a command on the target?",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                command.clone(),
                Style::default().fg(Color::White),
            )));
            lines.push(Line::from(Span::styled(
                format!("mode: {}", super::status::mode_text(*mode)),
                Style::default().fg(Color::DarkGray),
            )));
        }
        ApprovalKind::SendInput { payload } => {
            lines.push(Line::from(Span::styled(
                "Send to the current target? (Control characters are escaped.)",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(""));
            for line in payload.lines() {
                lines.push(Line::from(Span::styled(
                    line.to_string(),
                    Style::default().fg(Color::White),
                )));
            }
        }
        ApprovalKind::AccessoryChange { summary, command } => {
            lines.push(Line::from(Span::styled(
                "This changes Linkr Bee itself (not the target), sent over the \
                 encrypted Bluetooth management channel:",
                Style::default().fg(Color::LightYellow),
            )));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                summary.clone(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(Span::styled(
                command.clone(),
                Style::default().fg(Color::Gray),
            )));
        }
    }
    if !request.question.is_empty() {
        lines.push(Line::from(""));
        lines.extend(wrap_line(&request.question, width));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(
            "y",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" approve · ", Style::default().fg(Color::Gray)),
        Span::styled(
            "n",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" reject · ", Style::default().fg(Color::Gray)),
        Span::styled("Esc", Style::default().fg(Color::Gray)),
        Span::styled(" reject", Style::default().fg(Color::Gray)),
    ]));
    lines
}

/// F1 overlay. Mirrors the global keys of CONTRACTS.md section 5.
fn help_lines(width: u16) -> Vec<Line<'static>> {
    struct Row(&'static str, &'static str);
    const ROWS: [Row; 21] = [
        Row("Ctrl+P", "Command palette (every action, searchable)"),
        Row("F1", "This help"),
        Row("F2", "Terminal view"),
        Row("F3", "Diagnostics view (@i?)"),
        Row("F4", "Network view (WiFi / WebDAV)"),
        Row("F5", "Assistant view"),
        Row("Ctrl+Shift+K", "Focus the assistant composer"),
        Row("Ctrl+L", "Clear the terminal pane"),
        Row("Ctrl+Q", "Quit (asks first while connected)"),
        Row("Ctrl+Up", "Focus the sidebar"),
        Row("Esc", "Close an overlay / back to the terminal"),
        Row("Shift+PgUp/PgDn", "Scroll the terminal scrollback"),
        Row("Shift+Home/End", "Scrollback: top / bottom"),
        Row("Ctrl+Shift+R", "Arm one-shot Shift for the next key"),
        Row("Ctrl+Shift+C", "Arm one-shot Ctrl for the next key"),
        Row("Ctrl+Shift+A", "Arm one-shot Alt for the next key"),
        Row("Enter", "Send a line (Enter mode applies)"),
        Row("Tab / Shift+Tab", "Sent to the target as TAB / CSI Z"),
        Row("F6..F12", "Sent to the target unchanged"),
        Row(
            "Ctrl+P → term.*",
            "Font size, autoscroll, echo, save log, copy",
        ),
        Row("Ctrl+P → app.*", "Notices, help, quit, focus switching"),
    ];
    let mut lines = vec![rule(width)];
    for Row(key, text) in ROWS {
        lines.push(Line::from(vec![
            Span::styled(format!("{key:<16}"), Style::default().fg(Color::Cyan)),
            Span::styled(text.to_string(), Style::default().fg(Color::Gray)),
        ]));
    }
    lines.push(rule(width));
    lines.push(Line::from(Span::styled(
        "Byte sequences match web/terminal_keys.js.",
        Style::default().fg(Color::DarkGray),
    )));
    lines
}

// --- keys --------------------------------------------------------------------

/// Handle a key while a dialog is open. Every key is consumed by the dialog
/// layer; nothing falls through to the view underneath.
pub fn handle_key(app: &mut App, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    // Quit stays reachable even from inside a dialog.
    if ctrl && key.code == KeyCode::Char('q') {
        request_quit(app);
        return;
    }
    // Informational overlays close on any of the usual dismissal keys.
    if matches!(app.dialog, Some(Dialog::Help) | Some(Dialog::Notices)) {
        if matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q')) {
            app.dialog = None;
        }
        return;
    }
    let Some(dialog) = app.dialog.take() else {
        return;
    };
    match dialog {
        Dialog::Confirm {
            kind,
            title,
            message,
        } => match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => confirm(app, kind),
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {}
            _ => {
                app.dialog = Some(Dialog::Confirm {
                    kind,
                    title,
                    message,
                })
            }
        },
        Dialog::Approval(pending) => match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                pending.resolve(ApprovalDecision::Approved);
                app.toast(NoticeLevel::Info, "Approved.");
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                pending.resolve(ApprovalDecision::Rejected);
                app.toast(NoticeLevel::Info, "Rejected.");
            }
            _ => app.dialog = Some(Dialog::Approval(pending)),
        },
        other => {
            app.dialog = Some(other);
            dialog_edit(app, key);
        }
    }
}

/// Route a key to the editable dialog (UART settings, AI configuration).
fn dialog_edit(app: &mut App, key: KeyEvent) {
    if key.code == KeyCode::Esc {
        app.dialog = None;
        return;
    }
    if matches!(app.dialog, Some(Dialog::Settings(_))) {
        super::agent_settings::handle_key(app, key);
        return;
    }
    uart_edit(app, key);
}

fn uart_edit(app: &mut App, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let apply = matches!(key.code, KeyCode::Enter) || (ctrl && key.code == KeyCode::Char('s'));
    if !apply {
        let Some(Dialog::Uart { field, error, .. }) = &mut app.dialog else {
            return;
        };
        *error = None;
        match key.code {
            KeyCode::Char(c) if !ctrl => field.insert_char(c),
            KeyCode::Backspace => field.backspace(),
            KeyCode::Delete => field.delete(),
            KeyCode::Left => field.left(),
            KeyCode::Right => field.right(),
            KeyCode::Home => field.home(),
            KeyCode::End => field.end(),
            _ => {}
        }
        return;
    }

    // Enter / Ctrl+S: validate the spec and push it to the bridge.
    let spec = match &app.dialog {
        Some(Dialog::Uart { field, .. }) => field.text.clone(),
        _ => return,
    };
    match crate::protocol::validate::normalize_uart_spec(&spec) {
        Ok(spec) => {
            let rx = app.session.request_mgmt(format!("@u={spec}"), None);
            if let Some(Dialog::Uart {
                status,
                error,
                reply,
                ..
            }) = &mut app.dialog
            {
                *status = Some(format!("setting uart={spec}…"));
                *error = None;
                *reply = Some(rx);
            }
        }
        Err(message) => {
            if let Some(Dialog::Uart { error, .. }) = &mut app.dialog {
                *error = Some(message);
            }
        }
    }
}

/// Drain the in-flight `@u?` / `@u=` reply of the UART dialog (once per tick).
pub fn poll(app: &mut App) {
    let received = match &mut app.dialog {
        Some(Dialog::Uart {
            reply: Some(rx), ..
        }) => match rx.try_recv() {
            Ok(result) => Some(Ok(result)),
            Err(oneshot::error::TryRecvError::Empty) => None,
            Err(err) => Some(Err(err)),
        },
        _ => None,
    };
    let Some(received) = received else { return };
    let Some(Dialog::Uart {
        field,
        status,
        error,
        reply,
    }) = &mut app.dialog
    else {
        return;
    };
    *reply = None;
    let outcome = match received {
        Ok(outcome) => outcome,
        Err(oneshot::error::TryRecvError::Closed) => {
            *error = Some("disconnected before the UART reply".to_string());
            return;
        }
        Err(_) => return,
    };
    match outcome {
        Ok(mgmt) => {
            let mut text = String::new();
            for line in mgmt.lines.iter().chain(mgmt.events.iter()) {
                text.push_str(line);
                text.push('\n');
            }
            match parse_uart_reply(&text) {
                Some(settings) => {
                    field.set(settings.to_string());
                    *status = Some(format!("OK uart={settings}"));
                    *error = None;
                }
                None if mgmt.ok => {
                    *status = Some(super::replies::redact_secrets(text.trim()).to_string());
                }
                None => {
                    *error = Some(super::replies::redact_secrets(text.trim()).to_string());
                }
            }
        }
        Err(message) => *error = Some(super::replies::redact_secrets(&message)),
    }
}

/// Open the UART dialog and query the current settings (`@u?`).
pub fn open_uart(app: &mut App) {
    if !app.ble_connected() {
        app.toast(
            NoticeLevel::Warn,
            "Connect over BLE to change the bridge UART.",
        );
        return;
    }
    let rx = app.session.request_mgmt("@u?".to_string(), None);
    app.dialog = Some(Dialog::Uart {
        field: TextField::new(DEFAULT_UART),
        status: Some("querying uart…".to_string()),
        error: None,
        reply: Some(rx),
    });
}

/// Run a confirmed action.
pub fn confirm(app: &mut App, kind: ConfirmKind) {
    match kind {
        ConfirmKind::Reboot => app.send_text("reboot\n"),
        ConfirmKind::Disconnect => app.session.disconnect(),
        ConfirmKind::Quit => app.quit = true,
    }
}

/// Ctrl+Q: quit right away when idle, ask first while connected
/// (`confirmQuit` parity of the web client).
pub fn request_quit(app: &mut App) {
    app.palette = None;
    if app.connected() {
        app.dialog = Some(Dialog::Confirm {
            kind: ConfirmKind::Quit,
            title: "Quit".to_string(),
            message: QUIT_CONFIRM.to_string(),
        });
    } else {
        app.quit = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirmation_strings_match_the_web_client() {
        assert_eq!(REBOOT_CONFIRM, "Reboot the connected device now?");
        assert_eq!(QUIT_CONFIRM, "Quit the TUI now?");
        assert_eq!(
            DISCONNECT_CONFIRM,
            "Disconnect from the current device now?"
        );
        assert_eq!(DEFAULT_UART, "115200,8,n,1,n");
    }

    #[test]
    fn help_lists_the_contract_global_keys() {
        let text: String = help_lines(80)
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        for key in [
            "Ctrl+P",
            "F1",
            "F2",
            "F3",
            "F4",
            "F5",
            "Ctrl+Shift+K",
            "Ctrl+L",
            "Ctrl+Q",
        ] {
            assert!(text.contains(key), "help overlay is missing {key}");
        }
    }

    #[test]
    fn approval_cards_cover_all_three_kinds() {
        let kinds = [
            ApprovalKind::RunCommand {
                command: "reboot".to_string(),
                mode: crate::agent::ExecMode::Auto,
            },
            ApprovalKind::SendInput {
                payload: "ls\n".to_string(),
            },
            ApprovalKind::AccessoryChange {
                summary: "set uart".to_string(),
                command: "@u=115200,8,n,1,n".to_string(),
            },
        ];
        for kind in kinds {
            let request = crate::agent::ApprovalRequest {
                id: 1,
                kind,
                question: String::new(),
            };
            let (tx, _rx) = oneshot::channel();
            let pending = PendingApproval { request, tx };
            let lines = approval_lines(&pending, 60);
            let text: String = lines
                .iter()
                .map(|line| {
                    line.spans
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n");
            assert!(text.contains("approve"), "missing approve hint:\n{text}");
            assert!(text.contains("reject"), "missing reject hint:\n{text}");
        }
    }
}
