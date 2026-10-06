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
pub mod drawsync;
pub mod i18n;
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
use crate::lan_token_store;
use crate::session::{CoreBus, SessionHandle};
use crate::watch::{SerialWatch, WatchOptions};

use self::dialogs::Dialog;
use self::i18n::{strings, t, tr};
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

/// Consecutive terminal I/O failures the frame loop rides out before it gives
/// up on the console. One bad read used to tear the whole session down — which
/// is how dragging the window to its minimum size killed the Windows TUI (K2):
/// conhost errors while it reflows, and the next poll usually succeeds again.
const IO_FAILURE_LIMIT: u32 = 5;

/// Counts consecutive console failures so a burst is reported once and retried
/// instead of fatal. `failed` answers with the failure's position in the burst
/// (`Some(1)` = report me) or `None` once the limit is passed.
#[derive(Debug, Default)]
struct IoBudget {
    consecutive: u32,
}

impl IoBudget {
    fn failed(&mut self) -> Option<u32> {
        self.consecutive += 1;
        (self.consecutive <= IO_FAILURE_LIMIT).then_some(self.consecutive)
    }

    fn ok(&mut self) {
        self.consecutive = 0;
    }
}

/// Record a console failure. Returns `true` when the session must stop.
///
/// The first failure of a burst surfaces as an error notice (so a genuinely
/// broken console is still visible), the rest are absorbed, and the loop only
/// gives up when the console keeps failing — plus a short pause so a wedged
/// console cannot turn the frame loop into a busy spin.
fn io_failure(app: &mut App, budget: &mut IoBudget, text: String) -> bool {
    let keep_going = match budget.failed() {
        None => return true,
        Some(1) => {
            app.notices.push(NoticeLevel::Error, text);
            false
        }
        Some(_) => false,
    };
    std::thread::sleep(Duration::from_millis(40));
    keep_going
}

// Notices the event loop raises. The `linkr: …` lines further down are *not*
// here: they are plain stderr, written before the alternate screen is
// entered or after it is left, and the CLI half of the binary stays English.
strings! {
    MOD_CONNECTED_SHORT => "connected", "已连接";
    MOD_NOT_CONNECTED => "not connected", "未连接";
    MOD_DROPPED => "{} session events dropped.", "丢失了 {} 条会话事件。";
    MOD_INPUT_FAILED => "Terminal input failed: {}", "终端输入失败：{}";
    MOD_DRAW_FAILED => "Draw failed: {}", "画面绘制失败：{}";
    MOD_DISCONNECTED => "Disconnected: {}", "连接已断开：{}";
    MOD_CONN_FAILED => "Connection failed: {}", "连接失败：{}";
    MOD_CLEARED => "Terminal cleared.", "终端已清屏。";
    MOD_NEED_BLE_DIAG => "Connect over BLE to read diagnostics.", "请通过 BLE 连接后再读取诊断。";
    MOD_CJK_WIDE => "CJK probe: glyphs take two columns here.",
        "CJK 探测：本终端把宽字形按 2 列绘制。";
    MOD_CJK_NARROW => "CJK probe: this console counts CJK as one column — backing up the cells it skips.",
        "CJK 探测：本控制台把中文按 1 列计宽，已补写被跳过的格子。";
    MOD_CJK_ASSUMED => "CJK probe: no answer, drawing as if glyphs take two columns.",
        "CJK 探测：未取得回答，按 2 列绘制。";
}

type Ui = Terminal<drawsync::SyncBackend<CrosstermBackend<std::io::Stdout>>>;

/// Render context passed from the session after connect.
pub struct TuiContext {
    pub session: SessionHandle,
    pub bus: CoreBus,
    /// Connect the CLI deferred (A5): the target to dial once the screen is
    /// up. `None` when the process already holds a live session, so `--tui`
    /// with queries/loopback keeps connecting first.
    pub pending: Option<PendingConnect>,
}

