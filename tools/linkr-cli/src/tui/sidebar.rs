//! Left sidebar: connection card, quick-send presets, watch findings and
//! section links (CONTRACTS.md section 5).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::dialogs::{ConfirmKind, Dialog};
use super::settings::TransportChoice;
use super::state::{App, Focus, TextField, View};
use crate::event::NoticeLevel;

/// Quick-send presets from WEB_UX_SPEC section 4 (in order, `reboot` is the
/// dangerous one and asks for confirmation first).
pub const PRESETS: [(&str, bool); 5] = [
    ("help", false),
    ("version", false),
    ("uname -a", false),
    ("df -h", false),
    ("reboot", true),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SideEntry {
    Nav(View),
    ToggleTransport,
    Connect,
    Disconnect,
    SwitchDevice,
    BleName,
    LanHost,
    LanToken,
    Preset(usize),
    Finding(usize),
}

#[derive(Default)]
pub struct SidebarState {
    pub selection: usize,
    pub ble_name: TextField,
    pub lan_host: TextField,
    pub lan_token: TextField,
}

impl SidebarState {
    pub fn new(ble_name: String, lan_host: String) -> Self {
        Self {
            selection: 0,
            ble_name: TextField::new(ble_name),
            lan_host: TextField::new(lan_host),
            lan_token: TextField::new(""),
        }
    }

    fn select(&mut self, next: usize, len: usize) {
        if len == 0 {
            self.selection = 0;
        } else {
            self.selection = next.min(len - 1);
        }
    }
}

/// The flat list of selectable entries, in render order.
pub fn entries(app: &App) -> Vec<SideEntry> {
    let mut list = Vec::new();
    for view in [
        View::Terminal,
        View::Diagnostics,
        View::Network,
        View::Assistant,
    ] {
        list.push(SideEntry::Nav(view));
    }
    list.push(SideEntry::ToggleTransport);
    let lan = app.transport_choice() == TransportChoice::Lan;
    if !lan {
        list.push(SideEntry::BleName);
    }
    if app.connected() {
        list.push(SideEntry::Disconnect);
    } else {
        list.push(SideEntry::Connect);
        if !lan {
            list.push(SideEntry::SwitchDevice);
        }
    }
    if lan {
        list.push(SideEntry::LanHost);
        list.push(SideEntry::LanToken);
    }
    for i in 0..PRESETS.len() {
        list.push(SideEntry::Preset(i));
    }
    if app.watch_ok {
        for i in 0..app.watch.findings().len() {
            list.push(SideEntry::Finding(i));
        }
    }
    list
}

fn header(text: &str) -> Line<'static> {
    Line::from(Span::styled(
        text.to_string(),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ))
}

fn hint(text: &str) -> Line<'static> {
    Line::from(Span::styled(
        text.to_string(),
        Style::default().fg(Color::DarkGray),
    ))
}

