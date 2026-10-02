//! OpenCode-style TUI: sidebar, terminal pane, assistant panel, status line,
//! command palette and modal dialogs (`specs/WEB_UX_SPEC.md`, CONTRACTS.md
//! section 5).
//!
//! The whole UI runs on its own thread with its own Tokio runtime: `run` is
//! called from inside the CLI's runtime (`cli::drive_connected`), so blocking
//! that thread would stall the session. Everything the loop needs from async
//! land is reached through non-blocking handles (`oneshot::try_recv`,
//! `broadcast::try_recv`), which keeps the frame loop allocation-light and
//! makes the redraw cadence independent of the transport.

pub mod agent_settings;
pub mod assistant_view;
pub mod connect;
pub mod diagnostics_view;
pub mod dialogs;
pub mod keys;
pub mod layout;
pub mod network_view;
pub mod palette;
pub mod replies;
pub mod settings;
pub mod sidebar;
pub mod state;
pub mod status;
pub mod terminal_view;

use std::io::stdout;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;

use crate::event::{ConnectionState, CoreEvent, NoticeLevel};
use crate::session::{CoreBus, SessionHandle};
use crate::watch::{SerialWatch, WatchOptions};

use self::dialogs::Dialog;
use self::keys::{encode_key, translate_enter, StickyMods};
use self::palette::PaletteState;
use self::sidebar::SidebarState;
use self::state::{App, Focus, Notices, View};
use self::terminal_view::TerminalPane;

/// Default BLE device name (`PYTHON_CLI_SPEC` `--name` default), used by the
/// sidebar connection card until the user edits it.
pub const DEFAULT_BLE_NAME: &str = "Linkr BLE UART";

/// Idle frame cadence (about 60 fps): a crossterm poll timeout, not a busy
/// spin, so the status clock and the toasts stay live without burning CPU.
const TICK: Duration = Duration::from_millis(16);

type Ui = Terminal<CrosstermBackend<std::io::Stdout>>;

/// Render context passed from the session after connect.
pub struct TuiContext {
    pub session: SessionHandle,
    pub bus: CoreBus,
}

/// Run the TUI until the user quits. Returns the process exit code.
///
/// The UI lives on a dedicated thread so the caller's Tokio runtime keeps
/// driving the session while the terminal is owned by ratatui.
pub fn run(ctx: TuiContext) -> i32 {
    let thread = match std::thread::Builder::new()
        .name("linkr-tui".to_string())
        .spawn(move || ui_thread(ctx))
    {
        Ok(thread) => thread,
        Err(err) => {
            eprintln!("linkr: cannot start the TUI: {err}");
            return 1;
        }
    };
    match thread.join() {
        Ok(code) => code,
        Err(_) => {
            eprintln!("linkr: the TUI thread panicked");
            1
        }
    }
}

fn ui_thread(ctx: TuiContext) -> i32 {
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("linkr-tui-io")
        .build()
    {
        Ok(rt) => Arc::new(rt),
        Err(err) => {
            eprintln!("linkr: cannot start the TUI runtime: {err}");
            return 1;
        }
    };
    ui_session(rt, ctx)
}

/// Take the terminal over (raw mode + alternate screen) and restore it on
/// every exit path, including unwinding.
struct ScreenGuard;

impl Drop for ScreenGuard {
    fn drop(&mut self) {
        let mut out = stdout();
        let _ = crossterm::execute!(out, crossterm::event::DisableBracketedPaste);
        let _ = crossterm::execute!(out, LeaveAlternateScreen);
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(out, crossterm::cursor::Show);
    }
}