/// The deferred connect handed over by `cli::drive`.
pub struct PendingConnect {
    pub opts: crate::session::SessionOptions,
    pub setup: crate::session::SessionSetup,
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
///
/// It also opts into the kitty keyboard protocol. Without it the legacy
/// encoding cannot tell `Ctrl+Enter` from `Enter` — both are the single byte
/// `0x0d` — so "Ctrl+Enter sends" was undecodable no matter what the code
/// looked for. Terminals that do not implement the protocol (GNOME Terminal /
/// VTE, tracked upstream as vte#2601) are required to ignore `CSI > 1 u`, so
/// they lose nothing here and use the `Alt+Enter` fallback instead. The
/// support *query* is deliberately not sent: awaiting its reply stalls startup
/// for 2s on every terminal that never answers it.
struct ScreenGuard {
    /// We pushed `CSI > 1 u`, so we owe the terminal a matching `CSI < 1 u`.
    keyboard_enhanced: bool,
}

impl ScreenGuard {
    fn new() -> Self {
        let keyboard_enhanced = crossterm::execute!(
            stdout(),
            crossterm::event::PushKeyboardEnhancementFlags(
                crossterm::event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES,
            )
        )
        .is_ok();
        Self { keyboard_enhanced }
    }
}

impl Drop for ScreenGuard {
    fn drop(&mut self) {
        let mut out = stdout();
        if self.keyboard_enhanced {
            let _ = crossterm::execute!(out, crossterm::event::PopKeyboardEnhancementFlags);
        }
        let _ = crossterm::execute!(out, crossterm::event::DisableBracketedPaste);
        let _ = crossterm::execute!(out, LeaveAlternateScreen);
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(out, crossterm::cursor::Show);
    }
}

/// Hand stderr-backed diagnostics back to the console once the TUI is gone.
struct DiagnosticSinkGuard;

impl Drop for DiagnosticSinkGuard {
    fn drop(&mut self) {
        crate::cli::set_diagnostic_sink(None);
    }
}

fn ui_session(rt: Arc<tokio::runtime::Runtime>, ctx: TuiContext) -> i32 {
    if !crate::term::stdin_is_tty() {
        eprintln!("linkr: the TUI needs an interactive terminal");
        return 1;
    }
    // G3: crossterm takes one 1024-byte read per wake-up and returns with the
    // first parseable event, so a longer paste left the tail in the tty queue
    // with no edge for mio's epoll to report — it only moved on the next
    // keypress. The pump makes every read drain the queue. Declared before
    // `raw` so the real terminal is restored last, once crossterm is done
    // with the pty in front of it.
    let _pump = crate::term::InputPump::install();
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
    let _screen = ScreenGuard::new();
    // A click must not be able to freeze us: conhost's Quick Edit mode stops
    // painting the window and eats the keystrokes while a selection is live
    // (F2). Restored on drop, once the TUI is gone.
    let _quick_edit = crate::term::QuickEditGuard::disable();

    // Which way this console measures a CJK glyph decides whether a frame has
    // to paint the shadow column `Buffer::diff` skips (see `drawsync`).
    // `Terminal::clear()` below wipes the probe's scratch row with everything
    // else the screen had.
    let probe = drawsync::probe_cjk_width();

    let mut terminal = match Terminal::new(drawsync::SyncBackend::new(
        CrosstermBackend::new(stdout()),
        probe.width(),
    )) {
        Ok(terminal) => terminal,
        Err(err) => {
            eprintln!("linkr: cannot attach to the terminal: {err}");
            return 1;
        }
    };
    let _ = terminal.clear();

    let code = event_loop(&mut terminal, rt, ctx, probe);
    let _ = terminal.show_cursor();
    code
}

// --- state -------------------------------------------------------------------

fn build_app(rt: Arc<tokio::runtime::Runtime>, session: SessionHandle, bus: CoreBus) -> App {
    let settings = settings::load();
    let lan_host = settings.last_lan_host.clone();
    let info = session.info();
    let state = if info.connected {
        ConnectionState::Connected
    } else {
        ConnectionState::Disconnected
    };
    let lang = settings.lang;
    let detail = if info.label.is_empty() {
        match state {
            ConnectionState::Connected => t(MOD_CONNECTED_SHORT, lang).to_string(),
            _ => t(MOD_NOT_CONNECTED, lang).to_string(),
        }
    } else {
        info.label.clone()
    };
    let view = settings.active_view;
    let focus = match view {
        View::Assistant => Focus::Assistant,
        _ => Focus::Center,
    };
    let mut app = App {
        session,
        bus,
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
        settle_repaint_at: None,
        pending_connect: None,
        pending_scan: None,
        socket_query: None,
        lan_device: None,
        center_height: 24,
        center_width: 80,
        screen_height: 24,
        center_scroll: 0,
    };
    // web `restore()`: the token field follows the saved host, so a session
    // that starts on the bridge dials with what a BLE session captured.
    let store = lan_token_store::TokenStore::load();
    sidebar::fill_token_from_store(&mut app, &store);
    if app.state == ConnectionState::Connected {
        // The CLI can hand the TUI a session that is already up; nothing else
        // would ask for the token then.
        start_lan_token_capture(&mut app);
    }
    app
}

// --- event loop --------------------------------------------------------------

/// The probe verdict worth telling the user about, and the notice that says it.
///
/// `None` on a console that answered nothing and is not Windows: POSIX
/// terminals already draw two columns, so "no news" is news only where it can
/// mean a broken CJK rendering.
fn cjk_notice(probe: drawsync::Probe) -> Option<crate::tui::i18n::Entry> {
    use drawsync::{CjkWidth, Probe};
    match probe {
        Probe::Measured(CjkWidth::Wide) => Some(MOD_CJK_WIDE),
        Probe::Measured(CjkWidth::Narrow) => Some(MOD_CJK_NARROW),
        Probe::Assumed if cfg!(windows) => Some(MOD_CJK_ASSUMED),
        Probe::Assumed => None,
    }
}

fn event_loop(
    terminal: &mut Ui,
    rt: Arc<tokio::runtime::Runtime>,
    ctx: TuiContext,
    probe: drawsync::Probe,
) -> i32 {
    let TuiContext {
        session,
        bus,
        pending,
    } = ctx;
    let mut app = build_app(rt, session, bus);
    if let Some(entry) = cjk_notice(probe) {
        app.toast(NoticeLevel::Info, t(entry, app.lang()).to_string());
    }
    app.terminal.set_autoscroll(app.settings.autoscroll);
    let mut core = app.bus.subscribe();
    let mut sticky = StickyMods::default();
    let mut io_budget = IoBudget::default();

    // Everything the transport and the session print (`linkr: connected: …`,
    // the multi-match warning, ignored-frame notices) has to reach the notice
    // log: ratatui diffs its own buffer and would never repaint the cells a
    // stray `eprintln!` overwrote, leaving text stuck on the interface.
    let (diagnostic_tx, diagnostic_rx) = std::sync::mpsc::channel();
    crate::cli::set_diagnostic_sink(Some(diagnostic_tx));
    let _sink_guard = DiagnosticSinkGuard;

    // 0. The CLI's deferred connect: start it only now, so the radio work
    //    never delays (and never prevents) the interface coming up, and its
    //    `linkr: …` chatter lands in the notice log above.
    if let Some(pending) = pending {
        connect::begin_cli(&mut app, pending.opts, pending.setup);
    }

    while !app.quit {
        // An overlay covers a large slice of the frame: if one opens or
        // closes during this iteration the screen is repainted in full, so a
        // console that repainted underneath it cannot leave the panel's text
        // behind (K5).
        let overlay_before = app.overlay_open();
        // …and the same for the sidebar's link rows: switching between WiFi
        // and Bluetooth rewrites them wholesale (K6).
        let link_before = app.link_signature();

        // 1. Session bus: UART output, connection lifecycle, notices.
        loop {
            match core.try_recv() {
                Ok(event) => on_core_event(&mut app, event),
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(dropped)) => {
                    let lang = app.lang();
                    app.notices
                        .push(NoticeLevel::Warn, tr!(t(MOD_DROPPED, lang), dropped));
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Closed) => break,
            }
        }

        // 1b. Transport/session diagnostics raised while the screen was ours.
        while let Ok((level, text)) = diagnostic_rx.try_recv() {
            app.notices.push(level, text);
        }

        // 2. Non-blocking work parked on oneshots / broadcast receivers.
        connect::poll(&mut app);
        connect::poll_scan(&mut app);
        if app.dialog.is_none() && app.palette.is_none() {
            if let Some(pending) = app.take_approval() {
                app.dialog = Some(Dialog::Approval(Box::new(pending)));
            }
        }
        dialogs::poll(&mut app);
        network_view::poll(&mut app);
        app.diagnostics.poll(app.lang());
        poll_lan_token(&mut app);
        assistant_view::poll(&mut app);
        app.notices.tick();
        app.refresh_info();
        auto_diagnostics(&mut app);
        prefill_lan_host(&mut app);

        // 3. Terminal input (blocked at most one frame per key press).
        match event::poll(TICK) {
            Ok(true) => match event::read() {
                Ok(Event::Key(key)) if key.kind != KeyEventKind::Release => {
                    io_budget.ok();
                    handle_key(&mut app, &mut sticky, key)
                }
                Ok(Event::Resize(_, _)) => {
                    io_budget.ok();
                    app.schedule_resize_repaint(Instant::now());
                }
                Ok(Event::Paste(text)) => {
                    io_budget.ok();
                    paste(&mut app, &text);
                }
                Ok(_) => io_budget.ok(),
                Err(err) => {
                    let lang = app.lang();
                    let text = tr!(t(MOD_INPUT_FAILED, lang), err);
                    if io_failure(&mut app, &mut io_budget, text) {
                        break;
                    }
                }
            },
            Ok(false) => io_budget.ok(),
            Err(err) => {
                let lang = app.lang();
                let text = tr!(t(MOD_INPUT_FAILED, lang), err);
                if io_failure(&mut app, &mut io_budget, text) {
                    break;
                }
            }
        }

        // 4. Geometry: the VT grid follows the pane, the session follows the
        //    grid (web `terminal_geometry.js` debounce, here instant).
        let area = match terminal.size() {
            Ok(size) => Rect::new(0, 0, size.width, size.height),
            Err(_) => Rect::new(0, 0, 80, 24),
        };
        sync_geometry(&mut app, area);

        // 5. Draw. A forced repaint drops ratatui's model of the screen first:
        //    the console may have repainted itself behind our back (a Windows
        //    conhost reflows its buffer when the window changes size), and the
        //    cell diff would otherwise keep skipping every cell it believes is
        //    already correct — stale frames then stay up forever (F2). The
        //    settled repaint scheduled by the resize event lands here as well:
        //    by then the console has stopped moving, so this frame sticks (K1).
        app.sync_overlay_repaint(overlay_before);
        app.sync_link_repaint(link_before);
        app.poll_settle_repaint(Instant::now());
        if app.take_force_redraw() {
            let _ = terminal.clear();
        }
        match terminal.draw(|frame| layout::draw(frame, &app)) {
            Ok(_) => io_budget.ok(),
            Err(err) => {
                let lang = app.lang();
                let text = tr!(t(MOD_DRAW_FAILED, lang), err);
                if io_failure(&mut app, &mut io_budget, text) {
                    break;
                }
            }
        }
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
            let lang = app.lang();
            match state {
                ConnectionState::Disconnected => {
                    app.notices
                        .push(NoticeLevel::Info, tr!(t(MOD_DISCONNECTED, lang), detail));
                }
                ConnectionState::Failed => {
                    app.notices
                        .push(NoticeLevel::Error, tr!(t(MOD_CONN_FAILED, lang), detail));
                }
                _ => {}
            }
        }
        CoreEvent::Notice { level, text } => app.notices.push(level, text),
        // Scan results arrive as live events while the request waits for
        // `@scan done` (web `handleWifiScanLine`, CLI `pump_until`); the rest
        // of the management traffic keeps its own owner.
        CoreEvent::MgmtMessage { lines, .. } => network_view::on_mgmt_event(app, &lines),
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
        app.sidebar.lan_host.set(ip.clone());
        // The alias is the second half of the capture: the web has the token
        // and the IP in hand for one `lanTokens.capture(deviceId, token, ip)`,
        // ours arrive on their own schedules (`@s?` may still be in flight,
        // in which case the reply above writes the alias instead).
        let token = app.sidebar.lan_token.text.clone();
        if let (Some(device), false) = (app.lan_device.clone(), token.is_empty()) {
            let mut store = lan_token_store::TokenStore::load();
            let _ = store.capture(&device, &token, &ip);
            let _ = store.save();
        }
    }
}