/// Build the sidebar body. The selected entry gets the `▸ ` marker and bold.
pub fn render_lines(app: &App) -> Vec<Line<'static>> {
    let items = entries(app);
    let selected = app.sidebar.selection;
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut cursor = 0usize;

    macro_rules! item {
        ($entry:expr, $label:expr, $style:expr) => {{
            let is_sel = items.get(cursor) == Some(&$entry) && cursor == selected;
            let marker = if is_sel { "▸ " } else { "  " };
            let style = if is_sel {
                $style.add_modifier(Modifier::BOLD).fg(Color::Yellow)
            } else {
                $style
            };
            lines.push(Line::from(vec![
                Span::styled(marker, Style::default().fg(Color::Yellow)),
                Span::styled($label.to_string(), style),
            ]));
            cursor += 1;
        }};
    }

    lines.push(header("Connection"));
    let dot = match app.state {
        crate::event::ConnectionState::Connected => "●",
        crate::event::ConnectionState::Connecting => "◐",
        crate::event::ConnectionState::Failed => "◆",
        crate::event::ConnectionState::Disconnected => "○",
    };
    let dot_color = match app.state {
        crate::event::ConnectionState::Connected => Color::Green,
        crate::event::ConnectionState::Connecting => Color::Yellow,
        crate::event::ConnectionState::Failed => Color::Red,
        crate::event::ConnectionState::Disconnected => Color::DarkGray,
    };
    lines.push(Line::from(vec![
        Span::styled(format!("{dot} "), Style::default().fg(dot_color)),
        Span::raw(format!(
            "{} · {}",
            super::status::state_text(app.state),
            super::status::transport_text(app.info.kind)
        )),
    ]));
    if !app.info.label.is_empty() {
        lines.push(hint(&app.info.label));
    }

    let transport_style = Style::default().fg(Color::Gray);
    item!(
        SideEntry::ToggleTransport,
        format!(
            "Transport: {} {}",
            app.settings.transport.label(),
            if app.connected() {
                "(locked)"
            } else {
                "◂▸"
            }
        ),
        transport_style
    );

    let field_style = Style::default().fg(Color::White);
    if app.transport_choice() != TransportChoice::Lan {
        item!(
            SideEntry::BleName,
            format!("Device: {}", display_field(&app.sidebar.ble_name, false)),
            field_style
        );
        if app.sidebar.ble_name.text.is_empty() {
            lines.push(hint("   (empty name matches any Linkr device)"));
        }
    }

    if app.connected() {
        item!(
            SideEntry::Disconnect,
            "Disconnect",
            Style::default().fg(Color::Red)
        );
    } else {
        item!(
            SideEntry::Connect,
            "Connect",
            Style::default().fg(Color::Green)
        );
        if app.transport_choice() != TransportChoice::Lan {
            item!(
                SideEntry::SwitchDevice,
                "Switch device",
                Style::default().fg(Color::Gray)
            );
        }
    }

    if app.transport_choice() == TransportChoice::Lan {
        item!(
            SideEntry::LanHost,
            format!("LAN host: {}", display_field(&app.sidebar.lan_host, false)),
            field_style
        );
        item!(
            SideEntry::LanToken,
            format!("LAN token: {}", display_field(&app.sidebar.lan_token, true)),
            field_style
        );
        lines.push(hint("   token: 32 hex, blank when LAN auth is off"));
    }
    lines.push(Line::from(""));

    lines.push(header("Quick send"));
    lines.push(hint("Enter sends · reboot asks first"));
    for (index, (cmd, danger)) in PRESETS.iter().enumerate() {
        let style = if *danger {
            Style::default().fg(Color::LightRed)
        } else if app.connected() {
            Style::default().fg(Color::Gray)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let suffix = if *danger { " ⚠" } else { "" };
        item!(SideEntry::Preset(index), format!("{cmd}{suffix}"), style);
    }
    lines.push(Line::from(""));

    lines.push(header("Watch"));
    if !app.watch_ok {
        lines.push(hint("   watch engine not ready"));
    } else if app.watch.findings().is_empty() {
        lines.push(hint("   no findings yet"));
    } else {
        for (index, finding) in app.watch.findings().iter().enumerate() {
            let style = match finding.kind {
                crate::watch::FindingKind::Panic => Style::default().fg(Color::LightRed),
                crate::watch::FindingKind::BootLoop => Style::default().fg(Color::LightYellow),
                crate::watch::FindingKind::Pattern => Style::default().fg(Color::LightMagenta),
            };
            let label = format!("{} L{} {}", finding.label, finding.line, finding.count);
            item!(SideEntry::Finding(index), label, style);
        }
    }

    lines
}

fn display_field(field: &TextField, masked: bool) -> String {
    let (text, _) = field.display(if masked { Some('*') } else { None });
    if text.is_empty() {
        "–".to_string()
    } else {
        text
    }
}

/// Sidebar keys: Up/Down/Home/End move, Enter activates, typing edits fields.
pub fn handle_key(app: &mut App, key: KeyEvent) {
    let items = entries(app);
    let len = items.len();
    if len == 0 {
        return;
    }
    match key.code {
        KeyCode::Up => {
            app.sidebar.selection = app.sidebar.selection.saturating_sub(1);
            return;
        }
        KeyCode::Down => {
            let next = app.sidebar.selection + 1;
            app.sidebar.select(next, len);
            return;
        }
        KeyCode::Tab => {
            let next = app.sidebar.selection + 1;
            app.sidebar.select(next, len);
            return;
        }
        KeyCode::BackTab => {
            app.sidebar.selection = app.sidebar.selection.saturating_sub(1);
            return;
        }
        KeyCode::Home => {
            app.sidebar.selection = 0;
            return;
        }
        KeyCode::End => {
            app.sidebar.selection = len - 1;
            return;
        }
        KeyCode::Esc => {
            app.focus = Focus::Center;
            return;
        }
        KeyCode::Enter if app.sidebar.selection < len => {
            let entry = items[app.sidebar.selection];
            activate(app, entry);
            return;
        }
        _ => {}
    }

    // Text editing for the selected field entry.
    let Some(entry) = items.get(app.sidebar.selection).copied() else {
        return;
    };
    match entry {
        SideEntry::BleName => edit_field(&mut app.sidebar.ble_name, key),
        SideEntry::LanHost => edit_field(&mut app.sidebar.lan_host, key),
        SideEntry::LanToken => edit_field(&mut app.sidebar.lan_token, key),
        _ => return,
    }
    persist_connection_fields(app);
}