fn ui_session(rt: Arc<tokio::runtime::Runtime>, ctx: TuiContext) -> i32 {
    if !crate::term::stdin_is_tty() {
        eprintln!("linkr: the TUI needs an interactive terminal");
        return 1;
    }
    let raw = match crate::term::RawModeGuard::enable(true) {
        Ok(raw) => raw,
        Err(err) => {
            eprintln!("linkr: {err}");
            return 1;
        }
    };
    if let Err(err) =
        crossterm::execute!(stdout(), EnterAlternateScreen, event::EnableBracketedPaste)
    {
        eprintln!("linkr: cannot enter the alternate screen: {err}");
        drop(raw);
        return 1;
    }
    let _screen = ScreenGuard;

    let mut terminal = match Terminal::new(CrosstermBackend::new(stdout())) {
        Ok(terminal) => terminal,
        Err(err) => {
            eprintln!("linkr: cannot attach to the terminal: {err}");
            return 1;
        }
    };
    let _ = terminal.clear();

    let code = event_loop(&mut terminal, rt, ctx);
    let _ = terminal.show_cursor();
    code
}

// --- state -------------------------------------------------------------------

fn build_app(rt: Arc<tokio::runtime::Runtime>, ctx: TuiContext) -> App {
    let settings = settings::load();
    let lan_host = settings.last_lan_host.clone();
    let info = ctx.session.info();
    let state = if info.connected {
        ConnectionState::Connected
    } else {
        ConnectionState::Disconnected
    };
    let detail = if info.label.is_empty() {
        match state {
            ConnectionState::Connected => "connected".to_string(),
            _ => "not connected".to_string(),
        }
    } else {
        info.label.clone()
    };
    let view = settings.active_view;
    let focus = match view {
        View::Assistant => Focus::Assistant,
        _ => Focus::Center,
    };
    App {
        session: ctx.session,
        bus: ctx.bus,
        rt,
        settings,
        state,
        detail,
        info,
        view,
        focus,
        sidebar: SidebarState::new(DEFAULT_BLE_NAME.to_string(), lan_host),
        terminal: TerminalPane::new(true),
        diagnostics: diagnostics_view::DiagnosticsState::default(),
        network: network_view::NetworkState::new(),
        assistant: assistant_view::AssistantState::default(),
        palette: None,
        dialog: None,
        notices: Notices::default(),
        exec_mode: crate::agent::ExecMode::Auto,
        broker: dialogs::TuiBroker::new(),
        agent: None,
        watch: SerialWatch::new(WatchOptions::default()),
        watch_ok: true,
        quit: false,
        started: Instant::now(),
        force_redraw: true,
        pending_connect: None,
        center_height: 24,
        center_scroll: 0,
    }
}

// --- event loop --------------------------------------------------------------