/// `requestDeviceState()` when a BLE session comes up: diagnostics — they
/// carry the IP the store's host alias needs — plus `@s?`, the one line the
/// bridge reports its LAN access token on (web comment: "*capturing it during
/// the BLE session means the token field is already filled when the user
/// switches to LAN mode*").
///
/// Management commands travel over BLE only, and `@s?` is asked only when the
/// handshake advertised the bridge (web
/// `hasManagementCapability(MGMT_CAP_WEBSOCKET)`).
fn start_lan_token_capture(app: &mut App) {
    if !app.ble_connected() {
        return;
    }
    app.lan_device = app.info.device_id.clone();
    // web: `elements.wsTokenInput.value = lanTokens.selectDevice(deviceId)` —
    // a token belongs to the device that issued it, never to the last one.
    if let Some(device) = app.lan_device.clone() {
        let store = lan_token_store::TokenStore::load();
        app.sidebar
            .lan_token
            .set(store.select_device(&device).unwrap_or_default());
    }
    app.diagnostics.refresh(&app.session);
    if let Some(command) = lan_token_store::socket_query_command(app.info.capabilities) {
        app.socket_query = Some(app.session.request_mgmt(command.to_string(), None));
    }
}

/// Drain the `@s?` reply without blocking a frame (web
/// `handleSocketStatusLine`): the token goes to the field and to the store —
/// it is never drawn, never logged, and the field only takes it when the
/// store handed that value out itself.
fn poll_lan_token(app: &mut App) {
    let Some(rx) = &mut app.socket_query else {
        return;
    };
    let outcome = match rx.try_recv() {
        Ok(outcome) => outcome,
        Err(tokio::sync::oneshot::error::TryRecvError::Empty) => return,
        Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
            app.socket_query = None;
            return;
        }
    };
    app.socket_query = None;
    let reply = match outcome {
        Ok(reply) => reply,
        Err(error) => {
            // A device that refuses the question says why; the 5 s timeout
            // arrives empty and there is nothing worth telling.
            if !error.is_empty() {
                app.toast(NoticeLevel::Warn, error);
            }
            return;
        }
    };
    // The web runs its parser over every line it receives and lets each match
    // overwrite the field, so the last status line wins here too — the bridge
    // really does answer with `token=none` before the line that carries the
    // token.
    let lines: Vec<&str> = reply
        .lines
        .iter()
        .chain(reply.events.iter())
        .map(String::as_str)
        .collect();
    let Some(token) = lan_token_store::token_from_reply(lines) else {
        return;
    };
    let Some(device) = app.lan_device.clone() else {
        return;
    };
    let host = app.sidebar.lan_host.text.clone();
    let mut store = lan_token_store::TokenStore::load();
    let previous = store.select_device(&device).map(str::to_string);
    let replace =
        lan_token_store::may_replace_field(app.sidebar.lan_token.as_str(), previous.as_deref());
    if store.capture(&device, token, &host).is_some() {
        // The web store writes inside `capture()`; ours takes the caller's
        // word for which file it belongs to.
        let _ = store.save();
        if replace {
            app.sidebar.lan_token.set(token);
        }
    }
}

