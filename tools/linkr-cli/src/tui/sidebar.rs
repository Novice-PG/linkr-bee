//! Left sidebar: connection card, quick-send presets, watch findings and
//! section links (CONTRACTS.md section 5).

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::dialogs::{ConfirmKind, Dialog, DLG_NOT_SENT};
use super::i18n::{strings, t, tr, MSG_SAVE_SETTINGS};
use super::settings::TransportChoice;
use super::state::{App, Focus, TextField, View};
use crate::event::NoticeLevel;
use crate::lan_token_store::TokenStore;

/// How long the host field has to sit still before its edit reaches disk.
///
/// The web keeps settings in `localStorage`, where a write is free — here one
/// keystroke used to mean one `TokenStore::load()` (432 B) **plus** one full
/// rewrite of `tui.json`: 482 B, two write syscalls and 2.1 ms per character,
/// so pasting a 3000-character host stalled the interface for six seconds.
/// The value still reaches `app.settings` immediately (the dial path and every
/// other settings write read it from there); only the file waits, and
/// [`flush`] runs the pending write before a dial and on teardown.
pub const HOST_PERSIST_SETTLE: Duration = Duration::from_millis(400);

/// Quick-send presets from WEB_UX_SPEC section 4 (in order, `reboot` is the
/// dangerous one and asks for confirmation first).
pub const PRESETS: [(&str, bool); 5] = [
    ("help", false),
    ("version", false),
    ("uname -a", false),
    ("df -h", false),
    ("reboot", true),
];

strings! {
    SIDE_CONNECTION => "Connection", "连接";
    SIDE_TRANSPORT => "Transport: {} {}", "传输方式：{} {}";
    SIDE_TRANSPORT_LOCKED => "(locked)", "（已锁定）";
    SIDE_DEVICE => "Device: {}", "设备：{}";
    SIDE_EMPTY_NAME => "   (empty name matches any Linkr device)",
        "   （名称留空则匹配任意 Linkr 设备）";
    // First open has nothing to show in the host row: without this the field
    // was simply blank, while the web already carries a placeholder telling
    // you what shape of value it wants (`192.168.1.50 or ws://host/ws`).
    SIDE_EMPTY_HOST => "   (the bridge's address, e.g. 192.168.1.50 or ws://host/ws)",
        "   （桥接的地址，例 192.168.1.50 或 ws://host/ws）";
    SIDE_DISCONNECT => "Disconnect", "断开连接";
    SIDE_CONNECT => "Connect", "连接";
    SIDE_SWITCH_DEVICE => "Switch device", "切换设备";
    SIDE_LAN_HOST => "LAN host: {}", "局域网主机：{}";
    SIDE_LAN_TOKEN => "LAN token: {}", "局域网令牌：{}";
    SIDE_TOKEN_HINT => "   token: 32 hex, blank when LAN auth is off",
        "   令牌：32 位十六进制，局域网鉴权关闭时留空";
    SIDE_VIEWS => "Views", "视图";
    SIDE_QUICK_SEND => "Quick send", "快捷发送";
    SIDE_QUICK_HINT => "Enter sends · reboot asks first", "Enter 发送 · reboot 先确认";
    SIDE_WATCH => "Watch", "监控";
    SIDE_WATCH_NOT_READY => "   watch engine not ready", "   监控引擎未就绪";
    SIDE_WATCH_EMPTY => "   no findings yet", "   暂无发现";
    MSG_TRANSPORT_LOCKED => "Finish the connection attempt, or disconnect, before switching the transport.",
        "等连接尝试结束（或断开连接）再切换传输方式。";
    MSG_SWITCH_BLE => "Switch the transport to BLE first.", "请先把传输方式切换到 BLE。";
    CONFIRM_DISCONNECT_TITLE => "Disconnect", "断开连接";
    CONFIRM_DISCONNECT_MSG => "Disconnect from the current device now?",
        "立即断开与当前设备的连接？";
    CONFIRM_REBOOT_TITLE => "Reboot device", "重启设备";
    CONFIRM_REBOOT_MSG => "Reboot the connected device now?", "立即重启已连接的设备？";
}

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
    /// Set when the host was edited and the store read plus the `tui.json`
    /// write are still pending; [`poll`] runs them once typing pauses.
    pub host_persist_since: Option<Instant>,
    /// The token field as that pending host edit found it. [`flush`] may
    /// re-select a host's token only while the field still holds this — a
    /// token typed or pasted inside the settle window belongs to the user,
    /// and overwriting it would dial (and save) the store's older value
    /// instead of the one just entered.
    pub token_at_host_edit: String,
}

