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
pub mod clipboard;
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
pub mod transfer_view;

use std::io::stdout;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
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
    // Said when `Ctrl+Shift+V` could not read the system clipboard, which is
    // the only way a TUI can fail that key: the web wording without its
    // touch-screen advice (`web/app.js` → `pasteUnavailable`).
    MOD_PASTE_UNAVAILABLE => "Clipboard unavailable. Use your terminal's paste key.",
        "无法读取剪贴板，请改用你终端的粘贴键。";
    // Said when an inbound `OSC 52` asked this machine to copy something and
    // no clipboard helper took it: a headless box, or a desktop without
    // `wl-copy` / `xclip` / `xsel`. Silent there would leave the device
    // believing the copy landed.
    MOD_CLIP_WRITE_FAILED => "The device asked to copy to this machine's clipboard; no clipboard helper is available.",
        "设备请求写入本机剪贴板，但本机没有可用的剪贴板工具。";
    // Said once per session, right before the OSC 52 news, when *our own*
    // copy found no helper either — the reason the text did not reach the
    // system clipboard, and the fix. VTE-based terminals (GNOME Terminal and
    // friends) parse `OSC 52` and do nothing with it (GNOME bug 795774), so
    // without a helper there is no second way in.
    MOD_COPY_NO_HELPER => "No clipboard helper here (wl-clipboard / xclip), so the system clipboard was not written.",
        "本机没有剪贴板工具（wl-clipboard / xclip），系统剪贴板没有被写入。";
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
        let _ = crossterm::execute!(out, crossterm::event::DisableMouseCapture);
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
    if let Err(err) = crossterm::execute!(
        stdout(),
        EnterAlternateScreen,
        event::EnableBracketedPaste,
        // Mouse capture is what makes the pane able to hold its own selection
        // (`handle_mouse`): the host stops reporting clicks to itself, and the
        // drag — not the host's — is what gets copied on release.
        event::EnableMouseCapture
    ) {
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
        transfer: transfer_view::State::default(),
        palette: None,
        dialog: None,
        dialog_return: None,
        notices: Notices::default(),
        exec_mode: crate::agent::ExecMode::Auto,
        exec_expires_at: 0,
        pending_paste: None,
        clipboard_jobs: Vec::new(),
        copy_hint_shown: false,
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
        center_x: 0,
        center_y: 0,
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
        // Time and bytes for a transfer, whatever is on screen: it types at
        // its own pace and a view switch must not stall the link.
        transfer_view::poll(&mut app);
        app.diagnostics.poll(app.lang());
        poll_lan_token(&mut app);
        assistant_view::poll(&mut app);
        app.notices.tick();
        app.refresh_info();
        auto_diagnostics(&mut app);
        prefill_lan_host(&mut app);
        // Host edits reach disk only once the field has sat still (see
        // `sidebar::HOST_PERSIST_SETTLE`); this is where the pause runs them.
        sidebar::poll(&mut app, Instant::now());
        poll_clipboard(&mut app, &mut sticky);

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
                    paste(&mut app, &mut sticky, &text);
                }
                Ok(Event::Mouse(mouse)) => {
                    io_budget.ok();
                    handle_mouse(&mut app, &mut sticky, mouse);
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
    // First, a host typed in the last `HOST_PERSIST_SETTLE` has not reached
    // `tui.json` yet — it must not be dropped with the process.
    sidebar::flush(&mut app);
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
            // While a transfer captures, the link belongs to it. A ZDATA
            // frame is not text — painting it would garble the pane, and
            // feeding binary to the serial watch would invent findings out
            // of a CRC — so the grid, the watch and the pending-reply
            // machinery are all starved until the run says otherwise.
            if app.transfer.engine.capturing() {
                app.transfer.engine.on_rx(&bytes);
                return;
            }
            let at_ms = app.started.elapsed().as_millis() as u64;
            if app.watch_ok {
                app.watch.feed_bytes(&bytes, at_ms);
            }
            app.terminal.feed(&bytes);
            // Two things the device is still waiting on used to pile up in
            // the grid with no caller anywhere in the crate: the terminal's
            // own answers to `DSR`/`CPR` (`take_pending_reports`, so a program
            // asking where the cursor was waited forever) and the payload of
            // an inbound `OSC 52` (`take_clipboard`, so a device setting the
            // host clipboard did nothing).
            let reports = app.terminal.grid.take_pending_reports();
            if !reports.is_empty() {
                app.send_reply(reports);
            }
            if let Some(payload) = app.terminal.grid.take_clipboard() {
                // A worker, not `clipboard::write`: this runs on the frame
                // loop, and a helper that is not answering must not be what
                // stops the terminal from drawing. `poll_clipboard` raises the
                // "no helper" notice when it comes back.
                app.clipboard_jobs.push(clipboard::ClipboardJob::for_device(
                    clipboard::spawn_write(payload),
                ));
            }
        }
        CoreEvent::Connection { state, detail } => {
            app.info = app.session.info();
            app.set_connection_state(state);
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
        let token = app.sidebar.lan_token.as_str().to_string();
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
    let host = app.sidebar.lan_host.as_str().to_string();
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
    app.center_x = center.x;
    app.center_y = center.y;
    let (cols, rows) =
        terminal_view::grid_dims(center.width, center.height, app.settings.font_size);
    if app.terminal.sync_size(cols, rows) {
        app.session.set_terminal_size(cols, rows);
        app.force_redraw = true;
    }
}

/// Mouse input: a drag in the terminal pane makes a selection of its own, and
/// the wheel scrolls whatever is under the pointer.
///
/// The host terminal's own selection is out of reach once mouse capture is on
/// (`EnableMouseCapture`) — and a repaint would have wiped it anyway, which is
/// exactly the "I selected it, then it was gone" of P8 — so the selection
/// lives here, in grid coordinates, and **releasing the button** is what
/// copies it. That is the web's `copyBtn` contract (`web/app.js:3559`: read
/// the selection, `clipboard.writeText`, toast) with the button replaced by
/// the gesture itself.
fn handle_mouse(app: &mut App, sticky: &mut StickyMods, mouse: MouseEvent) {
    if matches!(
        mouse.kind,
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
    ) {
        let key = wheel_key(app, mouse.kind == MouseEventKind::ScrollUp);
        handle_key(app, sticky, key);
        return;
    }
    // The release ends the gesture wherever it lands, so it is handled
    // **before** the overlay guard below: that guard swallows the event when
    // a box opens over the pane (or the view switches) mid-drag, which left
    // `dragging` set — and the next stray release then re-sent a selection
    // the user had already dismissed, the exact thing
    // `a_release_with_no_press_behind_it_never_re_sends…` forbids, one path
    // further along.
    if matches!(mouse.kind, MouseEventKind::Up(MouseButton::Left)) && app.terminal.take_dragging() {
        finish_selection(app);
        return;
    }
    // Only the terminal pane has a grid to select in, and an overlay in front
    // keeps its own clicks (Esc / Enter / the arrows still get through).
    if app.dialog.is_some() || app.palette.is_some() || app.view != View::Terminal {
        return;
    }
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => match pane_cell(app, mouse.column, mouse.row) {
            Some(cell) => {
                // A click in the pane takes the keys back from wherever they
                // were: selecting and typing belong to the same pane.
                app.focus = Focus::Center;
                app.terminal.begin_selection(cell.0, cell.1);
            }
            // A press *outside* the pane can only be the residue of a gesture
            // whose release never reached us (a terminal need not report one
            // let go beyond its window). It dies here rather than stretching
            // over the sidebar and back into the pane.
            None => {
                app.terminal.take_dragging();
            }
        },
        MouseEventKind::Drag(MouseButton::Left) if app.terminal.is_dragging() => {
            let cell = pane_cell_clamped(app, mouse.column, mouse.row);
            app.terminal.extend_selection(cell.0, cell.1);
        }
        // The release itself is handled above the guard: a press with no
        // gesture behind it falls through here and does nothing.
        _ => {}
    }
}