/// Keep the grid, the pane and the session geometry in sync.
fn sync_geometry(app: &mut App, area: Rect) {
    let (_top, body, _bottom) = layout::zones(area);
    app.screen_height = area.height;
    let (_sidebar, center) = layout::columns(body, area.width >= 60);
    app.center_height = center.height;
    app.center_width = center.width;
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
            app.palette = Some(PaletteState::new(app.lang()));
            return;
        }
        Global::ClearTerminal => {
            app.terminal.clear();
            // Clearing the pane's scrollback is only half the job. conhost can
            // repaint its window behind ratatui's back — a click drops the
            // console into Quick Edit (`选择`) mode, which freezes painting
            // while we keep drawing — and the cell diff then skips every cell
            // it believes is already correct, so those stale rows stay on
            // screen forever. Ask the event loop for a real `terminal.clear()`
            // as well, and Ctrl+L recovers the *screen*, not just the pane.
            app.force_redraw = true;
            app.toast(NoticeLevel::Info, t(MOD_CLEARED, app.lang()).to_string());
            return;
        }
        Global::Help => {
            app.dialog = Some(Dialog::Help(0));
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

    if handle_center_scroll(app, key) {
        return;
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

/// Diagnostics keys: `r` re-reads `@i?`.
fn handle_diagnostics_key(app: &mut App, key: crossterm::event::KeyEvent) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('r') if !ctrl => {
            if app.ble_connected() {
                app.diagnostics.done = false;
                let session = app.session.clone();
                app.diagnostics.refresh(&session);
            } else {
                app.toast(
                    NoticeLevel::Warn,
                    t(MOD_NEED_BLE_DIAG, app.lang()).to_string(),
                );
            }
            true
        }
        _ => false,
    }
}

