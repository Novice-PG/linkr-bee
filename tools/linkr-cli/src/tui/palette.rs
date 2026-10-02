//! Command palette (`Ctrl+P`) — every action of the TUI in one searchable
//! list (CONTRACTS.md section 5).
//!
//! The registry is a plain `&'static [Action]` so a unit test can assert its
//! completeness against the canonical id list without building an `App`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::settings::TransportChoice;
use super::state::{App, Focus, View};
use crate::event::NoticeLevel;

/// One palette entry. `run` is a plain function so the table stays `const`.
pub struct Action {
    pub id: &'static str,
    pub title: &'static str,
    pub category: &'static str,
    pub shortcut: &'static str,
    pub run: fn(&mut App),
}

// --- action implementations --------------------------------------------------

fn focus_sidebar(app: &mut App) {
    app.focus = Focus::Sidebar;
}

fn view_terminal(app: &mut App) {
    app.set_view(View::Terminal);
}

fn view_diagnostics(app: &mut App) {
    app.set_view(View::Diagnostics);
}

fn view_network(app: &mut App) {
    app.set_view(View::Network);
}

fn view_assistant(app: &mut App) {
    app.set_view(View::Assistant);
}

fn focus_center(app: &mut App) {
    app.focus = Focus::Center;
}

fn focus_assistant(app: &mut App) {
    app.set_view(View::Assistant);
    app.focus = Focus::Assistant;
}

fn font_bigger(app: &mut App) {
    let next = (app.settings.font_size + 1).clamp(
        super::settings::MIN_FONT_SIZE,
        super::settings::MAX_FONT_SIZE,
    );
    if next != app.settings.font_size {
        app.settings.font_size = next;
        persist(app);
        app.force_redraw = true;
    }
}

fn font_smaller(app: &mut App) {
    let next = app.settings.font_size.saturating_sub(1).clamp(
        super::settings::MIN_FONT_SIZE,
        super::settings::MAX_FONT_SIZE,
    );
    if next != app.settings.font_size {
        app.settings.font_size = next;
        persist(app);
        app.force_redraw = true;
    }
}

fn font_reset(app: &mut App) {
    app.settings.font_size = super::settings::DEFAULT_FONT_SIZE;
    persist(app);
    app.force_redraw = true;
}

fn toggle_autoscroll(app: &mut App) {
    let on = !app.terminal.autoscroll;
    app.terminal.set_autoscroll(on);
    app.settings.autoscroll = on;
    persist(app);
}

fn toggle_echo(app: &mut App) {
    app.settings.local_echo = !app.settings.local_echo;
    persist(app);
}

fn cycle_enter(app: &mut App) {
    app.settings.enter_mode = app.settings.enter_mode.next();
    persist(app);
    app.toast(
        NoticeLevel::Info,
        format!("Enter mode: {}", app.settings.enter_mode.label()),
    );
}

fn clear_terminal(app: &mut App) {
    app.terminal.clear();
}

fn save_log(app: &mut App) {
    let name = super::terminal_view::TerminalPane::default_log_name();
    let path = std::path::PathBuf::from(&name);
    match app.terminal.save_log(&path) {
        Ok(bytes) => app
            .notices
            .push(NoticeLevel::Info, format!("Saved {bytes} bytes to {name}")),
        Err(err) => app
            .notices
            .push(NoticeLevel::Error, format!("Save failed: {err}")),
    }
}

fn copy_visible(app: &mut App) {
    let text = app.terminal.visible_text();
    // OSC 52 is addressed to the *host* terminal emulator, so it goes to
    // stdout (the grid records it too through the VT parser).
    let payload = super::terminal_view::osc52_write(&text);
    use std::io::Write as _;
    let mut out = std::io::stdout();
    let outcome = out.write_all(payload.as_bytes()).and_then(|()| out.flush());
    match outcome {
        Ok(()) => app.toast(
            NoticeLevel::Info,
            format!("Copied {} characters (OSC 52).", text.chars().count()),
        ),
        Err(err) => app
            .notices
            .push(NoticeLevel::Error, format!("Copy failed: {err}")),
    }
}