/// Copy what the drag covered — or, for a press that never moved, drop the
/// highlight and copy nothing. A click is how a selection is dismissed, so it
/// has to stay free of side effects.
///
/// Returns what was copied, which is what the tests read: the escape sequence
/// itself goes to stdout, on its way to the emulator running us.
fn finish_selection(app: &mut App) -> Option<String> {
    match app.terminal.selection_text() {
        Some(text) if !text.is_empty() => {
            copy_to_host(app, &text);
            Some(text)
        }
        // A click, a drag that never left its cell, or no selection at all:
        // nothing to send, and the highlight goes away — that is how a
        // selection is dismissed.
        _ => {
            app.terminal.clear_selection();
            None
        }
    }
}

/// What the wheel presses, so scrolling has one code path with the keys: the
/// terminal scrolls its own scrollback (which needs `Shift`), the lists take
/// the plain paging keys, the sidebar and the palette take the arrows.
fn wheel_key(app: &App, up: bool) -> KeyEvent {
    let code = if up {
        KeyCode::PageUp
    } else {
        KeyCode::PageDown
    };
    let plain = KeyModifiers::NONE;
    if app.palette.is_some() {
        return KeyEvent::new(if up { KeyCode::Up } else { KeyCode::Down }, plain);
    }
    match app.focus {
        Focus::Sidebar => KeyEvent::new(if up { KeyCode::Up } else { KeyCode::Down }, plain),
        Focus::Center if app.view == View::Terminal => KeyEvent::new(code, KeyModifiers::SHIFT),
        _ => KeyEvent::new(code, plain),
    }
}

/// Screen coordinates → grid cell, `None` when the point is not on one.
fn pane_cell(app: &App, column: u16, row: u16) -> Option<(usize, usize)> {
    if column < app.center_x || row < app.center_y {
        return None;
    }
    if column >= app.center_x.saturating_add(app.center_width)
        || row >= app.center_y.saturating_add(app.center_height)
    {
        return None;
    }
    grid_cell(app, column - app.center_x, row - app.center_y)
}

/// The same mapping clamped into the pane: a drag or a release that runs past
/// the edge keeps the far end of the selection *at* that edge instead of
/// losing it the moment the pointer leaves the pane.
fn pane_cell_clamped(app: &App, column: u16, row: u16) -> (usize, usize) {
    let (cols, rows) =
        terminal_view::grid_dims(app.center_width, app.center_height, app.settings.font_size);
    let x = column
        .saturating_sub(app.center_x)
        .min(cols.saturating_sub(1));
    let y = row.saturating_sub(app.center_y).min(rows.saturating_sub(1));
    grid_cell(app, x, y).unwrap_or((0, 0))
}

/// Pane-relative coordinates → (absolute grid row, display column).
fn grid_cell(app: &App, x: u16, y: u16) -> Option<(usize, usize)> {
    let (cols, rows) =
        terminal_view::grid_dims(app.center_width, app.center_height, app.settings.font_size);
    let row = app.terminal.grid.row_at(rows, app.terminal.offset, y)?;
    // With a font larger than the default the grid is narrower than the pane
    // it is drawn in; a click past its last column belongs to no cell.
    Some((row, usize::from(x.min(cols.saturating_sub(1)))))
}

/// Get text out of this program, by both routes that exist.
///
/// `OSC 52` is addressed to the emulator we run inside, so it goes to stdout;
/// the system clipboard is written by a helper, off the frame loop. The two
/// are independent and neither is assumed to work: an emulator may parse the
/// sequence and do nothing (VTE/GNOME Terminal — GNOME bug 795774), and a
/// desktop may have no `wl-copy` / `xclip` at all. So nothing is claimed here
/// — `poll_clipboard` reports what each route actually did, once the helper
/// answers.
///
/// Used by the palette's `term.copy` and by the selection's release.
pub fn copy_to_host(app: &mut App, text: &str) {
    let lang = app.lang();
    let chars = text.chars().count();
    // The helper gets the whole selection — it is off this thread, so the
    // stall the cap exists for cannot happen — while `OSC 52` keeps its cap,
    // and the report carries both numbers.
    let osc_text = cap_osc52(text);
    let osc_chars = osc_text.chars().count();
    let payload = terminal_view::osc52_write(osc_text.as_ref());
    use std::io::Write as _;
    let mut out = std::io::stdout();
    let outcome = out.write_all(payload.as_bytes()).and_then(|()| out.flush());
    app.clipboard_jobs.push(clipboard::ClipboardJob::for_copy(
        clipboard::spawn_write_text(text.to_string()),
        chars,
        outcome.is_ok().then_some(osc_chars),
    ));
    if let Err(err) = outcome {
        app.notices.push(
            NoticeLevel::Error,
            tr!(t(palette::PAL_MSG_COPY_FAILED, lang), err),
        );
    }
}

/// What one `OSC 52` may carry. The scrollback ring holds up to 4 MiB and a
/// selection may name all of it, but the write goes straight to the terminal
/// on this thread — a payload that size would stall the frame loop behind a
/// slow emulator. Cut on a glyph boundary; the toast counts what was actually
/// sent, so a capped copy says the smaller number.
const OSC52_MAX_BYTES: usize = 512 * 1024;

fn cap_osc52(text: &str) -> std::borrow::Cow<'_, str> {
    if text.len() <= OSC52_MAX_BYTES {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut cut = OSC52_MAX_BYTES;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    std::borrow::Cow::Owned(text[..cut].to_string())
}