/// PgUp/PgDn page the non-terminal center views (Diagnostics, Network). The
/// offset counts lines skipped from the top, so it is clamped to the content:
/// an unclamped value walked the pane blank, and the directions were the
/// wrong way round before.
fn handle_center_scroll(app: &mut App, key: crossterm::event::KeyEvent) -> bool {
    if app.focus != Focus::Center || !matches!(app.view, View::Diagnostics | View::Network) {
        return false;
    }
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return false;
    }
    let step = (app.center_height / 2).max(1);
    let limit = layout::center_scroll_limit(app);
    let up = match key.code {
        KeyCode::PageUp => true,
        KeyCode::PageDown => false,
        _ => return false,
    };
    app.center_scroll = layout::page_scroll(app.center_scroll, limit, step, up);
    true
}

/// F3 / palette `diag.refresh`: switch and query (BLE-gated, §3.6 matrix).
fn open_diagnostics(app: &mut App) {
    app.set_view(View::Diagnostics);
    app.diagnostics.done = false;
    if app.ble_connected() {
        let session = app.session.clone();
        app.diagnostics.refresh(&session);
    } else {
        app.toast(
            NoticeLevel::Warn,
            t(MOD_NEED_BLE_DIAG, app.lang()).to_string(),
        );
    }
}