fn edit_field(field: &mut TextField, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
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
}

fn persist_connection_fields(app: &mut App) {
    if !matches!(app.settings.transport, TransportChoice::Lan) {
        // The BLE name is not part of tui.json (the session remembers its own
        // device); LAN host is.
        return;
    }
    let host = app.sidebar.lan_host.text.trim().to_string();
    if app.settings.last_lan_host != host {
        app.settings.last_lan_host = host;
        if let Err(err) = super::settings::save(&app.settings) {
            app.toast(NoticeLevel::Warn, format!("Could not save settings: {err}"));
        }
    }
}

fn activate(app: &mut App, entry: SideEntry) {
    match entry {
        SideEntry::Nav(view) => app.set_view(view),
        SideEntry::ToggleTransport => {
            if app.connected() {
                app.toast(
                    NoticeLevel::Warn,
                    "Disconnect before switching the transport.",
                );
            } else {
                app.settings.transport = match app.settings.transport {
                    TransportChoice::Ble => TransportChoice::Lan,
                    TransportChoice::Lan => TransportChoice::Ble,
                };
                if let Err(err) = super::settings::save(&app.settings) {
                    app.toast(NoticeLevel::Warn, format!("Could not save settings: {err}"));
                }
            }
        }
        SideEntry::Connect => super::connect::connect(app),
        SideEntry::Disconnect => {
            app.dialog = Some(Dialog::Confirm {
                kind: ConfirmKind::Disconnect,
                title: "Disconnect".to_string(),
                message: "Disconnect from the current device now?".to_string(),
            });
        }
        SideEntry::SwitchDevice => {
            // Focus the device name field for editing.
            let items = entries(app);
            if let Some(pos) = items.iter().position(|e| *e == SideEntry::BleName) {
                app.sidebar.selection = pos;
            } else {
                app.toast(NoticeLevel::Info, "Switch the transport to BLE first.");
            }
        }
        SideEntry::BleName | SideEntry::LanHost | SideEntry::LanToken => {}
        SideEntry::Preset(index) => {
            let (cmd, danger) = PRESETS[index];
            if danger {
                app.dialog = Some(Dialog::Confirm {
                    kind: ConfirmKind::Reboot,
                    title: "Reboot device".to_string(),
                    message: "Reboot the connected device now?".to_string(),
                });
            } else {
                app.send_text(&format!("{cmd}\n"));
            }
        }
        SideEntry::Finding(index) => {
            if let Some(finding) = app.watch.findings().get(index) {
                let text = crate::watch::SerialWatch::describe(finding, "en");
                let text = if text.is_empty() {
                    format!("{}: {}", finding.label, finding.evidence)
                } else {
                    text
                };
                app.toast(NoticeLevel::Warn, text);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_match_the_web_client() {
        let cmds: Vec<&str> = PRESETS.iter().map(|(c, _)| *c).collect();
        assert_eq!(cmds, vec!["help", "version", "uname -a", "df -h", "reboot"]);
        // Exactly the reboot preset asks for confirmation.
        let danger: Vec<&str> = PRESETS
            .iter()
            .filter(|(_, d)| *d)
            .map(|(c, _)| *c)
            .collect();
        assert_eq!(danger, vec!["reboot"]);
    }

    #[test]
    fn confirmation_text_matches_the_web_client() {
        assert_eq!("Reboot the connected device now?", REBOOT_CONFIRM);
    }

    use super::super::dialogs::REBOOT_CONFIRM;
}