impl SidebarState {
    pub fn new(ble_name: String, lan_host: String) -> Self {
        Self {
            selection: 0,
            ble_name: TextField::new(ble_name),
            lan_host: TextField::new(lan_host),
            lan_token: TextField::new(""),
            host_persist_since: None,
            token_at_host_edit: String::new(),
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

/// The flat list of selectable entries, **in the order `render_lines` draws
/// them**: ↑/↓ walks this list while the eye reads the sidebar, so a single
/// entry drawn out of order shifts every marker below it (the section links
/// used to be selectable without being drawn, which hid the `▸` marker
/// entirely). A test walks both in lockstep to keep them in step.
pub fn entries(app: &App) -> Vec<SideEntry> {
    let mut list = Vec::new();
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
    // Section links (CONTRACTS.md section 5), drawn between the connection
    // card and the quick-send block.
    for view in [
        View::Terminal,
        View::Diagnostics,
        View::Network,
        View::Assistant,
        View::Transfer,
    ] {
        list.push(SideEntry::Nav(view));
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
    let lang = app.lang();
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

    lines.push(header(t(SIDE_CONNECTION, lang)));
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
            super::status::state_text(app.state, lang),
            super::status::transport_text(app.live_kind(), lang)
        )),
    ]));
    if !app.info.label.is_empty() {
        lines.push(hint(&app.info.label));
    }

    let transport_style = Style::default().fg(Color::Gray);
    item!(
        SideEntry::ToggleTransport,
        tr!(
            t(SIDE_TRANSPORT, lang),
            // The row reports the link that is actually up, never the stored
            // choice: the status line above already says `已连接 · BLE`, so a
            // `传输方式：LAN` next to it was a lie (F1). With no session the
            // choice *is* what would be dialled, so it still shows there.
            app.transport_choice().label(),
            if app.transport_locked() {
                t(SIDE_TRANSPORT_LOCKED, lang)
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
            tr!(
                t(SIDE_DEVICE, lang),
                display_field(&app.sidebar.ble_name, false)
            ),
            field_style
        );
        if app.sidebar.ble_name.is_empty() {
            lines.push(hint(t(SIDE_EMPTY_NAME, lang)));
        }
    }

    if app.connected() {
        item!(
            SideEntry::Disconnect,
            t(SIDE_DISCONNECT, lang),
            Style::default().fg(Color::Red)
        );
    } else {
        item!(
            SideEntry::Connect,
            t(SIDE_CONNECT, lang),
            Style::default().fg(Color::Green)
        );
        if app.transport_choice() != TransportChoice::Lan {
            item!(
                SideEntry::SwitchDevice,
                t(SIDE_SWITCH_DEVICE, lang),
                Style::default().fg(Color::Gray)
            );
        }
    }

    if app.transport_choice() == TransportChoice::Lan {
        item!(
            SideEntry::LanHost,
            tr!(
                t(SIDE_LAN_HOST, lang),
                display_field(&app.sidebar.lan_host, false)
            ),
            field_style
        );
        if app.sidebar.lan_host.is_empty() {
            lines.push(hint(t(SIDE_EMPTY_HOST, lang)));
        }
        item!(
            SideEntry::LanToken,
            tr!(
                t(SIDE_LAN_TOKEN, lang),
                display_field(&app.sidebar.lan_token, true)
            ),
            field_style
        );
        lines.push(hint(t(SIDE_TOKEN_HINT, lang)));
    }
    lines.push(Line::from(""));

    // Section links (CONTRACTS.md section 5): drawn exactly where `entries()`
    // lists them, so the cursor marker lands on what the eye is reading.
    lines.push(header(t(SIDE_VIEWS, lang)));
    let nav_style = Style::default().fg(Color::Gray);
    for view in [
        View::Terminal,
        View::Diagnostics,
        View::Network,
        View::Assistant,
        View::Transfer,
    ] {
        item!(SideEntry::Nav(view), view.label(lang), nav_style);
    }
    lines.push(Line::from(""));

    lines.push(header(t(SIDE_QUICK_SEND, lang)));
    lines.push(hint(t(SIDE_QUICK_HINT, lang)));
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

    lines.push(header(t(SIDE_WATCH, lang)));
    if !app.watch_ok {
        lines.push(hint(t(SIDE_WATCH_NOT_READY, lang)));
    } else if app.watch.findings().is_empty() {
        lines.push(hint(t(SIDE_WATCH_EMPTY, lang)));
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

/// Put the stored token of the host in the form into the token field (web
/// `elements.wsTokenInput.value = lanTokens.selectHost(host)`): the alias
/// resolves to the device that owns the token, so a field the user never
/// fills dials with what a BLE session already captured. Returns whether the
/// field was set — nothing is cleared when the host has no alias, a token the
/// user typed stays theirs.
pub fn fill_token_from_store(app: &mut App, store: &TokenStore) -> bool {
    let host = app.sidebar.lan_host.as_str().trim().to_string();
    if host.is_empty() {
        return false;
    }
    match store.select_host(&host) {
        Some(token) => {
            app.sidebar.lan_token.set(token);
            true
        }
        None => false,
    }
}

fn persist_connection_fields(app: &mut App) {
    if !matches!(app.settings.transport, TransportChoice::Lan) {
        // The BLE name is not part of tui.json (the session remembers its own
        // device); LAN host is.
        return;
    }
    // Compare by `&str`. This runs on **every** key in the field — a paste is
    // replayed as one keystroke per character (`mod.rs::paste`) — so the
    // unconditional `to_string()` below copied the whole, growing host on each
    // of them: O(n²) byte moves and n allocations inside the frame loop,
    // seconds of freeze for a large paste.
    if app.settings.last_lan_host == app.sidebar.lan_host.as_str().trim() {
        return;
    }
    // In memory immediately: the dial path, this row and every other settings
    // write read it from there. The disk half — reading the token store and
    // rewriting tui.json — waits in `poll()` instead of running per keystroke.
    app.settings.last_lan_host = app.sidebar.lan_host.as_str().trim().to_string();
    app.sidebar.token_at_host_edit = app.sidebar.lan_token.as_str().to_string();
    app.sidebar.host_persist_since = Some(Instant::now());
}

/// Run a pending host edit once the field has sat still for
/// [`HOST_PERSIST_SETTLE`]. The main loop calls this every tick; it is a no-op
/// until [`persist_connection_fields`] marked one.
pub fn poll(app: &mut App, now: Instant) {
    let Some(since) = app.sidebar.host_persist_since else {
        return;
    };
    if now.saturating_duration_since(since) < HOST_PERSIST_SETTLE {
        return;
    }
    flush(app);
}

/// Write the pending host edit out **now**: re-select that host's token and
/// save `tui.json`.
///
/// Dialing calls this before it reads the form, so a host typed a moment ago
/// still dials with its own token rather than the previous host's — the exact
/// state the per-keystroke write used to leave behind. Teardown calls it too,
/// so a host typed right before quitting is not dropped.
pub fn flush(app: &mut App) {
    flush_with(app, TokenStore::load);
}

/// [`flush`] with the store read injected — and read **only when the refill
/// may use it**, which is the whole point of the guard below.
fn flush_with(app: &mut App, load: impl FnOnce() -> TokenStore) {
    if app.sidebar.host_persist_since.take().is_none() {
        return;
    }
    // Editing the host re-selects that host's token — but only while the
    // field still holds what it held when the edit was made. Settling takes
    // [`HOST_PERSIST_SETTLE`], long enough to paste a token, and the refill
    // used to run unconditionally: the value the user had just entered was
    // replaced by the store's older one, then dialled and saved.
    if app.sidebar.lan_token.as_str() == app.sidebar.token_at_host_edit {
        fill_token_from_store(app, &load());
    }
    if let Err(err) = super::settings::save(&app.settings) {
        app.toast(
            NoticeLevel::Warn,
            tr!(t(MSG_SAVE_SETTINGS, app.lang()), err),
        );
    }
}

fn activate(app: &mut App, entry: SideEntry) {
    let lang = app.lang();
    match entry {
        SideEntry::Nav(view) => app.set_view(view),
        SideEntry::ToggleTransport => {
            if app.transport_locked() {
                app.toast(NoticeLevel::Warn, t(MSG_TRANSPORT_LOCKED, lang).to_string());
            } else {
                app.settings.transport = match app.settings.transport {
                    TransportChoice::Ble => TransportChoice::Lan,
                    TransportChoice::Lan => TransportChoice::Ble,
                };
                // Switching to the bridge pre-fills its token the way the web
                // does when the mode turns to `ws`.
                if matches!(app.settings.transport, TransportChoice::Lan) {
                    let store = TokenStore::load();
                    fill_token_from_store(app, &store);
                }
                if let Err(err) = super::settings::save(&app.settings) {
                    app.toast(NoticeLevel::Warn, tr!(t(MSG_SAVE_SETTINGS, lang), err));
                }
            }
        }
        SideEntry::Connect => super::connect::connect(app),
        SideEntry::Disconnect => {
            app.dialog = Some(Dialog::Confirm {
                kind: ConfirmKind::Disconnect,
                title: t(CONFIRM_DISCONNECT_TITLE, lang).to_string(),
                message: t(CONFIRM_DISCONNECT_MSG, lang).to_string(),
            });
        }
        SideEntry::SwitchDevice => {
            // Sweep the band and list what answers. The web gives this button
            // to `connect({ chooseDevice: true })`, which calls
            // `requestDevice()` and lets the browser both scan and list; the
            // TUI's version used to do neither — it just moved the cursor onto
            // the name field, so a second accessory could only be reached by
            // typing its address by hand.
            if app.transport_choice() == TransportChoice::Lan {
                app.toast(NoticeLevel::Info, t(MSG_SWITCH_BLE, lang).to_string());
            } else {
                super::connect::begin_scan(app);
            }
        }
        SideEntry::BleName | SideEntry::LanHost | SideEntry::LanToken => {}
        SideEntry::Preset(index) => {
            let (cmd, danger) = PRESETS[index];
            // The web keeps the presets and the key bar disabled until
            // `setConnected()` says otherwise, so a press with no link behind
            // it has to answer here — the row is only dimmed, and running the
            // command into the gate (`send_text`) would have reported nothing
            // at all, which for `reboot` reads as a success that never
            // happened.
            if !app.connected() {
                app.toast(NoticeLevel::Warn, t(DLG_NOT_SENT, lang).to_string());
                return;
            }
            if danger {
                app.dialog = Some(Dialog::Confirm {
                    kind: ConfirmKind::Reboot,
                    title: t(CONFIRM_REBOOT_TITLE, lang).to_string(),
                    message: t(CONFIRM_REBOOT_MSG, lang).to_string(),
                });
            } else {
                app.send_text(&format!("{cmd}\n"));
            }
        }
        SideEntry::Finding(index) => {
            if let Some(finding) = app.watch.findings().get(index) {
                let text = crate::watch::SerialWatch::describe(finding, lang.code());
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
    use super::super::i18n::Lang;
    use super::super::test_app;
    use super::*;
    use crate::event::ConnectionState;
    use crate::tui::settings::settings_path;

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

    /// The web keeps the presets disabled until `setConnected()`; ours are
    /// only dimmed, so a press with no link has to *say* so — running into
    /// `send_text`'s gate reported nothing at all, which for `reboot` reads
    /// as a reboot that never happened.
    #[test]
    fn a_preset_with_no_link_says_so_instead_of_sending_nothing() {
        let mut app = test_app();
        app.state = ConnectionState::Disconnected;

        activate(&mut app, SideEntry::Preset(1)); // "version"
        let toast = app.notices.toasts.last().expect("it explains itself");
        assert_eq!(toast.text, t(DLG_NOT_SENT, app.lang()));
        assert!(app.dialog.is_none(), "a plain preset never asks anything");

        // The dangerous one does not even open its box.
        activate(&mut app, SideEntry::Preset(4)); // "reboot"
        assert!(
            app.dialog.is_none(),
            "there is nothing to confirm when nothing can be sent"
        );

        // With a link behind it, the same press asks first, as always.
        app.state = ConnectionState::Connected;
        activate(&mut app, SideEntry::Preset(4));
        assert!(matches!(app.dialog, Some(Dialog::Confirm { .. })));
    }

    #[test]
    fn every_sidebar_message_is_translated() {
        super::super::i18n::assert_bilingual(ALL);
        assert!(ALL.len() >= 24, "sidebar alone carries 24 messages");
    }

    /// First open leaves the host row empty, so it has to say what belongs
    /// there — the web already tells you the same thing through
    /// `#wsHostInput`'s placeholder (`192.168.1.50 or ws://host/ws`).
    #[test]
    fn the_empty_lan_host_row_points_at_the_expected_address_shape() {
        let render = |app: &App| -> String {
            render_lines(app)
                .iter()
                .flat_map(|line| {
                    line.spans
                        .iter()
                        .map(|span| span.content.as_ref().to_string())
                })
                .collect::<Vec<_>>()
                .join("\n")
        };

        let mut app = test_app();
        app.settings.transport = TransportChoice::Lan;
        app.sidebar.lan_host.clear();
        let text = render(&app);
        assert!(text.contains("192.168.1.50"), "{text}");
        assert!(text.contains("ws://host/ws"), "{text}");

        // A real address silences it: the hint is a placeholder, not furniture.
        app.sidebar.lan_host.set("192.0.2.1".to_string());
        let text = render(&app);
        assert!(!text.contains("192.168.1.50"), "{text}");
    }

    /// The sidebar has to draw exactly what `entries()` lists, row for row.
    /// Regression: the four section links were selectable without being drawn,
    /// which shifted the `▸` marker off every later row — focusing the sidebar
    /// moved the selection around with nothing lighting up.
    #[test]
    fn the_cursor_walks_the_rows_top_to_bottom() {
        let mut app = crate::tui::test_app();
        let lang = app.lang();

        // The section links (CONTRACTS.md section 5) are on screen — in both
        // languages: the gauge walks this list in one run only, so a link
        // that fits in English and gets clipped in Chinese would pass it.
        for lang in [lang, Lang::Zh] {
            app.settings.lang = lang;
            for view in [
                View::Terminal,
                View::Diagnostics,
                View::Network,
                View::Assistant,
                View::Transfer,
            ] {
                let label = view.label(lang);
                assert!(
                    render_lines(&app)
                        .iter()
                        .any(|line| line.spans.iter().any(|span| span.content.contains(label))),
                    "the {lang:?} sidebar must draw the {label} section link"
                );
            }
        }
        app.settings.lang = lang;

        let items = entries(&app);
        let mut previous = 0;
        for index in 0..items.len() {
            app.sidebar.selection = index;
            let row = render_lines(&app)
                .iter()
                .position(|line| line.spans.first().map(|span| span.content.as_ref()) == Some("▸ "))
                .unwrap_or_else(|| panic!("entry {index} draws no cursor marker"));
            assert!(
                row > previous,
                "entry {index} is drawn at line {row}, at or above the \
                 previous entry at line {previous}"
            );
            previous = row;
        }
    }

    /// F1: the row reports the link that is actually up. The status line said
    /// `已连接 · BLE · Linkr BLE UART-3` while this row still claimed
    /// `传输方式：LAN（已锁定）`, because it rendered the stored *choice*.
    #[test]
    fn the_transport_row_reports_the_live_link() {
        let mut app = crate::tui::test_app();
        app.settings.transport = TransportChoice::Lan;
        app.state = ConnectionState::Connected;
        app.info.kind = Some(crate::transport::TransportKind::Ble);

        let row = transport_row(&app);
        assert!(!row.is_empty(), "the transport row must be drawn");
        assert!(
            row.contains("BLE"),
            "the row must report the live link: {row}"
        );
        assert!(!row.contains("LAN"), "the stored choice leaked in: {row}");
        assert!(
            row.contains("已锁定") || row.contains("locked"),
            "…and it is locked: {row}"
        );
    }

    /// Web parity: the lock follows `connecting || state.connected`
    /// (`web/app.js:1949-1950` disable both mode buttons on exactly that), so
    /// an attempt in flight renders the row `（已锁定）` just like a live
    /// session. It used to stay switchable mid-dial — which is what left the
    /// form on BLE while a LAN attempt was still running, with "Switch device"
    /// answering "Connecting…" from that dead end.
    #[test]
    fn the_lock_follows_the_attempt_and_the_live_link() {
        let mut app = crate::tui::test_app();
        app.state = ConnectionState::Connecting;
        app.pending_connect = Some(tokio::sync::oneshot::channel().1);
        let row = transport_row(&app);
        assert!(
            row.contains("已锁定") || row.contains("locked"),
            "locked while an attempt is in flight: {row}"
        );

        app.pending_connect = None;
        app.state = ConnectionState::Connected;
        let row = transport_row(&app);
        assert!(
            row.contains("已锁定") || row.contains("locked"),
            "locked next to a live link: {row}"
        );

        // Settled with nothing up, the marker comes back: the row promises a
        // toggle it can honour.
        app.state = ConnectionState::Disconnected;
        let row = transport_row(&app);
        assert!(row.contains("◂▸"), "switchable once settled: {row}");
    }

    /// Mid-attempt the toggle refuses **with a reason** instead of flipping
    /// the form out from under the dial that is running — the state web
    /// reaches by disabling both mode buttons (`web/app.js:1949-1950`).
    #[test]
    fn switching_the_transport_mid_attempt_is_refused_with_a_reason() {
        let mut app = crate::tui::test_app();
        app.settings.transport = TransportChoice::Lan;
        app.state = ConnectionState::Connecting;
        app.pending_connect = Some(tokio::sync::oneshot::channel().1);

        activate(&mut app, SideEntry::ToggleTransport);

        assert_eq!(
            app.settings.transport,
            TransportChoice::Lan,
            "the form must not move while an attempt is in flight"
        );
        let toast = app
            .notices
            .toasts
            .last()
            .expect("the refusal explains itself");
        assert_eq!(toast.text, t(MSG_TRANSPORT_LOCKED, app.lang()));
    }

    /// The transport row, as one string: the only row the sidebar renders from
    /// the *live* state instead of the stored settings.
    fn transport_row(app: &App) -> String {
        let lang = app.lang();
        // `SIDE_TRANSPORT` is a format string ("Transport: {} {}" /
        // "传输方式：{} {}"); its head marks that one row.
        let marker = t(SIDE_TRANSPORT, lang)
            .split('{')
            .next()
            .unwrap_or_default();
        render_lines(app)
            .iter()
            .filter(|line| line.spans.iter().any(|span| span.content.contains(marker)))
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The sidebar status line, as one string: dot, state, transport.
    fn status_row(app: &App) -> String {
        let lang = app.lang();
        render_lines(app)
            .iter()
            .filter(|line| {
                line.spans.iter().any(|span| {
                    span.content
                        .contains(super::super::status::state_text(app.state, lang))
                })
            })
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The reported "switch to LAN does nothing". The link was gone but its
    /// kind was not — `SessionInfo::kind` is written when the session is
    /// built and teardown never clears it — so this row rendered `BLE` while
    /// Enter rewrote a setting nobody could see, and the next dial went to
    /// BLE all the same. Both the row and the status line answer for a
    /// session that is still there, and only for one.
    #[test]
    fn the_sidebar_forgets_the_link_the_moment_it_is_gone() {
        let mut app = crate::tui::test_app();
        app.settings.transport = TransportChoice::Lan;
        app.info.kind = Some(crate::transport::TransportKind::Ble);
        app.state = ConnectionState::Disconnected;

        let row = transport_row(&app);
        assert!(row.contains("LAN"), "the stored choice must show: {row}");
        assert!(!row.contains("BLE"), "the dead link leaked in: {row}");
        assert!(
            !row.contains("已锁定") && !row.contains("locked"),
            "…and Enter has to move it: {row}"
        );

        let status = status_row(&app);
        assert!(
            !status.contains("BLE"),
            "the status line kept the dead link: {status}"
        );
        assert!(status.contains('—'), "no link, no transport: {status}");

        // A link that is up says so again, and locks the row (F1).
        app.state = ConnectionState::Connected;
        let row = transport_row(&app);
        assert!(row.contains("BLE"), "the live link must show: {row}");
        assert!(
            row.contains("已锁定") || row.contains("locked"),
            "…and it is locked: {row}"
        );
    }

    use super::super::dialogs::REBOOT_CONFIRM;

    /// The token field follows the host alias the way the web's token field
    /// does — and keeps what the user typed when the alias names nobody.
    #[test]
    fn the_token_field_follows_the_host_alias() {
        const DEVICE: &str = "4c494e4b52424c45010058bf2533078c";
        const TOKEN: &str = "0123456789abcdef0123456789abcdef";
        let mut store = TokenStore::default();
        store
            .capture(DEVICE, TOKEN, "192.168.0.104")
            .expect("accepted");

        let mut app = crate::tui::test_app();
        app.sidebar.lan_host.set("192.168.0.104");
        assert!(fill_token_from_store(&mut app, &store));
        assert_eq!(app.sidebar.lan_token.as_str(), TOKEN);

        // A host the store does not know leaves a hand-typed token alone.
        app.sidebar.lan_host.set("192.0.2.1");
        app.sidebar
            .lan_token
            .set("ffffffffffffffffffffffffffffffff");
        assert!(!fill_token_from_store(&mut app, &store));
        assert_eq!(
            app.sidebar.lan_token.as_str(),
            "ffffffffffffffffffffffffffffffff"
        );

        // No host to select by fills nothing.
        app.sidebar.lan_host.set("   ");
        assert!(!fill_token_from_store(&mut app, &store));
    }

    /// A test that reaches `settings::save()` must leave the developer's own
    /// `tui.json` exactly as it found it — panic or not.
    ///
    /// It also holds the settings-file lock for the whole test: libtest runs
    /// tests side by side, so without it another test could write the file
    /// between this test's read and its restore (and the assertion in between
    /// would read that test's bytes). The lock is released only after the
    /// file has been put back, because `Drop` runs before the fields do.
    struct SettingsGuard {
        snapshot: Option<Vec<u8>>,
        _lock: crate::tui::settings::file_lock::FileLock,
    }

    impl SettingsGuard {
        fn new() -> Self {
            let _lock = crate::tui::settings::file_lock::FileLock::acquire();
            Self {
                snapshot: std::fs::read(settings_path()).ok(),
                _lock,
            }
        }
    }

    impl Drop for SettingsGuard {
        fn drop(&mut self) {
            match &self.snapshot {
                Some(bytes) => {
                    let _ = std::fs::write(settings_path(), bytes);
                }
                None => {
                    let _ = std::fs::remove_file(settings_path());
                }
            }
        }
    }

    fn on_disk() -> Vec<u8> {
        std::fs::read(settings_path()).unwrap_or_default()
    }

    /// P2b: the edited host is live in memory at once (the dial path reads it
    /// from there), but `tui.json` is only rewritten once the field has sat
    /// still. Before this the write ran on **every** keystroke — 482 B and two
    /// write syscalls per character, 2.1 ms each, so a long paste stalled the
    /// interface for seconds.
    #[test]
    fn a_host_edit_reaches_disk_once_typing_pauses() {
        let _settings = SettingsGuard::new();
        let mut app = crate::tui::test_app();
        app.settings.transport = TransportChoice::Lan;
        app.sidebar.lan_host.set("192.0.2.1");
        app.settings.last_lan_host = "192.0.2.1".to_string();

        app.sidebar.lan_host.set("192.0.2.10");
        persist_connection_fields(&mut app);
        assert_eq!(
            app.settings.last_lan_host, "192.0.2.10",
            "the value is current in memory right away"
        );
        let since = app
            .sidebar
            .host_persist_since
            .expect("the disk half is pending");
        let snap = on_disk();

        // Inside the settle window the file is untouched.
        poll(&mut app, since + HOST_PERSIST_SETTLE / 2);
        assert_eq!(on_disk(), snap, "no write while the user is still typing");
        assert!(app.sidebar.host_persist_since.is_some());

        // The moment the field has sat still, the pending write runs.
        poll(&mut app, since + HOST_PERSIST_SETTLE);
        assert!(
            app.sidebar.host_persist_since.is_none(),
            "the pending write has run"
        );
        let written = String::from_utf8_lossy(&on_disk()).into_owned();
        assert!(
            written.contains("192.0.2.10"),
            "…and the new host is on disk"
        );

        // With nothing pending, flushing does nothing at all.
        let after = on_disk();
        flush(&mut app);
        assert_eq!(on_disk(), after, "an idle form writes nothing");
    }

    /// Dialing runs the pending write first, so a host typed a moment ago is
    /// dialled with its own token instead of the previous host's — the state
    /// the per-keystroke write used to leave behind. The token here is not
    /// 32 hex digits, so `start()` rejects the form and no dial leaves the
    /// process.
    #[test]
    fn dialing_flushes_the_pending_host_write_before_it_reads_the_form() {
        let _settings = SettingsGuard::new();
        let mut app = crate::tui::test_app();
        app.settings.transport = TransportChoice::Lan;
        app.sidebar.lan_host.set("192.0.2.1");
        app.settings.last_lan_host = "192.0.2.1".to_string();
        app.sidebar.lan_token.set("not-a-token");

        app.sidebar.lan_host.set("192.0.2.10");
        persist_connection_fields(&mut app);
        assert!(app.sidebar.host_persist_since.is_some());

        super::super::connect::connect(&mut app);

        assert!(
            app.sidebar.host_persist_since.is_none(),
            "the dial ran the pending write"
        );
        assert!(
            String::from_utf8_lossy(&on_disk()).contains("192.0.2.10"),
            "…which reached the settings file"
        );
    }

    /// The host edit re-selects that host's token — but only the token it
    /// found. The settle window is 400 ms, long enough to paste one of one's
    /// own, and the refill used to run unconditionally: what the user had
    /// just entered was replaced by the store's older value, then dialled
    /// *and* written to disk. A leftover from the previous host is still
    /// replaced, which is what changing the host means.
    #[test]
    fn only_the_token_the_host_edit_found_is_replaced() {
        const DEVICE: &str = "4c494e4b52424c45010058bf2533078c";
        const MINE: &str = "0123456789abcdef0123456789abcdef";
        const STORES: &str = "fedcba9876543210fedcba9876543210";
        fn store_for(host: &str) -> TokenStore {
            let mut store = TokenStore::default();
            store.capture(DEVICE, STORES, host).expect("accepted");
            store
        }

        let _settings = SettingsGuard::new();
        let mut app = crate::tui::test_app();
        app.settings.transport = TransportChoice::Lan;
        app.sidebar.lan_host.set("192.0.2.1");
        app.settings.last_lan_host = "192.0.2.1".to_string();
        app.sidebar.lan_token.set(MINE);

        app.sidebar.lan_host.set("192.0.2.10");
        persist_connection_fields(&mut app);
        // Inside the settle window the user pastes a token of their own.
        app.sidebar.lan_token.set(STORES);

        let mut reads = 0;
        flush_with(&mut app, || {
            reads += 1;
            store_for("192.0.2.10")
        });

        assert_eq!(
            app.sidebar.lan_token.as_str(),
            STORES,
            "what the user entered is what gets dialled and saved"
        );
        assert_eq!(reads, 0, "…so the store is not even read");
        assert!(app.sidebar.host_persist_since.is_none(), "it still settled");

        // Untouched, the very same flush re-selects the new host's token.
        app.sidebar.lan_token.set(MINE);
        app.sidebar.host_persist_since = Some(Instant::now());
        flush_with(&mut app, || store_for("192.0.2.10"));
        assert_eq!(
            app.sidebar.lan_token.as_str(),
            STORES,
            "the previous host's leftover is what the switch replaces"
        );
    }
}