fn toggle_transport(app: &mut App) {
    if app.connected() {
        app.toast(
            NoticeLevel::Warn,
            "Disconnect before switching the transport.",
        );
        return;
    }
    app.settings.transport = match app.settings.transport {
        TransportChoice::Ble => TransportChoice::Lan,
        TransportChoice::Lan => TransportChoice::Ble,
    };
    persist(app);
}

fn connect(app: &mut App) {
    super::connect::connect(app);
}

fn disconnect(app: &mut App) {
    app.session.disconnect();
}

fn open_uart(app: &mut App) {
    super::dialogs::open_uart(app);
}

fn refresh_diagnostics(app: &mut App) {
    app.set_view(View::Diagnostics);
    if app.ble_connected() {
        let session = app.session.clone();
        app.diagnostics.refresh(&session);
    } else {
        app.toast(NoticeLevel::Warn, "Connect over BLE to read diagnostics.");
    }
}

fn wifi_scan(app: &mut App) {
    app.set_view(View::Network);
    super::network_view::action(app, super::network_view::NetEntry::Scan);
}

fn wifi_status(app: &mut App) {
    app.set_view(View::Network);
    super::network_view::action(app, super::network_view::NetEntry::WifiStatus);
}

fn webdav_status(app: &mut App) {
    app.set_view(View::Network);
    super::network_view::action(app, super::network_view::NetEntry::WebdavStatus);
}

fn ask_assistant(app: &mut App) {
    focus_assistant(app);
}

fn cycle_mode(app: &mut App) {
    let next = match app.exec_mode {
        crate::agent::ExecMode::Manual => crate::agent::ExecMode::Auto,
        crate::agent::ExecMode::Auto => crate::agent::ExecMode::FullAuto,
        crate::agent::ExecMode::FullAuto => crate::agent::ExecMode::Manual,
    };
    super::assistant_view::set_mode(app, next);
}

fn agent_settings(app: &mut App) {
    super::assistant_view::open_settings(app);
}

fn new_chat(app: &mut App) {
    app.assistant = super::assistant_view::AssistantState::default();
    app.assistant.status = "New chat.".to_string();
}

fn stop_agent(app: &mut App) {
    if let Some(agent) = &app.agent {
        agent.handle.stop();
    }
    if app.assistant.busy {
        app.assistant.busy = false;
        app.assistant
            .entries
            .push(super::assistant_view::Entry::System(
                super::assistant_view::STOP_MESSAGE.to_string(),
            ));
    }
}

fn export_report(app: &mut App) {
    // `agent::build_report` (workstream A) is not exported yet; the action
    // stays discoverable and says so instead of silently doing nothing.
    app.toast(
        NoticeLevel::Warn,
        "Report export needs agent::build_report, which this build does not export yet.",
    );
}

fn show_help(app: &mut App) {
    app.dialog = Some(super::dialogs::Dialog::Help);
}

fn show_notices(app: &mut App) {
    app.dialog = Some(super::dialogs::Dialog::Notices);
}

fn quit(app: &mut App) {
    super::dialogs::request_quit(app);
}

fn persist(app: &mut App) {
    if let Err(err) = super::settings::save(&app.settings) {
        app.notices
            .push(NoticeLevel::Warn, format!("Could not save settings: {err}"));
    }
}