// --- tests -------------------------------------------------------------------

/// An [`App`] with no transport behind it: enough to render a pane and assert
/// on what the user actually sees. Shared by the view tests so each of them
/// can compare its rendered rows against its own key handling.
#[cfg(test)]
pub(crate) fn test_app() -> App {
    App {
        session: SessionHandle::test_detached(),
        bus: CoreBus::new(),
        rt: Arc::new(tokio::runtime::Runtime::new().expect("runtime")),
        settings: self::settings::TuiSettings::default(),
        state: ConnectionState::Connected,
        detail: String::new(),
        info: crate::session::SessionInfo::default(),
        view: View::Terminal,
        focus: Focus::Center,
        sidebar: SidebarState::new(String::new(), String::new()),
        terminal: TerminalPane::new(true),
        diagnostics: self::diagnostics_view::DiagnosticsState::default(),
        network: self::network_view::NetworkState::new(),
        assistant: self::assistant_view::AssistantState::default(),
        palette: None,
        dialog: None,
        notices: Notices::default(),
        exec_mode: crate::agent::ExecMode::Auto,
        broker: self::dialogs::TuiBroker::new(),
        agent: None,
        watch: SerialWatch::new(WatchOptions::default()),
        watch_ok: true,
        quit: false,
        started: Instant::now(),
        force_redraw: false,
        settle_repaint_at: None,
        pending_connect: None,
        pending_scan: None,
        socket_query: None,
        lan_device: None,
        center_height: 24,
        center_width: 80,
        screen_height: 24,
        center_scroll: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::settings::EnterMode;

    #[test]
    fn every_loop_notice_is_translated() {
        super::i18n::assert_bilingual(ALL);
        assert!(ALL.len() >= 9, "the event loop raises 9 notices");
    }

    #[test]
    fn the_cjk_probe_verdict_reaches_the_notice_log() {
        use drawsync::{CjkWidth, Probe};
        assert_eq!(
            cjk_notice(Probe::Measured(CjkWidth::Narrow)),
            Some(MOD_CJK_NARROW),
            "a console that counts CJK as one column has to say so"
        );
        assert_eq!(
            cjk_notice(Probe::Measured(CjkWidth::Wide)),
            Some(MOD_CJK_WIDE)
        );
        // On POSIX consoles "no answer" is the normal case, not news — only a
        // Windows one can mean CJK is about to render wrong.
        assert_eq!(
            cjk_notice(Probe::Assumed),
            if cfg!(windows) {
                Some(MOD_CJK_ASSUMED)
            } else {
                None
            }
        );
    }

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

    /// K2: dragging the Windows console to its minimum size made conhost fail a
    /// read once — and one failure used to end the session. A burst must be
    /// reported once, ridden out, and only a console that keeps failing is
    /// worth giving up on.
    #[test]
    fn only_the_first_failure_of_a_burst_is_reported() {
        let mut app = crate::tui::test_app();
        let mut budget = IoBudget::default();
        assert!(!io_failure(&mut app, &mut budget, "first".into()));
        assert!(!io_failure(&mut app, &mut budget, "second".into()));
        assert_eq!(app.notices.log.len(), 1, "the burst surfaces once");
        assert_eq!(app.notices.log[0].1, "first");
    }

    /// K2: the session ends only after [`IO_FAILURE_LIMIT`] failures in a row.
    #[test]
    fn the_loop_gives_up_only_when_the_console_keeps_failing() {
        let mut app = crate::tui::test_app();
        let mut budget = IoBudget::default();
        for round in 0..IO_FAILURE_LIMIT {
            assert!(
                !io_failure(&mut app, &mut budget, format!("hit {round}")),
                "failure {round} of {IO_FAILURE_LIMIT} must be tolerated"
            );
        }
        assert!(
            io_failure(&mut app, &mut budget, "fatal".into()),
            "a console that never recovers ends the session"
        );
    }

    /// K2: one good read ends the burst, so a rare glitch minutes apart is
    /// never counted towards the limit.
    #[test]
    fn one_good_read_clears_the_failure_burst() {
        let mut budget = IoBudget::default();
        assert_eq!(budget.failed(), Some(1));
        budget.ok();
        assert_eq!(budget.failed(), Some(1), "the burst restarts from one");
    }

    /// F2: Ctrl+L has to repaint the **screen**, not only empty the pane's
    /// scrollback. conhost can repaint its own window behind ratatui's back
    /// (a click drops the console into Quick Edit / `选择` mode, which freezes
    /// painting while we keep drawing), and the cell diff then skips every
    /// cell it believes is already correct — those stale rows would otherwise
    /// stay up for the rest of the session.
    #[test]
    fn ctrl_l_clears_the_pane_and_arms_a_full_repaint() {
        let mut app = crate::tui::test_app();
        assert!(!app.force_redraw);

        let mut sticky = StickyMods::default();
        handle_key(
            &mut app,
            &mut sticky,
            crossterm::event::KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL),
        );

        assert!(app.force_redraw, "Ctrl+L must ask for a full repaint");
        assert!(
            !app.notices.toasts.is_empty(),
            "…and the clear must still be announced"
        );
    }
}