fn event_loop(terminal: &mut Ui, rt: Arc<tokio::runtime::Runtime>, ctx: TuiContext) -> i32 {
    let mut app = build_app(rt, ctx);
    app.terminal.set_autoscroll(app.settings.autoscroll);
    let mut core = app.bus.subscribe();
    let mut sticky = StickyMods::default();

    while !app.quit {
        // 1. Session bus: UART output, connection lifecycle, notices.
        loop {
            match core.try_recv() {
                Ok(event) => on_core_event(&mut app, event),
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(dropped)) => {
                    app.notices.push(
                        NoticeLevel::Warn,
                        format!("{dropped} session events dropped."),
                    );
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Closed) => break,
            }
        }

        // 2. Non-blocking work parked on oneshots / broadcast receivers.
        connect::poll(&mut app);
        if app.dialog.is_none() && app.palette.is_none() {
            if let Some(pending) = app.take_approval() {
                app.dialog = Some(Dialog::Approval(Box::new(pending)));
            }
        }
        dialogs::poll(&mut app);
        network_view::poll(&mut app);
        app.diagnostics.poll();
        assistant_view::poll(&mut app);
        app.notices.tick();
        app.refresh_info();
        auto_diagnostics(&mut app);
        prefill_lan_host(&mut app);

        // 3. Terminal input (blocked at most one frame per key press).
        match event::poll(TICK) {
            Ok(true) => match event::read() {
                Ok(Event::Key(key)) if key.kind != KeyEventKind::Release => {
                    handle_key(&mut app, &mut sticky, key)
                }
                Ok(Event::Resize(_, _)) => app.force_redraw = true,
                Ok(Event::Paste(text)) => paste(&mut app, &text),
                Ok(_) => {}
                Err(err) => {
                    app.notices
                        .push(NoticeLevel::Error, format!("Terminal input failed: {err}"));
                    break;
                }
            },
            Ok(false) => {}
            Err(err) => {
                app.notices
                    .push(NoticeLevel::Error, format!("Terminal input failed: {err}"));
                break;
            }
        }

        // 4. Geometry: the VT grid follows the pane, the session follows the
        //    grid (web `terminal_geometry.js` debounce, here instant).
        let area = match terminal.size() {
            Ok(size) => Rect::new(0, 0, size.width, size.height),
            Err(_) => Rect::new(0, 0, 80, 24),
        };
        sync_geometry(&mut app, area);

        // 5. Draw.
        if let Err(err) = terminal.draw(|frame| layout::draw(frame, &app)) {
            app.notices
                .push(NoticeLevel::Error, format!("Draw failed: {err}"));
            break;
        }
        app.force_redraw = false;
    }

    // Teardown: stop the assistant, hang up politely, let the disconnect
    // command reach the transport before the caller's runtime disappears.
    if let Some(agent) = app.agent.take() {
        agent.handle.stop();
    }
    if app.info.connected || app.state == ConnectionState::Connected {
        app.session.disconnect();
        let rt = app.rt.clone();
        rt.block_on(async { tokio::time::sleep(Duration::from_millis(120)).await });
    }
    0
}

/// Feed one bus event into the state that shows it.
fn on_core_event(app: &mut App, event: CoreEvent) {
    match event {
        CoreEvent::UartRx(bytes) => {
            let at_ms = app.started.elapsed().as_millis() as u64;
            if app.watch_ok {
                app.watch.feed_bytes(&bytes, at_ms);
            }
            app.terminal.feed(&bytes);
        }
        CoreEvent::Connection { state, detail } => {
            app.info = app.session.info();
            app.state = state;
            app.detail = detail.clone();
            match state {
                ConnectionState::Disconnected => {
                    app.notices
                        .push(NoticeLevel::Info, format!("Disconnected: {detail}"));
                }
                ConnectionState::Failed => {
                    app.notices
                        .push(NoticeLevel::Error, format!("Connection failed: {detail}"));
                }
                _ => {}
            }
        }
        CoreEvent::Notice { level, text } => app.notices.push(level, text),
        // Management traffic has its own owners (dialogs, diagnostics, the
        // network form); the assistant reads it from the bus on its own.
        CoreEvent::MgmtMessage { .. } => {}
    }
}

/// Fetch `@i?` when the diagnostics view opens without data. A failed request
/// is not retried automatically (the `r` key clears the error first).
fn auto_diagnostics(app: &mut App) {
    if app.view != View::Diagnostics || !app.ble_connected() {
        return;
    }
    if app.diagnostics.loading || app.diagnostics.done || app.diagnostics.error.is_some() {
        return;
    }
    let session = app.session.clone();
    app.diagnostics.refresh(&session);
}

/// The web client copies `wifi.ip` into the LAN host field when it looks like
/// an address; do the same once diagnostics reported one.
fn prefill_lan_host(app: &mut App) {
    if !app.sidebar.lan_host.is_empty() {
        return;
    }
    let Some((_, ip)) = diagnostics_view::wifi_from_info(&app.diagnostics.groups) else {
        return;
    };
    if diagnostics_view::looks_like_ipv4(&ip) {
        app.sidebar.lan_host.set(ip);
    }
}