/// The canonical registry, grouped for display. Order = render order.
pub const ACTIONS: &[Action] = &[
    // Views
    Action {
        id: "view.terminal",
        title: "Open terminal view",
        category: "View",
        shortcut: "F2",
        run: view_terminal,
    },
    Action {
        id: "view.diagnostics",
        title: "Open diagnostics view",
        category: "View",
        shortcut: "F3",
        run: view_diagnostics,
    },
    Action {
        id: "view.network",
        title: "Open network view",
        category: "View",
        shortcut: "F4",
        run: view_network,
    },
    Action {
        id: "view.assistant",
        title: "Open assistant view",
        category: "View",
        shortcut: "F5",
        run: view_assistant,
    },
    // Focus
    Action {
        id: "focus.sidebar",
        title: "Focus the sidebar",
        category: "Focus",
        shortcut: "Ctrl+Up",
        run: focus_sidebar,
    },
    Action {
        id: "focus.terminal",
        title: "Focus the terminal",
        category: "Focus",
        shortcut: "Esc",
        run: focus_center,
    },
    Action {
        id: "focus.assistant",
        title: "Focus the assistant composer",
        category: "Focus",
        shortcut: "Ctrl+Shift+K",
        run: focus_assistant,
    },
    // Connection
    Action {
        id: "connect",
        title: "Connect",
        category: "Connection",
        shortcut: "",
        run: connect,
    },
    Action {
        id: "disconnect",
        title: "Disconnect",
        category: "Connection",
        shortcut: "",
        run: disconnect,
    },
    Action {
        id: "transport.toggle",
        title: "Toggle BLE / LAN transport",
        category: "Connection",
        shortcut: "",
        run: toggle_transport,
    },
    Action {
        id: "uart.settings",
        title: "UART settings…",
        category: "Connection",
        shortcut: "",
        run: open_uart,
    },
    // Terminal
    Action {
        id: "term.font_bigger",
        title: "Bigger font",
        category: "Terminal",
        shortcut: "Ctrl+=",
        run: font_bigger,
    },
    Action {
        id: "term.font_smaller",
        title: "Smaller font",
        category: "Terminal",
        shortcut: "Ctrl+-",
        run: font_smaller,
    },
    Action {
        id: "term.font_reset",
        title: "Reset font size",
        category: "Terminal",
        shortcut: "Ctrl+0",
        run: font_reset,
    },
    Action {
        id: "term.autoscroll",
        title: "Toggle autoscroll",
        category: "Terminal",
        shortcut: "",
        run: toggle_autoscroll,
    },
    Action {
        id: "term.echo",
        title: "Toggle local echo",
        category: "Terminal",
        shortcut: "",
        run: toggle_echo,
    },
    Action {
        id: "term.enter_mode",
        title: "Cycle Enter mode",
        category: "Terminal",
        shortcut: "",
        run: cycle_enter,
    },
    Action {
        id: "term.clear",
        title: "Clear the terminal",
        category: "Terminal",
        shortcut: "Ctrl+L",
        run: clear_terminal,
    },
    Action {
        id: "term.save_log",
        title: "Save log to file",
        category: "Terminal",
        shortcut: "",
        run: save_log,
    },
    Action {
        id: "term.copy",
        title: "Copy visible output (OSC 52)",
        category: "Terminal",
        shortcut: "",
        run: copy_visible,
    },
    // Diagnostics / network
    Action {
        id: "diag.refresh",
        title: "Refresh diagnostics (@i?)",
        category: "Diagnostics",
        shortcut: "",
        run: refresh_diagnostics,
    },
    Action {
        id: "wifi.scan",
        title: "Scan WiFi networks",
        category: "Network",
        shortcut: "",
        run: wifi_scan,
    },
    Action {
        id: "wifi.status",
        title: "Query WiFi status",
        category: "Network",
        shortcut: "",
        run: wifi_status,
    },
    Action {
        id: "webdav.status",
        title: "Query WebDAV status",
        category: "Network",
        shortcut: "",
        run: webdav_status,
    },
    // Assistant
    Action {
        id: "agent.ask",
        title: "Ask the assistant",
        category: "Assistant",
        shortcut: "",
        run: ask_assistant,
    },
    Action {
        id: "agent.mode",
        title: "Cycle execution mode",
        category: "Assistant",
        shortcut: "",
        run: cycle_mode,
    },
    Action {
        id: "agent.settings",
        title: "AI configuration…",
        category: "Assistant",
        shortcut: "",
        run: agent_settings,
    },
    Action {
        id: "agent.new_chat",
        title: "New chat",
        category: "Assistant",
        shortcut: "",
        run: new_chat,
    },
    Action {
        id: "agent.stop",
        title: "Stop the running turn",
        category: "Assistant",
        shortcut: "",
        run: stop_agent,
    },
    Action {
        id: "agent.export",
        title: "Export report",
        category: "Assistant",
        shortcut: "",
        run: export_report,
    },
    // App
    Action {
        id: "app.help",
        title: "Keyboard help",
        category: "App",
        shortcut: "F1",
        run: show_help,
    },
    Action {
        id: "app.notices",
        title: "Notice log",
        category: "App",
        shortcut: "",
        run: show_notices,
    },
    Action {
        id: "app.quit",
        title: "Quit the TUI",
        category: "App",
        shortcut: "Ctrl+Q",
        run: quit,
    },
];