fn paste(app: &mut App, sticky: &mut StickyMods, text: &str) {
    // Bracketed paste is on for the whole TUI, so the terminal hands the
    // clipboard over as a single `Event::Paste` — never as keystrokes. That
    // makes the destination an all-or-nothing decision: where a cursor blinks
    // the text has to be written into that field, or not one character
    // arrives. Dropping it (what this did until 10-06) is why the AI config
    // refused Ctrl+V while the pane beside it took it happily.
    if !accepts_paste(app) {
        return;
    }
    if field_has_focus(app) {
        for key in paste_keys(app, text) {
            handle_key(app, sticky, key);
        }
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

/// Where a paste would land: a focused text field, or the pane itself with
/// nothing over it. Split out of [`paste`] so `Ctrl+Shift+V` can decide
/// *before* reading the clipboard whether there is anywhere to put it — and
/// the answer has to be the one `paste` itself gives.
fn accepts_paste(app: &App) -> bool {
    if field_has_focus(app) {
        return true;
    }
    !(app.dialog.is_some() || app.palette.is_some())
        && app.focus == Focus::Center
        && app.view == View::Terminal
}

/// `Ctrl+Shift+V`: put the system clipboard where the keyboard currently
/// writes, which is what the web toolbar button does (`app.js` →
/// `pasteTerminalButton` reads `navigator.clipboard.readText()` and toasts
/// when the browser refuses it).
///
/// `read` is a *closure* for a reason: reaching for the clipboard means
/// spawning helpers and waiting out their timeouts (`clipboard::read`, up to
/// 1.5 s on the first one), so the gate has to decide before that cost is
/// paid — otherwise a key that lands nowhere froze the loop and threw the
/// answer away. `None` means no helper answered in time (see [`clipboard`]).
/// There is no clipboard API to fall back on inside a terminal program, so
/// the user is told instead — and the toast names the route that always
/// exists, the emulator's own paste key, which arrives here as
/// `Event::Paste`.
fn begin_paste(app: &mut App, spawn: impl FnOnce() -> std::sync::mpsc::Receiver<Option<String>>) {
    if !accepts_paste(app) {
        // Same inertness as a bracketed paste with a confirmation open: an
        // unanswered key must not report a failure either — and it must not
        // pay for a clipboard read it is going to drop.
        return;
    }
    // Queue, don't wait: the answer is collected by `poll_clipboard` on a
    // later tick. A second key while one is in flight replaces it — two keys
    // are one paste, not two.
    app.pending_paste = Some(spawn());
}

/// Apply a clipboard answer. `None` (and `Some("")`) means no helper could be
/// reached: the user is told instead, naming the route that always exists.
fn finish_paste(app: &mut App, sticky: &mut StickyMods, result: Option<String>) {
    if !accepts_paste(app) {
        // The focus moved on while the helper ran; there is nowhere to put it,
        // exactly as there was nowhere when the key arrived with a box open.
        return;
    }
    match result {
        Some(text) if !text.is_empty() => {
            // The web button calls `resetModifiers()` before pasting so an
            // armed Ctrl cannot rewrite the first character.
            *sticky = StickyMods::default();
            paste(app, sticky, &text);
        }
        _ => app.toast(
            NoticeLevel::Error,
            t(MOD_PASTE_UNAVAILABLE, app.lang()).to_string(),
        ),
    }
}

/// Collect clipboard answers that finished off the frame loop (once per tick).
///
/// Nothing here ever blocks: a receiver with no answer yet is simply put back
/// for the next frame, which is the whole point of the split — `clipboard`'s
/// helpers each own a deadline (`FAST` per helper, 1.5 s for PowerShell) and
/// this thread is the one that draws.
pub(crate) fn poll_clipboard(app: &mut App, sticky: &mut StickyMods) {
    if let Some(rx) = app.pending_paste.take() {
        match rx.try_recv() {
            Ok(result) => finish_paste(app, sticky, result),
            Err(std::sync::mpsc::TryRecvError::Empty) => app.pending_paste = Some(rx),
            // The worker died without answering: report it like no answer.
            Err(std::sync::mpsc::TryRecvError::Disconnected) => finish_paste(app, sticky, None),
        }
    }
    let jobs = std::mem::take(&mut app.clipboard_jobs);
    for job in jobs {
        match job.rx.try_recv() {
            Ok(true) => clipboard_answer(app, &job, true),
            Ok(false) => clipboard_answer(app, &job, false),
            Err(std::sync::mpsc::TryRecvError::Empty) => app.clipboard_jobs.push(job),
            // The worker died without answering. For a copy this program
            // started that still has to be reported — staying quiet is the
            // old lie again; an inbound write that vanished has no news.
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                if job.copy.is_some() {
                    clipboard_answer(app, &job, false);
                }
            }
        }
    }
}

/// One clipboard worker's verdict, worded by what it was asked to do.
///
/// A copy **we** started is reported either way. A helper that took the text
/// is a copy that really happened (`PAL_MSG_COPIED`, the web's
/// `clipboard.writeText` equivalent); none taking it means the text only went
/// to the emulator over `OSC 52`, and the emulator may parse that sequence
/// and drop it on the floor (VTE/GNOME Terminal — GNOME bug 795774), so the
/// toast says exactly that instead of "Copied", with the reason and the fix
/// behind it the first time in a session.
fn clipboard_answer(app: &mut App, job: &clipboard::ClipboardJob, took: bool) {
    let lang = app.lang();
    match job.copy {
        Some(report) if took => app.toast(
            NoticeLevel::Info,
            tr!(t(palette::PAL_MSG_COPIED, lang), report.chars),
        ),
        Some(report) => {
            if !app.copy_hint_shown {
                app.copy_hint_shown = true;
                app.toast(NoticeLevel::Warn, t(MOD_COPY_NO_HELPER, lang).to_string());
            }
            // `None` means the write to stdout failed, and that failure is
            // already on the log by its own message — don't claim we sent it.
            if let Some(osc) = report.osc {
                app.toast(
                    NoticeLevel::Info,
                    tr!(t(palette::PAL_MSG_COPY_OSC52_ONLY, lang), osc),
                );
            }
        }
        None if !took => app.notices.push(
            NoticeLevel::Warn,
            t(MOD_CLIP_WRITE_FAILED, lang).to_string(),
        ),
        None => {}
    }
}

/// True when a text field is what the keyboard currently drives — that is
/// where a paste belongs, and nowhere else. A `Confirm` is deliberately not
/// one: it reads a bare `y` as "yes, reboot now" (`dialogs::confirm`), so
/// pasting into it has to be inert rather than guessed at.
fn field_has_focus(app: &App) -> bool {
    if let Some(dialog) = app.dialog.as_ref() {
        return matches!(dialog, Dialog::Uart { .. } | Dialog::Settings(_));
    }
    if app.palette.is_some() {
        return true;
    }
    match app.focus {
        Focus::Sidebar | Focus::Assistant => true,
        Focus::Center => app.view == View::Network,
    }
}

/// Clipboard text turned into the keystrokes the focused field would have
/// seen if it had been typed. Line breaks are dropped on a single-line field
/// (the browser does the same to `<input>`, so `endpoint`, `api_key` and the
/// rest behave identically) and become Enter in the assistant composer, the
/// only multiline one; every other control character is dropped too, since
/// none of the fields can hold it.
fn paste_keys(app: &App, text: &str) -> Vec<KeyEvent> {
    // The destination is whatever `field_has_focus()` picked, and with a
    // dialog or the palette open that is *not* `app.focus`: reading the focus
    // here turned a newline into Enter inside the AI configuration dialog
    // (which answers Enter on the Save/Clear rows) or the palette.
    let multiline = app.dialog.is_none() && app.palette.is_none() && app.focus == Focus::Assistant;
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut keys = Vec::new();
    for (index, line) in normalized.split('\n').enumerate() {
        // The line *break* goes; the text on either side of it does not. A
        // single-line field drops the breaks the way an `<input>` sanitizes a
        // pasted value (strip U+000A and U+000D, keep the rest), so `a\nb`
        // becomes `ab`. Skipping the whole iteration on `index > 0` — which
        // is what this used to do — kept only the first line and threw every
        // line after it away in silence, and a paste whose first line was
        // empty did nothing at all.
        if index > 0 && multiline {
            keys.push(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        }
        keys.extend(
            line.chars()
                .filter(|c| !c.is_control())
                .map(|c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)),
        );
    }
    keys
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
    PasteClipboard,
}

/// `F2..F6` → view (the five surfaces). F6 is the transfer view's
/// shortcut alias: the palette's `transfer.*` actions are its front door,
/// and both do the same thing — open the form and run the precheck.
fn fkey_view(n: u8) -> Option<View> {
    match n {
        2 => Some(View::Terminal),
        3 => Some(View::Diagnostics),
        4 => Some(View::Network),
        5 => Some(View::Assistant),
        6 => Some(View::Transfer),
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
        // The TUI's half of the web toolbar: `pasteTerminalButton` reads the
        // clipboard, the modifier buttons latch the one-shot modifiers.
        KeyCode::Char('v') if ctrl && shift => Global::PasteClipboard,
        // One-shot modifier latches (the TUI's stand-in for the web key bar).
        KeyCode::Char('r') if ctrl && shift => Global::ArmShift,
        KeyCode::Char('c') if ctrl && shift => Global::ArmCtrl,
        KeyCode::Char('a') if ctrl && shift => Global::ArmAlt,
        _ => Global::None,
    }
}

/// Route one key press: overlays first, then the global bindings, then the
/// focused pane.
///
/// A one-shot modifier latch lives for exactly this press: whatever consumes
/// the key consumes the latch with it (web `resetModifiers()`), so an armed
/// Ctrl cannot survive into the sidebar, a dialog or the scrollback and
/// rewrite a key pressed seconds later — only the terminal branch used to
/// clear it. The three `Arm*` bindings are the exception: they are how a latch
/// is released as well as armed, so they keep it for [`arm_sticky`].
fn handle_key(app: &mut App, sticky: &mut StickyMods, key: crossterm::event::KeyEvent) {
    let global = global_key(key.code, key.modifiers);
    let arms_a_latch = matches!(global, Global::ArmShift | Global::ArmCtrl | Global::ArmAlt);
    route_key(app, sticky, key, global);
    if !arms_a_latch {
        *sticky = StickyMods::default();
    }
}

/// The routing itself; [`handle_key`] wraps it to settle the one-shot latch.
fn route_key(
    app: &mut App,
    sticky: &mut StickyMods,
    key: crossterm::event::KeyEvent,
    global: Global,
) {
    // Paste carries text, not a keystroke, so it is routed before the
    // overlays: an overlay owns the keyboard (CONTRACTS.md section 5), but
    // the AI configuration dialog is precisely where a clipboard — an API
    // key — belongs. `begin_paste` is inert wherever `Event::Paste` would
    // be, so a confirmation still ignores it.
    if matches!(global, Global::PasteClipboard) {
        // The gate is read *before* a helper thread is started, so a key with
        // nowhere to put its answer costs nothing — and the read itself runs
        // off this thread, whose job is drawing.
        begin_paste(app, clipboard::spawn_read);
        return;
    }

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
            // The other half is evidence hygiene (spec §6.2/§6.4): Clear also
            // empties the assistant's serial journal and cancels a turn still
            // streaming — web's `clearButton` does `agentJournal.reset()` and
            // `agentPanel.logsCleared()` (which stops the run) in the same
            // click. The conversation itself is deliberately left alone: it
            // belongs to `Ctrl+Shift+N` here, so a wiped screen never costs
            // the chat.
            if let Some(runtime) = app.agent.as_ref() {
                runtime.handle.stop();
                runtime.handle.reset_journal();
            }
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
            match view {
                View::Diagnostics => open_diagnostics(app),
                // The precheck is the gate, so opening the view *is* asking
                // the device whether it can transfer at all.
                View::Transfer => transfer_view::open(app, None),
                _ => app.set_view(view),
            }
            return;
        }
        Global::ArmShift => {
            let next = StickyMods {
                shift: true,
                ..StickyMods::default()
            };
            arm_sticky(app, sticky, next, "Shift");
            return;
        }
        Global::ArmCtrl => {
            let next = StickyMods {
                ctrl: true,
                ..StickyMods::default()
            };
            arm_sticky(app, sticky, next, "Ctrl");
            return;
        }
        Global::ArmAlt => {
            let next = StickyMods {
                alt: true,
                ..StickyMods::default()
            };
            arm_sticky(app, sticky, next, "Alt");
            return;
        }
        // Already returned at the top of this function, in every state: one
        // place decides what the paste key does, and the arm exists only so
        // the match stays exhaustive.
        Global::PasteClipboard => {}
        Global::None => {}
    }

    // Esc walks one level out: sidebar → center, assistant / other views →
    // the terminal view, and only from the terminal view it is a terminal key.
    if key.code == KeyCode::Esc {
        if app.focus == Focus::Sidebar {
            // One level out lands on the panel under the cursor. The assistant
            // panel only reads keys under `Focus::Assistant`, so parking it on
            // `Center` there swallowed every plain keystroke: the view match
            // below hands `Terminal | Assistant` nothing, and the terminal
            // branch is skipped because the view is not `Terminal`.
            app.focus = if app.view == View::Assistant {
                Focus::Assistant
            } else {
                Focus::Center
            };
            return;
        }
        // A transfer in flight is the one thing that must not survive the
        // view being left: it types into the device's shell, and nobody is
        // watching a pane that is no longer there.
        if app.view == View::Transfer && transfer_view::escape(app) {
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
        View::Transfer => {
            transfer_view::handle_key(app, key);
            return;
        }
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
    if app.focus != Focus::Center
        || !matches!(app.view, View::Diagnostics | View::Network | View::Transfer)
    {
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
        transfer: self::transfer_view::State::default(),
        palette: None,
        dialog: None,
        dialog_return: None,
        notices: Notices::default(),
        exec_mode: crate::agent::ExecMode::Auto,
        exec_expires_at: 0,
        pending_paste: None,
        clipboard_jobs: Vec::new(),
        copy_hint_shown: false,
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
        center_x: 0,
        center_y: 0,
        screen_height: 24,
        center_scroll: 0,
    }
}

/// `test_app()` with a live assistant runtime attached. The feed task and the
/// turn loop both run on `app.rt`, so the tests that watch Clear or a connect
/// empty the evidence window have something to watch — the journal itself
/// stays private to `agent`, hence the `journal_len()` probe.
#[cfg(test)]
pub(crate) fn attach_agent(app: &mut App) -> crate::agent::AgentHandle {
    let broker = Arc::new(app.broker.clone()) as Arc<dyn crate::agent::ApprovalBroker>;
    let rt = app.rt.clone();
    let _guard = rt.handle().enter();
    let handle = crate::agent::spawn(None, broker, app.session.clone(), app.bus.clone());
    // Same adoption path as `ensure_agent`, so a mode picked before the first
    // question is pushed into the runtime here too.
    crate::tui::assistant_view::adopt_runtime(app, handle.clone());
    handle
}

/// Put bytes into the assistant's evidence window and wait for the feed to
/// take them. Publishing has to repeat: a broadcast send with no subscriber
/// yet is lost, and the feed task may not have subscribed on the first pass.
#[cfg(test)]
pub(crate) fn feed_journal(app: &App, handle: &crate::agent::AgentHandle, bytes: &[u8]) {
    for _ in 0..400 {
        if handle.journal_len() > 0 {
            return;
        }
        app.bus.publish(CoreEvent::UartRx(bytes.to_vec()));
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("the feed never reached the assistant's journal");
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
    fn the_terminal_answers_the_device_instead_of_hoarding_the_replies() {
        let mut app = test_app();
        // Cursor position request + a clipboard set, in one chunk of output.
        let bytes = b"\x1b[2;5H\x1b[6n\x1b]52;c;aGVsbG8=\x07".to_vec();

        on_core_event(&mut app, CoreEvent::UartRx(bytes));

        assert!(
            app.terminal.grid.take_pending_reports().is_empty(),
            "the device's `CPR` question has to be sent back, not left queued"
        );
        assert!(
            app.terminal.grid.take_clipboard().is_none(),
            "the `OSC 52` payload has to reach the clipboard layer"
        );
    }

    /// The answer used to travel through `send_bytes`, whose `connected()` gate
    /// is a **typed-key** rule: it drops input after a link drop so one notice
    /// per keystroke cannot bury the log. But `state` lags the transport by a
    /// poll tick (and sits on `Connecting` for the length of an adopt), so a
    /// `CPR` question arriving in that window was consumed and then thrown
    /// away — the program that asked waited forever, and the queue was empty
    /// either way, which is why the test above could not see it. A detached
    /// session fails *every* send with "session gone", so that notice is the
    /// proof the reply was actually handed over.
    #[test]
    fn a_cursor_report_reaches_the_session_even_while_the_link_is_flapping() {
        let mut app = test_app();
        app.state = ConnectionState::Failed;
        assert!(app.notices.log.is_empty(), "starting from a quiet log");

        on_core_event(&mut app, CoreEvent::UartRx(b"\x1b[6n".to_vec()));

        assert!(
            app.terminal.grid.take_pending_reports().is_empty(),
            "the question is consumed either way"
        );
        assert!(
            app.notices
                .log
                .iter()
                .any(|(_, text)| text.contains("session gone")),
            "…but it has to reach the session, not be swallowed by the gate"
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
    fn f_keys_map_to_the_five_views_plus_help() {
        assert_eq!(fkey_view(2), Some(View::Terminal));
        assert_eq!(fkey_view(3), Some(View::Diagnostics));
        assert_eq!(fkey_view(4), Some(View::Network));
        assert_eq!(fkey_view(5), Some(View::Assistant));
        assert_eq!(fkey_view(6), Some(View::Transfer));
        assert_eq!(fkey_view(1), None, "F1 is the help overlay");
        assert_eq!(fkey_view(7), None, "F7 and up belong to the target");
    }

    /// The transfer entry point does the same thing from either door: the
    /// view comes up and the precheck is already in flight, so a second key
    /// press is never needed to find out whether the device can transfer.
    #[test]
    fn f6_opens_the_transfer_view_and_runs_the_precheck() {
        let mut app = crate::tui::test_app();
        assert_eq!(
            global_key(KeyCode::F(6), KeyModifiers::NONE),
            Global::View(View::Transfer)
        );
        route_key(
            &mut app,
            &mut StickyMods::default(),
            crossterm::event::KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE),
            Global::View(View::Transfer),
        );
        assert_eq!(app.view, View::Transfer);
        assert!(
            matches!(
                app.transfer.engine.phase,
                crate::transfer::Phase::Cmd {
                    step: crate::transfer::Step::Probe,
                    ..
                }
            ),
            "the precheck is in flight, not waiting to be pressed"
        );
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
        assert_eq!(
            global_key(KeyCode::Char('v'), both),
            Global::PasteClipboard,
            "paste is the other half of the web toolbar"
        );
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
        assert_eq!(
            global_key(KeyCode::Char('v'), KeyModifiers::NONE),
            Global::None,
            "a bare `v` belongs to the target shell, only the chord is taken"
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

    /// G-2, in the scope you picked (10-07): Clear also empties the
    /// save-log ring (`web/app.js:3386` + spec §6.2), resets the assistant's
    /// serial evidence window (spec §6.4) and cancels a turn still streaming —
    /// but the conversation stays, because here it belongs to `Ctrl+Shift+N`.
    #[test]
    fn ctrl_l_clears_the_evidence_window_and_keeps_the_conversation() {
        let mut app = crate::tui::test_app();
        let handle = crate::tui::attach_agent(&mut app);
        crate::tui::feed_journal(&app, &handle, b"boot: bytes from the old session");
        assert!(handle.journal_len() > 0, "the window holds evidence");

        app.terminal.feed(b"device output\r\n");
        app.assistant
            .entries
            .push(assistant_view::Entry::User("why?".into()));
        app.assistant
            .entries
            .push(assistant_view::Entry::Assistant("because".into()));
        let entries = app.assistant.entries.len();

        let mut sticky = StickyMods::default();
        handle_key(
            &mut app,
            &mut sticky,
            crossterm::event::KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL),
        );

        assert_eq!(handle.journal_len(), 0, "Clear resets the serial journal");
        assert!(
            app.terminal.grid.log_bytes().is_empty(),
            "…and empties the save-log ring"
        );
        assert_eq!(
            app.assistant.entries.len(),
            entries,
            "the conversation is not part of Clear"
        );
        assert!(app.agent.is_some(), "and the runtime stays up");
    }

    // Paste. Bracketed paste is on for the whole TUI, so a paste never
    // arrives as keystrokes: the destination has to be written to directly.

    /// The report that started this (10-06): Ctrl+V in the AI config did
    /// nothing at all. The endpoint field is `FIELDS[0]`, so the default
    /// selection is already pointing at it.
    #[test]
    fn a_paste_lands_in_the_ai_config_field() {
        let mut app = test_app();
        app.dialog = Some(Dialog::Settings(
            agent_settings::AgentSettingsState::default(),
        ));
        let mut sticky = StickyMods::default();

        paste(&mut app, &mut sticky, "https://api.example.com/v1\n");

        let Some(Dialog::Settings(state)) = app.dialog.as_ref() else {
            panic!("the dialog must stay open");
        };
        assert_eq!(
            state.endpoint.as_str(),
            "https://api.example.com/v1",
            "the trailing line break is not part of a single-line field"
        );
        assert!(state.dirty, "a paste is a keystroke, so it flags the form");
    }

    /// A paste that is more than one line long must not lose everything
    /// after the first break in a single-line field: the browser strips the
    /// line *breaks* out of an `<input>` and keeps every other character
    /// (HTML's value sanitization for `type=text` removes U+000A and U+000D
    /// and nothing else), so `one\ntwo` arrives as `onetwo`. The loop used to
    /// skip the whole iteration for `index > 0`, which typed only the first
    /// line and dropped the rest without a word.
    #[test]
    fn a_single_line_field_keeps_every_line_of_a_paste() {
        let mut app = test_app();
        app.dialog = Some(Dialog::Settings(
            agent_settings::AgentSettingsState::default(),
        ));
        let mut sticky = StickyMods::default();

        paste(&mut app, &mut sticky, "one\ntwo");

        let Some(Dialog::Settings(state)) = app.dialog.as_ref() else {
            panic!("the dialog must stay open");
        };
        assert_eq!(state.endpoint.as_str(), "onetwo");
        assert!(state.dirty, "a paste is a keystroke, so it flags the form");
    }

    /// …and a paste whose first line is empty is not a paste of nothing:
    /// once the break goes, the second line is still there to type. Before
    /// the fix this one was silently inert — no keys, no toast, no paste.
    #[test]
    fn a_paste_that_starts_with_a_line_break_still_lands() {
        let mut app = test_app();
        app.dialog = Some(Dialog::Settings(
            agent_settings::AgentSettingsState::default(),
        ));
        let mut sticky = StickyMods::default();

        paste(&mut app, &mut sticky, "\nvalue");

        let Some(Dialog::Settings(state)) = app.dialog.as_ref() else {
            panic!("the dialog must stay open");
        };
        assert_eq!(state.endpoint.as_str(), "value");
    }

    /// A `Confirm` reads a bare `y` as "yes" (`dialogs::confirm` →
    /// `app.quit = true` for the quit dialog). Pasting while one is open has
    /// to be inert: nobody means to quit by pasting an API key.
    #[test]
    fn a_paste_cannot_answer_a_confirmation() {
        let mut app = test_app();
        app.dialog = Some(Dialog::Confirm {
            kind: dialogs::ConfirmKind::Quit,
            title: String::new(),
            message: String::new(),
        });
        let mut sticky = StickyMods::default();

        paste(&mut app, &mut sticky, "y");

        assert!(!app.quit, "a pasted `y` is not consent");
        assert!(app.dialog.is_some(), "…and the box stays up");
    }

    /// The composer is the one multiline field: pasting a paragraph has to
    /// keep its line breaks (Enter inserts `\n` there).
    #[test]
    fn the_composer_pastes_its_lines() {
        let mut app = test_app();
        app.focus = Focus::Assistant;
        let mut sticky = StickyMods::default();

        paste(&mut app, &mut sticky, "line one\r\nline two");

        assert_eq!(app.assistant.composer.as_str(), "line one\nline two");
    }

    /// The newline split used to read `app.focus`, which is *not* the
    /// destination once an overlay is up — `field_has_focus()` picks the
    /// dialog/palette regardless. With the AI configuration dialog open and
    /// focus still on the composer, a pasted newline became a real Enter on
    /// the focused row: Save writes a half-pasted form, Clear deletes the
    /// stored record from disk, and the palette runs whatever is highlighted.
    #[test]
    fn a_paste_never_forges_an_enter_for_an_overlay() {
        let mut app = test_app();
        app.focus = Focus::Assistant; // the stale value `paste_keys` used to read
        app.dialog = Some(Dialog::Settings(
            agent_settings::AgentSettingsState::default(),
        ));
        let keys = paste_keys(&app, "one\ntwo");
        assert!(
            keys.iter().all(|k| k.code != KeyCode::Enter),
            "a dialog must not receive a forged Enter: {keys:?}"
        );

        app.dialog = None;
        app.palette = Some(palette::PaletteState::default());
        let keys = paste_keys(&app, "one\ntwo");
        assert!(
            keys.iter().all(|k| k.code != KeyCode::Enter),
            "the palette must not receive a forged Enter either: {keys:?}"
        );
    }

    /// Arming the same latch twice has to release it (`arm_sticky`'s release
    /// branch). The callers used to pass the *flipped* state, which made that
    /// branch unreachable: the second press reported "armed" while disarming.
    #[test]
    fn pressing_a_latch_twice_says_that_it_was_released() {
        let mut app = test_app();
        let mut sticky = StickyMods::default();
        let both = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
        let arm = KeyEvent::new(KeyCode::Char('r'), both);

        handle_key(&mut app, &mut sticky, arm);
        assert!(sticky.shift, "the first press arms the latch");
        assert!(
            app.notices
                .toasts
                .last()
                .is_some_and(|t| t.text.contains("armed")),
            "armed is reported: {:?}",
            app.notices.toasts.last()
        );

        handle_key(&mut app, &mut sticky, arm);
        assert!(!sticky.shift, "the second press releases it");
        assert!(
            app.notices
                .toasts
                .last()
                .is_some_and(|t| t.text.contains("released")),
            "released is reported: {:?}",
            app.notices.toasts.last()
        );
    }

    /// "Armed for the next key" means the *next* key, wherever it goes: only
    /// the terminal branch used to consume the latch, so an armed Ctrl could
    /// sit through sidebar/scroll/dialog keys and rewrite a character typed
    /// seconds later into `0x03`.
    #[test]
    fn a_latch_does_not_outlive_the_key_it_was_armed_for() {
        let mut app = test_app();
        let mut sticky = StickyMods::default();
        let both = KeyModifiers::CONTROL | KeyModifiers::SHIFT;

        handle_key(
            &mut app,
            &mut sticky,
            KeyEvent::new(KeyCode::Char('c'), both),
        );
        assert!(sticky.ctrl, "the latch is armed");

        // A view switch is a plain consumed key with nothing to encode.
        handle_key(
            &mut app,
            &mut sticky,
            KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE),
        );
        assert_eq!(
            sticky,
            StickyMods::default(),
            "one shot means one key press"
        );
    }

    /// A sidebar field is just as editable as a dialog one, and the sidebar
    /// is where the LAN token (32 hex characters) gets pasted in practice.
    #[test]
    fn a_paste_reaches_the_focused_sidebar_field() {
        let mut app = test_app();
        app.focus = Focus::Sidebar;
        let items = sidebar::entries(&app);
        app.sidebar.selection = items
            .iter()
            .position(|entry| matches!(entry, sidebar::SideEntry::BleName))
            .expect("the device name entry exists while connected over BLE");
        let mut sticky = StickyMods::default();

        paste(&mut app, &mut sticky, "Linkr BLE UART-3");

        assert_eq!(app.sidebar.ble_name.as_str(), "Linkr BLE UART-3");
    }

    /// The pane itself keeps the old behaviour: the text is fed to the grid
    /// (local echo) and sent to the device as bytes.
    #[test]
    fn a_paste_in_the_terminal_still_goes_to_the_device() {
        let mut app = test_app();
        app.settings.local_echo = true;
        let mut sticky = StickyMods::default();

        paste(&mut app, &mut sticky, "echo hi");

        assert!(
            app.terminal.visible_text().contains("echo hi"),
            "the terminal path must not have been disturbed: {:?}",
            app.terminal.visible_text()
        );
    }

    /// A receiver whose answer is already in: production hands back a thread
    /// that is still running, the tests hand back one that has replied.
    fn answered(result: Option<String>) -> std::sync::mpsc::Receiver<Option<String>> {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = tx.send(result);
        rx
    }

    /// The AI config dialog is where a clipboard actually matters — the API
    /// key is a click away in the browser and a paste away here.
    #[test]
    fn a_paste_key_types_the_clipboard_into_the_focused_field() {
        let mut app = test_app();
        app.dialog = Some(Dialog::Settings(
            agent_settings::AgentSettingsState::default(),
        ));
        let mut sticky = StickyMods {
            ctrl: true,
            ..StickyMods::default()
        };

        begin_paste(&mut app, || {
            answered(Some("https://api.example.com/v1".to_string()))
        });
        poll_clipboard(&mut app, &mut sticky);

        let Some(Dialog::Settings(state)) = app.dialog.as_ref() else {
            panic!("the dialog must stay open");
        };
        assert_eq!(state.endpoint.as_str(), "https://api.example.com/v1");
        assert!(
            sticky.is_empty(),
            "the web button resets the modifiers so an armed Ctrl cannot rewrite the paste"
        );
    }

    /// No helper answered (a desktop with neither `wl-paste` nor `xclip`,
    /// or one that denied it): the user has to be told, in the UI's language,
    /// and nothing may be typed anywhere.
    #[test]
    fn a_paste_key_that_cannot_read_the_clipboard_says_so() {
        let mut app = test_app();
        app.dialog = Some(Dialog::Settings(
            agent_settings::AgentSettingsState::default(),
        ));
        let mut sticky = StickyMods::default();

        begin_paste(&mut app, || answered(None));
        poll_clipboard(&mut app, &mut sticky);

        let Some(Dialog::Settings(state)) = app.dialog.as_ref() else {
            panic!("the dialog must stay open");
        };
        assert!(state.endpoint.as_str().is_empty(), "nothing gets typed");
        let expected = t(MOD_PASTE_UNAVAILABLE, app.lang());
        assert!(
            app.notices.log.iter().any(|(_, line)| line == expected),
            "the failure must reach the notice log, got {:?}",
            app.notices.log
        );
    }

    /// A bracketed paste is inert while a confirmation is open (`y` would be
    /// consent), so the key is too — including its failure report: there was
    /// nowhere to paste, which is not a clipboard problem.
    #[test]
    fn a_paste_key_cannot_answer_a_confirmation() {
        let mut app = test_app();
        app.dialog = Some(Dialog::Confirm {
            kind: dialogs::ConfirmKind::Quit,
            title: String::new(),
            message: String::new(),
        });
        let mut sticky = StickyMods::default();
        let before = app.notices.log.len();
        let mut reads = 0;

        begin_paste(&mut app, || {
            reads += 1;
            answered(Some("y".to_string()))
        });
        poll_clipboard(&mut app, &mut sticky);

        assert_eq!(
            reads, 0,
            "the gate decides before the clipboard is reached for: reading it \
             means spawning helpers and waiting out their timeouts, for an \
             answer this key would drop anyway"
        );
        assert!(!app.quit, "a pasted `y` is not consent");
        assert!(app.dialog.is_some(), "…and the box stays up");
        assert_eq!(
            app.notices.log.len(),
            before,
            "an inert paste reports nothing"
        );
    }

    /// F: the key must return while the answer is still on its way. The
    /// frame loop is the thread that draws, and `clipboard::read` can spend
    /// `FAST` per helper (1.5 s for PowerShell) waiting out a wedged desktop
    /// — a paste may not be what stalls the interface.
    #[test]
    fn a_paste_key_returns_before_the_clipboard_answers() {
        let mut app = test_app();
        app.dialog = Some(Dialog::Settings(
            agent_settings::AgentSettingsState::default(),
        ));
        let mut sticky = StickyMods::default();
        let before = app.notices.log.len();
        let (tx, rx) = std::sync::mpsc::channel();

        begin_paste(&mut app, || rx);

        assert!(
            app.pending_paste.is_some(),
            "queued for a later tick, not waited on"
        );
        assert_eq!(app.notices.log.len(), before, "…and nothing reported yet");
        let Some(Dialog::Settings(state)) = app.dialog.as_ref() else {
            panic!("the dialog must stay open");
        };
        assert!(state.endpoint.as_str().is_empty(), "nothing typed yet");

        // The worker answers; the next tick collects it.
        assert!(tx
            .send(Some("https://api.example.com/v1".to_string()))
            .is_ok());
        poll_clipboard(&mut app, &mut sticky);
        let Some(Dialog::Settings(state)) = app.dialog.as_ref() else {
            panic!("the dialog must stay open");
        };
        assert_eq!(state.endpoint.as_str(), "https://api.example.com/v1");
        assert!(app.pending_paste.is_none(), "…and the slot is free again");
    }

    /// The other half of F: an inbound `OSC 52` used to run its helper chain
    /// inline in `on_core_event`, so a device copying to the host could stall
    /// the terminal for the length of a deadline. The payload is consumed and
    /// handed to a worker instead; the "no helper" notice comes back later.
    #[test]
    fn an_inbound_clipboard_set_does_not_run_on_the_frame_loop() {
        let mut app = test_app();
        let before = app.notices.log.len();

        on_core_event(
            &mut app,
            CoreEvent::UartRx(b"\x1b]52;c;aGVsbG8=\x07".to_vec()),
        );

        assert!(
            app.terminal.grid.take_clipboard().is_none(),
            "the payload is consumed here"
        );
        assert_eq!(
            app.clipboard_jobs.len(),
            1,
            "…and the write runs on its own thread"
        );
        assert_eq!(
            app.notices.log.len(),
            before,
            "no helper deadline is paid on the frame loop"
        );
    }

    /// `accepts_paste` is the gate the key reads *before* touching the
    /// clipboard, so it has to answer exactly what `paste` itself accepts.
    /// Esc is "one level out". From the sidebar in the assistant view that
    /// used to park the focus on `Focus::Center`, which the assistant panel
    /// never reads — every plain keystroke then fell through the view match
    /// (`Terminal | Assistant => {}`) and past the terminal branch (the view is
    /// not `Terminal`), so the composer stopped receiving anything.
    #[test]
    fn escaping_the_sidebar_puts_the_keys_back_in_the_composer() {
        let mut app = test_app();
        app.set_view(View::Assistant);
        app.focus = Focus::Sidebar;
        let mut sticky = StickyMods::default();

        handle_key(
            &mut app,
            &mut sticky,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        );
        handle_key(
            &mut app,
            &mut sticky,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        );

        assert_eq!(app.focus, Focus::Assistant, "the panel reads its own focus");
        assert_eq!(app.view, View::Assistant);
        assert_eq!(
            app.assistant.composer.as_str(),
            "x",
            "the keystroke must reach the composer"
        );
    }

    #[test]
    fn the_paste_gate_agrees_with_paste() {
        let mut app = test_app();
        app.dialog = None;
        app.palette = None;

        app.focus = Focus::Center;
        app.view = View::Terminal;
        assert!(accepts_paste(&app), "the pane itself takes a paste");
        app.view = View::Diagnostics;
        assert!(!accepts_paste(&app), "nothing is focused to receive it");
        app.view = View::Terminal;

        app.focus = Focus::Sidebar;
        assert!(accepts_paste(&app), "the sidebar fields take a paste");

        app.focus = Focus::Center;
        app.dialog = Some(Dialog::Confirm {
            kind: dialogs::ConfirmKind::Quit,
            title: String::new(),
            message: String::new(),
        });
        assert!(!accepts_paste(&app), "a confirmation ignores it");

        app.dialog = None;
        app.palette = Some(palette::PaletteState::new(app.lang()));
        assert!(accepts_paste(&app), "the palette query is a field");
    }

    // --- mouse selection and the wheel (P8) --------------------------------

    /// A pane the tests can name coordinates in: sidebar to the left, the
    /// terminal pane one row down and 60 columns wide.
    ///
    /// The grid is built at the pane's size rather than resized into it: a
    /// shrink pushes the rows it drops into the scrollback, which would move
    /// every absolute row the assertions name.
    fn mouse_app() -> App {
        let mut app = test_app();
        app.view = View::Terminal;
        app.focus = Focus::Center;
        app.center_x = 30;
        app.center_y = 1;
        app.center_width = 60;
        app.center_height = 20;
        let (cols, rows) = terminal_view::grid_dims(60, 20, app.settings.font_size);
        app.terminal.grid = terminal_view::TermGrid::new(cols, rows);
        app.terminal.dims = (cols, rows);
        app.terminal.grid.feed(b"alpha beta\r\ngamma delta\r\n");
        app
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn down(column: u16, row: u16) -> MouseEvent {
        mouse(MouseEventKind::Down(MouseButton::Left), column, row)
    }

    fn drag(column: u16, row: u16) -> MouseEvent {
        mouse(MouseEventKind::Drag(MouseButton::Left), column, row)
    }

    fn up(column: u16, row: u16) -> MouseEvent {
        mouse(MouseEventKind::Up(MouseButton::Left), column, row)
    }

    #[test]
    fn screen_coordinates_land_on_the_grid_cell_under_them() {
        let app = mouse_app();

        assert_eq!(pane_cell(&app, 30, 1), Some((0, 0)), "the pane's corner");
        assert_eq!(pane_cell(&app, 36, 2), Some((1, 6)));
        assert_eq!(pane_cell(&app, 29, 2), None, "the sidebar is not the pane");
        assert_eq!(pane_cell(&app, 36, 0), None, "nor is the top status bar");
        assert_eq!(pane_cell(&app, 90, 2), None, "nor past its right edge");
        assert_eq!(pane_cell(&app, 36, 21), None, "nor below its last row");
    }

    #[test]
    fn a_drag_selects_the_block_and_the_release_copies_it() {
        let mut app = mouse_app();
        let mut sticky = StickyMods::default();
        app.focus = Focus::Sidebar;

        handle_mouse(&mut app, &mut sticky, down(36, 2));

        assert_eq!(
            app.focus,
            Focus::Center,
            "a click in the pane takes the keys back to it"
        );
        assert!(app.terminal.is_dragging());
        assert_eq!(app.terminal.selection.unwrap().anchor, (1, 6));

        handle_mouse(&mut app, &mut sticky, drag(40, 2));
        assert_eq!(app.terminal.selection.unwrap().focus, (1, 10));

        handle_mouse(&mut app, &mut sticky, up(40, 2));

        assert!(!app.terminal.is_dragging(), "the release ends the gesture");
        // The text is handed to both routes at once; the words that report it
        // wait for the helper (`poll_clipboard`), because until something
        // answers we do not know where it landed.
        let job = app.clipboard_jobs.last().expect("the release copies");
        let report = job.copy.expect("…and it is this program's own copy");
        assert_eq!(report.chars, 5, "gamma delta cols 6..10 = \"delta\"");
        assert_eq!(report.osc, Some(5), "the same five reach the emulator too");
        assert!(
            app.terminal.selection.is_some(),
            "the block stays on screen so the user can see what was copied"
        );
    }

    #[test]
    fn a_click_that_never_moved_dismisses_the_selection_without_copying() {
        let mut app = mouse_app();
        let mut sticky = StickyMods::default();
        app.terminal.begin_selection(1, 0);

        handle_mouse(&mut app, &mut sticky, down(36, 2));
        handle_mouse(&mut app, &mut sticky, up(36, 2));

        assert!(app.terminal.selection.is_none());
        assert!(
            app.notices.log.is_empty(),
            "a click is how a selection is dismissed: no side effects"
        );
    }

    #[test]
    fn a_release_with_no_press_behind_it_never_re_sends_what_is_on_screen() {
        let mut app = mouse_app();
        let mut sticky = StickyMods::default();
        app.terminal.begin_selection(1, 0);
        assert!(app.terminal.take_dragging(), "the gesture is now over");

        handle_mouse(&mut app, &mut sticky, up(36, 2));

        assert!(app.terminal.selection.is_some(), "left exactly as it was");
        assert!(app.notices.log.is_empty());
    }

    /// A box opening over the pane before the button lifts must not strand
    /// the gesture. The release still ends it — copying what the drag
    /// covered — and leaves nothing behind for a later, unrelated press to
    /// stretch and re-send.
    #[test]
    fn a_box_opening_over_a_drag_ends_the_gesture_when_the_button_lifts() {
        let mut app = mouse_app();
        let mut sticky = StickyMods::default();

        handle_mouse(&mut app, &mut sticky, down(36, 2));
        handle_mouse(&mut app, &mut sticky, drag(40, 2));
        app.dialog = Some(Dialog::Confirm {
            kind: dialogs::ConfirmKind::Quit,
            title: String::new(),
            message: String::new(),
        });

        handle_mouse(&mut app, &mut sticky, up(40, 2));

        assert!(
            !app.terminal.is_dragging(),
            "the flag must not outlive the release, or the next stray one \
             re-sends what is on screen"
        );
        // The copy itself is queued, not announced: `poll_clipboard` says what
        // happened once a helper answers.
        let queued = app.clipboard_jobs.len();
        assert_eq!(queued, 1, "the release copies what the drag covered");

        // Whatever the leftover would have been, it is gone: a press that
        // lands in the sidebar and drags back into the pane starts nothing.
        app.dialog = None;
        let copies = app.notices.log.len();
        handle_mouse(&mut app, &mut sticky, down(4, 4));
        handle_mouse(&mut app, &mut sticky, drag(60, 6));
        handle_mouse(&mut app, &mut sticky, up(60, 6));
        assert_eq!(
            app.notices.log.len(),
            copies,
            "a gesture that is over cannot be stretched by the next one"
        );
        assert_eq!(
            app.clipboard_jobs.len(),
            queued,
            "…and it cannot send a second copy either"
        );
    }

    #[test]
    fn a_drag_past_the_edge_keeps_the_far_end_at_the_edge() {
        let mut app = mouse_app();
        let mut sticky = StickyMods::default();

        handle_mouse(&mut app, &mut sticky, down(36, 2));
        handle_mouse(&mut app, &mut sticky, drag(400, 400));

        assert_eq!(
            app.terminal.selection.unwrap().focus,
            (19, 59),
            "the last cell of the pane, not off it"
        );
    }

    #[test]
    fn the_mouse_is_ignored_everywhere_but_the_terminal_pane() {
        let mut app = mouse_app();
        let mut sticky = StickyMods::default();

        app.view = View::Assistant;
        handle_mouse(&mut app, &mut sticky, down(36, 2));
        assert!(app.terminal.selection.is_none(), "another view");

        app.view = View::Terminal;
        app.palette = Some(palette::PaletteState::new(app.lang()));
        handle_mouse(&mut app, &mut sticky, down(36, 2));
        assert!(
            app.terminal.selection.is_none(),
            "the palette keeps its clicks"
        );

        app.palette = None;
        app.dialog = Some(Dialog::Confirm {
            kind: dialogs::ConfirmKind::Quit,
            title: String::new(),
            message: String::new(),
        });
        handle_mouse(&mut app, &mut sticky, down(36, 2));
        assert!(app.terminal.selection.is_none(), "so does a dialog");
    }

    #[test]
    fn the_wheel_presses_the_key_the_pane_under_it_expects() {
        let mut app = mouse_app();

        assert_eq!(
            wheel_key(&app, true),
            KeyEvent::new(KeyCode::PageUp, KeyModifiers::SHIFT),
            "the terminal scrolls its own scrollback, which needs Shift"
        );
        assert_eq!(
            wheel_key(&app, false),
            KeyEvent::new(KeyCode::PageDown, KeyModifiers::SHIFT)
        );

        app.focus = Focus::Assistant;
        assert_eq!(
            wheel_key(&app, true),
            KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE)
        );

        app.focus = Focus::Sidebar;
        assert_eq!(
            wheel_key(&app, false),
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            "the sidebar is a list: the wheel walks it"
        );

        app.focus = Focus::Center;
        app.view = View::Network;
        assert_eq!(
            wheel_key(&app, true),
            KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE)
        );

        app.view = View::Terminal;
        app.palette = Some(palette::PaletteState::new(app.lang()));
        assert_eq!(
            wheel_key(&app, false),
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            "the palette walks with the arrows"
        );
    }

    #[test]
    fn the_wheel_scrolls_the_terminals_scrollback() {
        let mut app = mouse_app();
        app.terminal.grid = terminal_view::TermGrid::with_scrollback(60, 20, 40);
        let feed: String = (0..30).map(|line| format!("line{line}\r\n")).collect();
        app.terminal.grid.feed(feed.as_bytes());
        app.terminal.autoscroll = false;
        app.terminal.offset = 3;
        let mut sticky = StickyMods::default();

        handle_mouse(
            &mut app,
            &mut sticky,
            mouse(MouseEventKind::ScrollUp, 40, 5),
        );
        let scrolled = app.terminal.offset;
        assert!(
            scrolled > 3,
            "wheel up walks back, not forward ({scrolled})"
        );
        assert!(scrolled <= app.terminal.grid.scrollback_len());

        handle_mouse(
            &mut app,
            &mut sticky,
            mouse(MouseEventKind::ScrollDown, 40, 5),
        );
        assert!(
            app.terminal.offset < scrolled,
            "wheel down walks forward again"
        );
    }

    #[test]
    fn an_oversized_selection_is_cut_on_a_glyph_boundary() {
        assert!(matches!(cap_osc52("héllo"), std::borrow::Cow::Borrowed(_)));

        // The byte the cap lands on is inside a three-byte glyph.
        let big = format!("{}中", "a".repeat(OSC52_MAX_BYTES - 1));
        let capped = cap_osc52(&big);
        assert!(capped.len() <= OSC52_MAX_BYTES);
        assert_eq!(
            capped.chars().count(),
            OSC52_MAX_BYTES - 1,
            "cut on the glyph boundary, never through the glyph"
        );
        assert!(capped.chars().all(|ch| ch == 'a'));
    }

    /// A worker that has already answered — the way `poll_clipboard` is tested
    /// on a machine that may or may not have a clipboard helper installed.
    fn answered_copy(took: bool, chars: usize, osc: Option<usize>) -> clipboard::ClipboardJob {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = tx.send(took);
        clipboard::ClipboardJob::for_copy(rx, chars, osc)
    }

    fn answered_device(took: bool) -> clipboard::ClipboardJob {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = tx.send(took);
        clipboard::ClipboardJob::for_device(rx)
    }

    /// Nothing is claimed before a worker answers. The old code toasted
    /// "Copied." the moment the bytes left stdout, which is precisely how a
    /// VTE terminal (GNOME Terminal: GNOME bug 795774) got away with ignoring
    /// them while the toast insisted otherwise.
    #[test]
    fn a_copy_queues_its_report_instead_of_claiming_one() {
        let mut app = mouse_app();
        let before = app.notices.log.len();

        copy_to_host(&mut app, "héllo");

        assert_eq!(
            app.notices.log.len(),
            before,
            "no copy is reported before a helper has answered"
        );
        let job = app.clipboard_jobs.last().expect("a copy job is queued");
        let report = job.copy.expect("…tagged as our own copy, not a device's");
        assert_eq!(report.chars, 5, "five characters");
        assert_eq!(report.osc, Some(5), "all five went out over OSC 52 too");
    }

    /// A helper that took the text is a copy that really happened — the same
    /// thing `navigator.clipboard.writeText` resolving means on the web — and
    /// it is reported as one.
    #[test]
    fn a_copy_a_helper_took_is_reported_as_a_copy() {
        let mut app = test_app();
        let mut sticky = StickyMods::default();
        app.clipboard_jobs.push(answered_copy(true, 5, Some(5)));

        poll_clipboard(&mut app, &mut sticky);

        let (_, text) = app.notices.log.last().expect("the copy is reported");
        assert_eq!(
            text.as_str(),
            tr!(t(palette::PAL_MSG_COPIED, app.lang()), 5)
        );
        assert!(app.clipboard_jobs.is_empty(), "the job was collected");
    }

    /// No helper on this desktop: the text went to the emulator over OSC 52
    /// and nowhere else, so that is what the corner says — with the reason and
    /// the fix, once per session rather than once per selection.
    #[test]
    fn a_copy_with_no_helper_says_it_only_went_to_the_terminal() {
        let mut app = test_app();
        let mut sticky = StickyMods::default();
        app.clipboard_jobs.push(answered_copy(false, 7, Some(7)));

        poll_clipboard(&mut app, &mut sticky);

        let last = app.notices.log.len() - 1;
        assert_eq!(
            app.notices.log[last].1,
            tr!(t(palette::PAL_MSG_COPY_OSC52_ONLY, app.lang()), 7),
            "what really happened, not a claim"
        );
        assert_eq!(
            app.notices.log[last - 1].1,
            t(MOD_COPY_NO_HELPER, app.lang()),
            "the reason and the fix ride along"
        );
        assert!(app.copy_hint_shown);

        app.clipboard_jobs.push(answered_copy(false, 3, Some(3)));
        poll_clipboard(&mut app, &mut sticky);
        let repeats = app
            .notices
            .log
            .iter()
            .filter(|(_, text)| text.as_str() == t(MOD_COPY_NO_HELPER, app.lang()))
            .count();
        assert_eq!(repeats, 1, "once per session, not once per selection");
    }

    /// An inbound `OSC 52` has no "copied N characters" to give: only its
    /// failure reaches the log, and a copy of ours never doubles as its news.
    #[test]
    fn an_inbound_write_reports_only_when_it_failed() {
        let mut app = test_app();
        let mut sticky = StickyMods::default();
        let before = app.notices.log.len();

        app.clipboard_jobs.push(answered_device(true));
        poll_clipboard(&mut app, &mut sticky);
        assert_eq!(
            app.notices.log.len(),
            before,
            "a device copy that landed says nothing on our behalf"
        );

        app.clipboard_jobs.push(answered_device(false));
        poll_clipboard(&mut app, &mut sticky);
        let (_, text) = app.notices.log.last().expect("the failure is said");
        assert_eq!(text, t(MOD_CLIP_WRITE_FAILED, app.lang()));
        assert!(
            !app.copy_hint_shown,
            "the helper hint belongs to our own copies"
        );
    }
}