/// Keep the grid, the pane and the session geometry in sync.
fn sync_geometry(app: &mut App, area: Rect) {
    let (_top, body, _bottom) = layout::zones(area);
    let (_sidebar, center) = layout::columns(body, area.width >= 60);
    app.center_height = center.height;
    let (cols, rows) =
        terminal_view::grid_dims(center.width, center.height, app.settings.font_size);
    if app.terminal.sync_size(cols, rows) {
        app.session.set_terminal_size(cols, rows);
        app.force_redraw = true;
    }
}

fn paste(app: &mut App, text: &str) {
    if app.dialog.is_some() || app.palette.is_some() {
        return;
    }
    if app.focus != Focus::Center || app.view != View::Terminal {
        return;
    }
    let plain = translate_enter(text.as_bytes(), app.settings.enter_mode);
    if app.settings.local_echo {
        app.terminal.feed(&plain);
    }
    // Honour DECSET 2004 when the target shell asked for bracketed paste.
    let payload = if app.terminal.grid.bracketed_paste() {
        let mut wrapped = b"\x1b[200~".to_vec();
        wrapped.extend_from_slice(&plain);
        wrapped.extend_from_slice(b"\x1b[201~");
        wrapped
    } else {
        plain
    };
    app.send_bytes(payload);
}

// --- keys --------------------------------------------------------------------

fn arm_sticky(app: &mut App, sticky: &mut StickyMods, next: StickyMods, label: &str) {
    if *sticky == next {
        *sticky = StickyMods::default();
        app.toast(NoticeLevel::Info, format!("{label} latch released."));
    } else {
        *sticky = next;
        app.toast(
            NoticeLevel::Info,
            format!("{label} armed for the next key."),
        );
    }
}

/// Global bindings (CONTRACTS.md section 5). Resolved before the focused
/// pane sees a key, so they work from every view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Global {
    None,
    Palette,
    Help,
    ClearTerminal,
    Quit,
    FocusSidebar,
    View(View),
    ArmShift,
    ArmCtrl,
    ArmAlt,
}

/// `F2..F5` → view (the four surfaces).
fn fkey_view(n: u8) -> Option<View> {
    match n {
        2 => Some(View::Terminal),
        3 => Some(View::Diagnostics),
        4 => Some(View::Network),
        5 => Some(View::Assistant),
        _ => None,
    }
}

fn global_key(code: KeyCode, mods: KeyModifiers) -> Global {
    let ctrl = mods.contains(KeyModifiers::CONTROL);
    let shift = mods.contains(KeyModifiers::SHIFT);
    match code {
        KeyCode::Char('q') if ctrl => Global::Quit,
        KeyCode::Char('p') if ctrl => Global::Palette,
        KeyCode::Char('l') if ctrl => Global::ClearTerminal,
        KeyCode::Char('k') if ctrl && shift => Global::View(View::Assistant),
        KeyCode::F(1) => Global::Help,
        KeyCode::F(n) => fkey_view(n).map(Global::View).unwrap_or(Global::None),
        KeyCode::Up if ctrl => Global::FocusSidebar,
        // One-shot modifier latches (the TUI's stand-in for the web key bar).
        KeyCode::Char('r') if ctrl && shift => Global::ArmShift,
        KeyCode::Char('c') if ctrl && shift => Global::ArmCtrl,
        KeyCode::Char('a') if ctrl && shift => Global::ArmAlt,
        _ => Global::None,
    }
}