/// Open state of the palette.
#[derive(Default)]
pub struct PaletteState {
    pub query: String,
    pub selected: usize,
}

/// Indices of the actions matching `query` (id / title / category substring).
pub fn matches(query: &str) -> Vec<usize> {
    let needle = query.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return (0..ACTIONS.len()).collect();
    }
    ACTIONS
        .iter()
        .enumerate()
        .filter(|(_, action)| {
            action.id.to_ascii_lowercase().contains(&needle)
                || action.title.to_ascii_lowercase().contains(&needle)
                || action.category.to_ascii_lowercase().contains(&needle)
        })
        .map(|(index, _)| index)
        .collect()
}

/// Body lines of the palette overlay.
pub fn render_lines(app: &App) -> Vec<Line<'static>> {
    let Some(state) = &app.palette else {
        return Vec::new();
    };
    let found = matches(&state.query);
    let mut lines = vec![Line::from(vec![
        Span::styled(
            "› ",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            state.query.clone(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ])];
    lines.push(Line::from(Span::styled(
        "─".repeat(56),
        Style::default().fg(Color::DarkGray),
    )));
    if found.is_empty() {
        lines.push(Line::from(Span::styled(
            "No matching action.",
            Style::default().fg(Color::DarkGray),
        )));
        return lines;
    }
    let start = state.selected.saturating_sub(9);
    for (row, index) in found.iter().skip(start).take(18).enumerate() {
        let action = &ACTIONS[*index];
        let selected = start + row == state.selected;
        lines.push(Line::from(vec![
            Span::styled(
                if selected { "▸ " } else { "  " },
                Style::default().fg(Color::Yellow),
            ),
            Span::styled(
                format!("{:<28}", action.title),
                Style::default()
                    .fg(if selected {
                        Color::Yellow
                    } else {
                        Color::White
                    })
                    .add_modifier(if selected {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            ),
            Span::styled(
                format!("{:<12}", action.category),
                Style::default().fg(Color::Cyan),
            ),
            Span::styled(action.id.to_string(), Style::default().fg(Color::DarkGray)),
            Span::styled(
                if action.shortcut.is_empty() {
                    String::new()
                } else {
                    format!("  {}", action.shortcut)
                },
                Style::default().fg(Color::Magenta),
            ),
        ]));
    }
    lines
}

/// Keys while the palette is open. Returns `true` when consumed.
pub fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    let Some(state) = app.palette.as_mut() else {
        return false;
    };
    let found = matches(&state.query);
    match key.code {
        KeyCode::Esc => {
            app.palette = None;
        }
        KeyCode::Enter => {
            let picked = found.get(state.selected).copied();
            app.palette = None;
            if let Some(index) = picked {
                (ACTIONS[index].run)(app);
            }
        }
        KeyCode::Up | KeyCode::BackTab => {
            state.selected = state.selected.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Tab => {
            let last = found.len().saturating_sub(1);
            state.selected = (state.selected + 1).min(last);
        }
        KeyCode::Home => state.selected = 0,
        KeyCode::End => state.selected = found.len().saturating_sub(1),
        KeyCode::Backspace => {
            state.query.pop();
            state.selected = 0;
        }
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.query.push(c);
            state.selected = 0;
        }
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.query.clear();
            state.selected = 0;
        }
        _ => {}
    }
    // Re-clamp after the query changed.
    if let Some(state) = app.palette.as_mut() {
        let last = matches(&state.query).len().saturating_sub(1);
        state.selected = state.selected.min(last);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The canonical id list of CONTRACTS.md section 5 ("palette lists every
    /// action"). The registry must match it exactly, in order.
    const CANONICAL: &[&str] = &[
        "view.terminal",
        "view.diagnostics",
        "view.network",
        "view.assistant",
        "focus.sidebar",
        "focus.terminal",
        "focus.assistant",
        "connect",
        "disconnect",
        "transport.toggle",
        "uart.settings",
        "term.font_bigger",
        "term.font_smaller",
        "term.font_reset",
        "term.autoscroll",
        "term.echo",
        "term.enter_mode",
        "term.clear",
        "term.save_log",
        "term.copy",
        "diag.refresh",
        "wifi.scan",
        "wifi.status",
        "webdav.status",
        "agent.ask",
        "agent.mode",
        "agent.settings",
        "agent.new_chat",
        "agent.stop",
        "agent.export",
        "app.help",
        "app.notices",
        "app.quit",
    ];

    #[test]
    fn registry_is_complete_and_ordered() {
        let ids: Vec<&str> = ACTIONS.iter().map(|action| action.id).collect();
        assert_eq!(ids, CANONICAL, "palette registry drifted from the contract");
    }

    #[test]
    fn every_action_is_labelled() {
        for action in ACTIONS {
            assert!(!action.title.is_empty(), "{} has no title", action.id);
            assert!(!action.category.is_empty(), "{} has no category", action.id);
        }
    }

    #[test]
    fn ids_are_unique() {
        for (index, action) in ACTIONS.iter().enumerate() {
            let count = ACTIONS.iter().filter(|a| a.id == action.id).count();
            assert_eq!(count, 1, "duplicate id {}", action.id);
            let _ = index;
        }
    }

    #[test]
    fn required_contract_actions_exist() {
        for id in [
            "connect",
            "uart.settings",
            "wifi.scan",
            "term.save_log",
            "view.network",
            "term.autoscroll",
            "agent.ask",
            "app.quit",
        ] {
            assert!(
                ACTIONS.iter().any(|action| action.id == id),
                "palette is missing {id}"
            );
        }
    }

    #[test]
    fn search_matches_id_title_and_category() {
        let found = matches("save");
        let ids: Vec<&str> = found.iter().map(|i| ACTIONS[*i].id).collect();
        assert!(ids.contains(&"term.save_log"), "{ids:?}");

        // Substring match across id, title and category (lowercased).
        let found = matches("assistant");
        assert!(!found.is_empty());
        let ids: Vec<&str> = found.iter().map(|i| ACTIONS[*i].id).collect();
        assert!(ids.contains(&"agent.ask"), "{ids:?}");
        assert!(ids.contains(&"view.assistant"), "{ids:?}");
        assert!(ids.contains(&"focus.assistant"), "{ids:?}");
        for action in ACTIONS.iter().filter(|a| a.category == "Assistant") {
            assert!(
                ids.contains(&action.id),
                "{} not found for \"assistant\"",
                action.id
            );
        }

        let found = matches("wifi.scan");
        assert_eq!(
            found,
            vec![ACTIONS.iter().position(|a| a.id == "wifi.scan").unwrap()]
        );

        assert!(matches("nothing-matches-this").is_empty());
        assert_eq!(matches("").len(), ACTIONS.len());
    }
}