/// Route one key press: overlays first, then the global bindings, then the
/// focused pane.
fn handle_key(app: &mut App, sticky: &mut StickyMods, key: crossterm::event::KeyEvent) {
    let global = global_key(key.code, key.modifiers);

    // Overlays own the keyboard entirely (CONTRACTS.md section 5), but quit
    // and the palette toggle stay reachable from inside them.
    if app.palette.is_some() {
        match global {
            Global::Quit => dialogs::request_quit(app),
            Global::Palette => app.palette = None,
            _ => {
                palette::handle_key(app, key);
            }
        }
        return;
    }
    if app.dialog.is_some() {
        dialogs::handle_key(app, key);
        return;
    }

    match global {
        Global::Quit => {
            dialogs::request_quit(app);
            return;
        }
        Global::Palette => {
            app.palette = Some(PaletteState::default());
            return;
        }
        Global::ClearTerminal => {
            app.terminal.clear();
            app.toast(NoticeLevel::Info, "Terminal cleared.");
            return;
        }
        Global::Help => {
            app.dialog = Some(Dialog::Help);
            return;
        }
        Global::FocusSidebar => {
            app.focus = Focus::Sidebar;
            return;
        }
        Global::View(view) => {
            if view == View::Diagnostics {
                open_diagnostics(app);
            } else {
                app.set_view(view);
            }
            return;
        }
        Global::ArmShift => {
            let next = StickyMods {
                shift: !sticky.shift,
                ..StickyMods::default()
            };
            arm_sticky(app, sticky, next, "Shift");
            return;
        }
        Global::ArmCtrl => {
            let next = StickyMods {
                ctrl: !sticky.ctrl,
                ..StickyMods::default()
            };
            arm_sticky(app, sticky, next, "Ctrl");
            return;
        }
        Global::ArmAlt => {
            let next = StickyMods {
                alt: !sticky.alt,
                ..StickyMods::default()
            };
            arm_sticky(app, sticky, next, "Alt");
            return;
        }
        Global::None => {}
    }

    // Esc walks one level out: sidebar → center, assistant / other views →
    // the terminal view, and only from the terminal view it is a terminal key.
    if key.code == KeyCode::Esc {
        if app.focus == Focus::Sidebar {
            app.focus = Focus::Center;
            return;
        }
        if app.focus == Focus::Assistant || app.view != View::Terminal {
            app.set_view(View::Terminal);
            return;
        }
    }

    match app.focus {
        Focus::Sidebar => {
            sidebar::handle_key(app, key);
            return;
        }
        Focus::Assistant => {
            assistant_view::handle_key(app, key);
            return;
        }
        Focus::Center => {}
    }

    match app.view {
        View::Network => {
            network_view::handle_key(app, key);
            return;
        }
        View::Diagnostics => {
            if handle_diagnostics_key(app, key) {
                return;
            }
        }
        View::Terminal | View::Assistant => {}
    }

    // Terminal pane: scrollback first, then byte encoding.
    if app.view == View::Terminal {
        if handle_scroll_key(app, key) {
            return;
        }
        if let Some(bytes) = encode_key(
            key.code,
            key.modifiers,
            app.terminal.grid.app_cursor_keys(),
            *sticky,
        ) {
            *sticky = StickyMods::default();
            let payload = if key.code == KeyCode::Enter {
                translate_enter(&bytes, app.settings.enter_mode)
            } else {
                bytes
            };
            if app.settings.local_echo {
                app.terminal.feed(&payload);
            }
            app.send_bytes(payload);
        }
    }
}

/// Shift+PgUp/PgDn/Home/End scroll the scrollback (web xterm parity).
fn handle_scroll_key(app: &mut App, key: crossterm::event::KeyEvent) -> bool {
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let plain = !key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
    if !shift || !plain {
        return false;
    }
    let step = (app.center_height / 2).max(1) as usize;
    match key.code {
        KeyCode::PageUp => app.terminal.scroll_back(step),
        KeyCode::PageDown => app.terminal.scroll_forward(step),
        KeyCode::Home => app.terminal.offset = app.terminal.grid.scrollback_len(),
        KeyCode::End => app.terminal.to_bottom(),
        _ => return false,
    }
    true
}

/// Diagnostics keys: `r` re-reads `@i?`, PgUp/PgDn move the grid.
fn handle_diagnostics_key(app: &mut App, key: crossterm::event::KeyEvent) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('r') if !ctrl => {
            if app.ble_connected() {
                app.diagnostics.done = false;
                let session = app.session.clone();
                app.diagnostics.refresh(&session);
            } else {
                app.toast(NoticeLevel::Warn, "Connect over BLE to read diagnostics.");
            }
            true
        }
        KeyCode::PageUp => {
            app.center_scroll = app.center_scroll.saturating_add(5);
            true
        }
        KeyCode::PageDown => {
            app.center_scroll = app.center_scroll.saturating_sub(5);
            true
        }
        _ => false,
    }
}

/// F3 / palette `diag.refresh`: switch and query (BLE-gated, §3.6 matrix).
fn open_diagnostics(app: &mut App) {
    app.set_view(View::Diagnostics);
    app.diagnostics.done = false;
    if app.ble_connected() {
        let session = app.session.clone();
        app.diagnostics.refresh(&session);
    } else {
        app.toast(NoticeLevel::Warn, "Connect over BLE to read diagnostics.");
    }
}

// --- tests -------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::settings::EnterMode;

    #[test]
    fn default_ble_name_is_the_python_cli_default() {
        assert_eq!(DEFAULT_BLE_NAME, "Linkr BLE UART");
    }

    #[test]
    fn enter_modes_are_the_documented_three() {
        assert_eq!(EnterMode::Raw.as_str(), "raw");
        assert_eq!(EnterMode::Lf.as_str(), "lf");
        assert_eq!(EnterMode::Crlf.as_str(), "crlf");
    }

    #[test]
    fn f_keys_map_to_the_four_views_plus_help() {
        assert_eq!(fkey_view(2), Some(View::Terminal));
        assert_eq!(fkey_view(3), Some(View::Diagnostics));
        assert_eq!(fkey_view(4), Some(View::Network));
        assert_eq!(fkey_view(5), Some(View::Assistant));
        assert_eq!(fkey_view(1), None);
        assert_eq!(fkey_view(6), None);
    }

    #[test]
    fn global_bindings_cover_the_contract_keys() {
        let ctrl = KeyModifiers::CONTROL;
        let both = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
        assert_eq!(global_key(KeyCode::Char('p'), ctrl), Global::Palette);
        assert_eq!(global_key(KeyCode::F(1), KeyModifiers::NONE), Global::Help);
        assert_eq!(
            global_key(KeyCode::F(3), KeyModifiers::NONE),
            Global::View(View::Diagnostics)
        );
        assert_eq!(
            global_key(KeyCode::Char('k'), both),
            Global::View(View::Assistant)
        );
        assert_eq!(global_key(KeyCode::Char('l'), ctrl), Global::ClearTerminal);
        assert_eq!(global_key(KeyCode::Char('q'), ctrl), Global::Quit);
        assert_eq!(global_key(KeyCode::Up, ctrl), Global::FocusSidebar);
        assert_eq!(global_key(KeyCode::Char('r'), both), Global::ArmShift);
        assert_eq!(global_key(KeyCode::Char('c'), both), Global::ArmCtrl);
        assert_eq!(global_key(KeyCode::Char('a'), both), Global::ArmAlt);
    }

    #[test]
    fn plain_keys_stay_free_for_the_target_shell() {
        // Anything the TUI does not bind must reach the terminal: this is
        // what keeps `?`, `q` and Ctrl+C usable inside the shell.
        assert_eq!(
            global_key(KeyCode::Char('q'), KeyModifiers::NONE),
            Global::None
        );
        assert_eq!(
            global_key(KeyCode::Char('?'), KeyModifiers::SHIFT),
            Global::None
        );
        assert_eq!(
            global_key(KeyCode::Char('c'), KeyModifiers::CONTROL),
            Global::None
        );
        assert_eq!(global_key(KeyCode::Enter, KeyModifiers::NONE), Global::None);
        assert_eq!(global_key(KeyCode::Esc, KeyModifiers::NONE), Global::None);
        assert_eq!(global_key(KeyCode::F(12), KeyModifiers::NONE), Global::None);
    }
}
