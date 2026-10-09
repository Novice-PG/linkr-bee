//! Connection flow of the sidebar / palette `connect` action.
//!
//! The (re)connect runs on the TUI's own runtime and reports back through a
//! oneshot the event loop polls, so the UI keeps drawing while the radio or
//! the WebSocket handshake is in flight.

use std::time::Duration;

use crate::event::{ConnectionState, NoticeLevel};
use crate::session::{
    spawn_session_with, SessionHandle, SessionOptions, SessionSetup, TransportSpec,
};
use crate::transport::{DiscoveredDevice, TransportKind};

use super::dialogs::Dialog;
use super::i18n::{strings, t, tr, Lang};
use super::settings::TransportChoice;
use super::state::App;

strings! {
    CONN_TOKEN => "The access token must be 32 lowercase hex characters.",
        "访问令牌必须是 32 位小写十六进制字符。";
    CONN_ALREADY => "Already connected.", "已经连接。";
    CONN_NO_HOST => "Enter the device address first.", "请先输入设备地址。";
    CONN_CONNECTING_OVER => "connecting over {}…", "正在通过 {} 连接…";
    CONN_CONNECTING => "connecting…", "连接中…";
    CONN_TASK_STOPPED => "connect task stopped", "连接任务已停止";
    CONN_FAILED_STOPPED => "Connect failed: task stopped.", "连接失败：任务已停止。";
    CONN_FAILED => "Connect failed: {}", "连接失败：{}";
    CONN_CONNECTED => "Connected{}.", "已连接{}。";
    CONN_CONNECTED_TO => " to {}", "到 {}";
    CONN_SCAN_FAILED => "Scan failed: {}", "搜索失败：{}";
    CONN_SCAN_STOPPED => "Scan stopped.", "搜索已停止。";
    CONN_SAVE_FAILED => "Could not save the settings: {}", "无法保存设置：{}";
}

/// Web `WS_CONNECT_TIMEOUT_MS` is 15 s; BLE keeps the CLI default scan
/// timeout of 8 s (`--timeout 8.0`).
const BLE_TIMEOUT: Duration = Duration::from_secs(8);

fn lan_token(raw: &str, lang: Lang) -> Result<Option<String>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let valid = trimmed.len() == 32
        && !trimmed.chars().any(char::is_uppercase)
        && trimmed.chars().all(|c| c.is_ascii_hexdigit());
    if !valid {
        return Err(t(CONN_TOKEN, lang).to_string());
    }
    Ok(Some(trimmed.to_string()))
}

/// Validate the current form and start connecting. Returns the reason when the
/// input is not usable (the caller toasts it).
pub fn start(app: &mut App) -> Result<(), String> {
    // A dial in flight is *not* "already connected": it is what the bottom
    // status line is reporting at this very moment (`connecting...`, from the
    // session's own Connection event). Folding it into CONN_ALREADY made the
    // corner toast read "Already connected." while the bar said the link was
    // still being set up — the two disagreeing on one screen. The startup
    // deferred connect (A5) makes this the *first* press, not just a double.
    if app.pending_connect.is_some() {
        return Err(t(CONN_CONNECTING, app.lang()).to_string());
    }
    if app.connected() {
        return Err(t(CONN_ALREADY, app.lang()).to_string());
    }
    // The token a BLE session captured lives in the store until the dial
    // starts; the web fills its field from `lanTokens` before connecting too.
    if matches!(app.transport_choice(), TransportChoice::Lan) && app.sidebar.lan_token.is_empty() {
        let store = crate::lan_token_store::TokenStore::load();
        super::sidebar::fill_token_from_store(app, &store);
    }
    let opts = dial_options(app)?;
    begin(app, opts, SessionSetup::default());
    Ok(())
}

/// What a dial from this form starts with.
///
/// Split out of [`start`] because the switches it carries are the ones a
/// reconnect has to keep: this literal used to rebuild them from nothing, so
/// `--debug-io` (and now the palette's toggle) survived exactly one connect and
/// then went quiet.
fn dial_options(app: &App) -> Result<SessionOptions, String> {
    Ok(SessionOptions {
        transport: spec(app)?,
        ble_write_size: app.settings.ble_write_size,
        log_file: None,
        debug_io: app.settings.debug_io,
        geometry: false,
    })
}

/// The target the form describes, or the reason it is not dialable yet (pure:
/// `start` dials what this returns, tests read it without touching a radio).
fn spec(app: &App) -> Result<TransportSpec, String> {
    let lang = app.lang();
    match app.transport_choice() {
        TransportChoice::Lan => {
            let host = app.sidebar.lan_host.as_str().trim().to_string();
            if host.is_empty() {
                return Err(t(CONN_NO_HOST, lang).to_string());
            }
            Ok(TransportSpec::Lan {
                host,
                token: lan_token(app.sidebar.lan_token.as_str(), lang)?,
            })
        }
        TransportChoice::Ble => Ok(TransportSpec::Ble {
            name: app.sidebar.ble_name.as_str().to_string(),
            // A `--address` the CLI was started with is remembered, so a
            // retry dials that peripheral instead of the first name match.
            address: non_empty(&app.settings.last_ble_address),
            timeout: BLE_TIMEOUT,
        }),
    }
}

fn non_empty(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Dial `opts` in the background: the state/detail flip now, the outcome
/// arrives through [`poll`]. Shared by the sidebar action and by the CLI's
/// deferred connect, which hands its own options over instead of dialling
/// before the interface opens (A5).
fn begin(app: &mut App, opts: SessionOptions, setup: SessionSetup) {
    let lang = app.lang();
    let label = match &opts.transport {
        TransportSpec::Lan { .. } => TransportChoice::Lan.label(),
        TransportSpec::Ble { .. } => TransportChoice::Ble.label(),
    };
    app.state = ConnectionState::Connecting;
    app.detail = tr!(t(CONN_CONNECTING_OVER, lang), label);
    // The corner may only describe *this* attempt: a "Connected to …." toast
    // from the session that just dropped lives for TOAST_LIFETIME (2.2 s) and
    // would otherwise sit there claiming success while the bar below already
    // reads "connecting…". The notice log keeps the history either way.
    app.notices.toasts.clear();
    let bus = app.bus.clone();
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.rt.spawn(async move {
        let outcome = spawn_session_with(opts, bus, setup)
            .await
            .map_err(|err| err.to_string());
        let _ = tx.send(outcome);
    });
    app.pending_connect = Some(rx);
}

/// Entry point for the connect the CLI deferred: point the sidebar at the
/// target the process was started with (so a manual retry dials the same
/// device), then dial in the background.
pub fn begin_cli(app: &mut App, opts: SessionOptions, setup: SessionSetup) {
    seed_form(app, &opts);
    begin(app, opts, setup);
    let lang = app.lang();
    app.notices
        .push(NoticeLevel::Info, t(CONN_CONNECTING, lang).to_string());
}

/// Mirror `opts` into the sidebar form and the persisted transport choice.
fn seed_form(app: &mut App, opts: &SessionOptions) {
    match &opts.transport {
        TransportSpec::Ble { name, address, .. } => {
            app.settings.transport = TransportChoice::Ble;
            app.sidebar.ble_name.set(name.clone());
            app.settings.last_ble_address = address.clone().unwrap_or_default();
        }
        TransportSpec::Lan { host, token } => {
            app.settings.transport = TransportChoice::Lan;
            app.sidebar.lan_host.set(host.clone());
            app.sidebar.lan_token.set(token.clone().unwrap_or_default());
            app.settings.last_lan_host = host.clone();
        }
    }
}

/// Sidebar / palette entry point: toast the reason on invalid input.
pub fn connect(app: &mut App) {
    let lang = app.lang();
    // The attempt already running owns the status line: its own
    // "connecting…" toast from the first press plus the bottom bar. Adding a
    // second one would only stack a redundant line in the corner, so a repeat
    // press changes nothing.
    if app.pending_connect.is_some() {
        return;
    }
    // A host edited moments ago still has its token re-selection pending; run
    // it before the form is read, so the dial sees host and token together.
    super::sidebar::flush(app);
    match start(app) {
        Ok(()) => app
            .notices
            .push(NoticeLevel::Info, t(CONN_CONNECTING, lang).to_string()),
        Err(message) => app.notices.push(NoticeLevel::Warn, message),
    }
}

/// Sweep for Linkr accessories and open the picker. The web hands
/// `switchDeviceButton` straight to `connect({ chooseDevice: true })`, which
/// calls `requestDevice()` and lets the browser scan *and* list; a terminal
/// owns both halves, so the box opens first (with "scanning…") and fills when
/// the radio answers — pressing the entry used to just move the cursor onto a
/// text field, which is why there was no searching and no choosing at all.
pub fn begin_scan(app: &mut App) {
    let lang = app.lang();
    // Nothing to pick while a dial is in flight (the entry is hidden while
    // connected, so this is the only overlap worth guarding). It has to be
    // checked **first**: with a sweep and a dial both running — sweep, Esc,
    // then Connect — the reopen below used to bring the picker up beside the
    // dial, and choosing there rewrote host and transport behind the lock
    // that was refusing exactly that, then closed the box and dialled nothing.
    if app.pending_connect.is_some() {
        app.notices
            .push(NoticeLevel::Warn, t(CONN_CONNECTING, lang).to_string());
        return;
    }
    if app.pending_scan.is_some() {
        // A sweep is already running — pressing Esc on the picker drops the
        // box but not the scan (`dialogs` Devices arm). Reopen the box on the
        // running sweep: returning silently left the entry dead for up to
        // `BLE_TIMEOUT`, and the result was then thrown away because
        // `fill_scan_dialog` found no picker.
        app.dialog = Some(Dialog::Devices {
            items: Vec::new(),
            selected: 0,
            scanning: true,
        });
        return;
    }
    app.dialog = Some(Dialog::Devices {
        items: Vec::new(),
        selected: 0,
        scanning: true,
    });
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.pending_scan = Some(rx);
    app.rt.spawn(async move {
        let outcome = crate::transport::ble::scan(BLE_TIMEOUT)
            .await
            .map(|mut devices| {
                // The same order the CLI's `--scan` table prints, so the two
                // listings line up row for row.
                crate::transport::ble::sort_for_display(&mut devices);
                devices
            })
            .map_err(|err| err.to_string());
        let _ = tx.send(outcome);
    });
}

/// Drain the in-flight scan (once per loop tick) and turn its result into the
/// picker's rows.
pub fn poll_scan(app: &mut App) {
    let outcome = {
        let Some(rx) = &mut app.pending_scan else {
            return;
        };
        match rx.try_recv() {
            Ok(outcome) => outcome,
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => return,
            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                app.pending_scan = None;
                fill_scan_dialog(app, Vec::new());
                let lang = app.lang();
                app.notices
                    .push(NoticeLevel::Warn, t(CONN_SCAN_STOPPED, lang).to_string());
                return;
            }
        }
    };
    app.pending_scan = None;
    let lang = app.lang();
    match outcome {
        Ok(items) => fill_scan_dialog(app, items),
        Err(message) => {
            // A failed sweep still has a box on screen: empty it so the reader
            // sees the failure in the corner instead of waiting on a list that
            // is never coming.
            fill_scan_dialog(app, Vec::new());
            app.notices
                .push(NoticeLevel::Warn, tr!(t(CONN_SCAN_FAILED, lang), message));
        }
    }
}

/// Put the swept devices into the open picker (a no-op if the user already
/// dismissed it — the result is dropped, not resurrected over their input).
fn fill_scan_dialog(app: &mut App, items: Vec<DiscoveredDevice>) {
    if let Some(Dialog::Devices {
        items: slot,
        scanning,
        ..
    }) = &mut app.dialog
    {
        *slot = items;
        *scanning = false;
    }
}

/// Dial the device the picker's cursor is on.
pub fn pick(app: &mut App, device: DiscoveredDevice) {
    aim(app, &device);
    let lang = app.lang();
    if let Err(err) = super::settings::save(&app.settings) {
        app.notices
            .push(NoticeLevel::Warn, tr!(t(CONN_SAVE_FAILED, lang), err));
    }
    connect(app);
}

/// Point the connection form at `device`. Split out from [`pick`] so the test
/// can pin both things a pick has to fix without writing a settings file or
/// waking the radio.
fn aim(app: &mut App, device: &DiscoveredDevice) {
    app.dialog = None;
    app.settings.transport = TransportChoice::Ble;
    // The address is what the retry path dials, exactly like a `--address` the
    // CLI was started with; the name is only a display/match convenience.
    app.settings.last_ble_address = device.address.clone();
    // The field held the *prefix* it was asked to match (`Linkr BLE UART`),
    // which is why the sidebar never showed the `-3` the board actually
    // advertises. The scan sees the real name, so write that back.
    if let Some(name) = device.name.as_ref().filter(|name| !name.trim().is_empty()) {
        app.sidebar.ble_name.set(name.clone());
    }
}

/// Drain the in-flight connect (once per loop tick).
pub fn poll(app: &mut App) {
    let outcome = {
        let Some(rx) = &mut app.pending_connect else {
            return;
        };
        match rx.try_recv() {
            Ok(outcome) => outcome,
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => return,
            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                app.pending_connect = None;
                app.state = ConnectionState::Failed;
                let lang = app.lang();
                app.detail = t(CONN_TASK_STOPPED, lang).to_string();
                app.notices
                    .push(NoticeLevel::Error, t(CONN_FAILED_STOPPED, lang).to_string());
                return;
            }
        }
    };
    app.pending_connect = None;
    match outcome {
        Ok(session) => adopt(app, session),
        Err(message) => {
            app.state = ConnectionState::Failed;
            app.detail = message.clone();
            let lang = app.lang();
            app.notices
                .push(NoticeLevel::Error, tr!(t(CONN_FAILED, lang), message));
        }
    }
}

/// Take ownership of a freshly connected session.
fn adopt(app: &mut App, session: SessionHandle) {
    app.session = session;
    app.refresh_info();
    // The handshake is done: this is where the web asks the device for its
    // state (`requestDeviceState()`), token included.
    super::start_lan_token_capture(app);
    if app.info.kind == Some(TransportKind::Lan) {
        app.settings.transport = TransportChoice::Lan;
    }
    // Edge-triggered: if `refresh_info()` above already saw the link, this is
    // a no-op side-effect-wise; if the session reports it a tick later, this
    // is where "on connect" fires (spec §6.4).
    app.set_connection_state(ConnectionState::Connected);
    app.detail = app.info.label.clone();
    app.terminal.set_autoscroll(app.settings.autoscroll);
    let lang = app.lang();
    let label = app.info.label.clone();
    let tail = if label.is_empty() {
        String::new()
    } else {
        tr!(t(CONN_CONNECTED_TO, lang), label)
    };
    app.notices
        .push(NoticeLevel::Info, tr!(t(CONN_CONNECTED, lang), tail));
    // Geometry sync: push the current pane size to the new session.
    let (cols, rows) = app.terminal.dims;
    app.session.set_terminal_size(cols, rows);
}

#[cfg(test)]
mod tests {
    use super::super::test_app;
    use super::*;

    /// What "Switch device" has to leave behind after Enter: the address the
    /// retry path dials, and the **full** advertised name in the sidebar. The
    /// field used to keep the match prefix (`Linkr BLE UART`), which is why
    /// every board's `-3` was never on screen.
    #[test]
    fn picking_a_scanned_device_dials_its_address_and_shows_its_full_name() {
        let mut app = test_app();
        app.sidebar.ble_name.set("Linkr BLE UART".to_string());
        app.settings.last_ble_address.clear();
        app.dialog = Some(Dialog::Devices {
            items: Vec::new(),
            selected: 0,
            scanning: true,
        });

        aim(
            &mut app,
            &DiscoveredDevice {
                address: "EE:C7:42:34:48:CF".to_string(),
                name: Some("Linkr BLE UART-3".to_string()),
                rssi: Some(-58),
            },
        );

        assert!(app.dialog.is_none(), "the picker must close on Enter");
        assert_eq!(app.settings.last_ble_address, "EE:C7:42:34:48:CF");
        assert_eq!(app.sidebar.ble_name.as_str(), "Linkr BLE UART-3");
        match spec(&app).expect("the picked device is dialable") {
            TransportSpec::Ble { name, address, .. } => {
                assert_eq!(address.as_deref(), Some("EE:C7:42:34:48:CF"));
                assert_eq!(name, "Linkr BLE UART-3");
            }
            other => panic!("expected BLE, got {other:?}"),
        }
    }

    /// Not every advertisement carries a name. Blanking the field would throw
    /// away the prefix a fresh name match still needs, so only a real name
    /// overwrites it — the address is what actually dials either way.
    #[test]
    fn picking_a_nameless_device_keeps_the_name_field() {
        let mut app = test_app();
        app.sidebar.ble_name.set("Linkr BLE UART".to_string());
        app.settings.last_ble_address.clear();

        aim(
            &mut app,
            &DiscoveredDevice {
                address: "AA:BB:CC:DD:EE:FF".to_string(),
                name: None,
                rssi: None,
            },
        );

        assert_eq!(app.sidebar.ble_name.as_str(), "Linkr BLE UART");
        assert_eq!(app.settings.last_ble_address, "AA:BB:CC:DD:EE:FF");
    }

    /// A5: the sidebar's Connect has to dial the target the process was
    /// started with (`--name`/`--address`/`--lan`/`--lan-token`), not the
    /// defaults — otherwise a retry after a failed deferred connect could
    /// pick a different peripheral.
    #[test]
    fn seed_form_points_the_sidebar_at_the_cli_target() {
        let mut app = test_app();
        let ble = SessionOptions {
            transport: TransportSpec::Ble {
                name: "Linkr BLE UART-3".to_string(),
                address: Some("EE:C7:42:34:48:CF".to_string()),
                timeout: Duration::from_secs(2),
            },
            ble_write_size: 0,
            log_file: None,
            debug_io: false,
            geometry: false,
        };
        seed_form(&mut app, &ble);
        assert_eq!(app.sidebar.ble_name.as_str(), "Linkr BLE UART-3");
        assert_eq!(app.settings.last_ble_address, "EE:C7:42:34:48:CF");
        assert_eq!(app.transport_choice(), TransportChoice::Ble);
        // The retry path reads that address back, so it never falls back to
        // "first device matching the name".
        match spec(&app).expect("the seeded form is ready to dial") {
            TransportSpec::Ble { name, address, .. } => {
                assert_eq!(name, "Linkr BLE UART-3");
                assert_eq!(address.as_deref(), Some("EE:C7:42:34:48:CF"));
            }
            other => panic!("expected BLE, got {other:?}"),
        }
        // No `--address` on the command line: match by name, like before.
        app.settings.last_ble_address.clear();
        match spec(&app).expect("a name-only form is dialable") {
            TransportSpec::Ble { address, .. } => assert_eq!(address, None),
            other => panic!("expected BLE, got {other:?}"),
        }

        let mut lan_app = test_app();
        let lan = SessionOptions {
            transport: TransportSpec::Lan {
                host: "192.168.0.104".to_string(),
                token: Some("0123456789abcdef0123456789abcdef".to_string()),
            },
            ble_write_size: 0,
            log_file: None,
            debug_io: false,
            geometry: false,
        };
        seed_form(&mut lan_app, &lan);
        assert_eq!(lan_app.sidebar.lan_host.as_str(), "192.168.0.104");
        assert_eq!(
            lan_app.sidebar.lan_token.as_str(),
            "0123456789abcdef0123456789abcdef"
        );
        assert_eq!(lan_app.settings.last_lan_host, "192.168.0.104");
        assert_eq!(lan_app.transport_choice(), TransportChoice::Lan);
        match spec(&lan_app).expect("the seeded LAN form is ready to dial") {
            TransportSpec::Lan { host, token } => {
                assert_eq!(host, "192.168.0.104");
                assert_eq!(token.as_deref(), Some("0123456789abcdef0123456789abcdef"));
            }
            other => panic!("expected LAN, got {other:?}"),
        }
    }

    /// P2: `--lan` startup seeding wrote `lan_host.text` straight over the
    /// longer value restored from `tui.json` — caret included — so the very
    /// first frame sliced past the end and the process aborted. Seeding now
    /// goes through `set()`, which moves the caret with the text.
    #[test]
    fn seeding_a_shorter_lan_host_over_a_long_restored_one_keeps_the_form_renderable() {
        let mut app = test_app();
        app.sidebar.lan_host.set("ws://192.0.2.9:99999/ws");
        app.sidebar
            .lan_token
            .set("ffffffffffffffffffffffffffffffff");
        app.settings.last_lan_host = "ws://192.0.2.9:99999/ws".to_string();

        let lan = SessionOptions {
            transport: TransportSpec::Lan {
                host: "192.0.2.1".to_string(),
                token: Some("0123456789abcdef0123456789abcdef".to_string()),
            },
            ble_write_size: 0,
            log_file: None,
            debug_io: false,
            geometry: false,
        };
        seed_form(&mut app, &lan);

        assert_eq!(app.sidebar.lan_host.as_str(), "192.0.2.1");
        // What the sidebar asks the field for on every frame: text plus the
        // caret column. Both must sit inside the seeded value.
        assert_eq!(
            app.sidebar.lan_host.display(None),
            ("192.0.2.1".to_string(), 9)
        );
        assert_eq!(
            app.sidebar.lan_token.display(Some('*')),
            ("*".repeat(32), 32)
        );
    }

    /// Web parity: `switchDeviceButton.disabled = connectionInFlight || …`
    /// (`web/app.js:847`, `:1945`), so the sweep waits for an attempt that is
    /// already running — and says so with the same "Connecting…" the status
    /// line is showing, rather than opening an empty picker beside a dial
    /// that has not settled.
    #[test]
    fn sweeping_for_devices_waits_for_an_attempt_in_flight() {
        let mut app = test_app();
        app.state = ConnectionState::Connecting;
        app.pending_connect = Some(tokio::sync::oneshot::channel().1);

        begin_scan(&mut app);

        assert!(app.dialog.is_none(), "no picker next to a running dial");
        assert!(app.pending_scan.is_none(), "…and no scan started");
        let toast = app
            .notices
            .toasts
            .last()
            .expect("the refusal explains itself");
        assert_eq!(toast.text, t(CONN_CONNECTING, app.lang()));

        // Settled, the same press opens the picker and starts the sweep.
        app.pending_connect = None;
        app.state = ConnectionState::Disconnected;
        begin_scan(&mut app);
        assert!(matches!(app.dialog, Some(Dialog::Devices { .. })));
        assert!(app.pending_scan.is_some());
    }

    /// A sweep outlives an Esc on the picker; if a dial then starts, pressing
    /// the entry again must not bring the box up **beside** that dial —
    /// choosing a device there rewrites host and transport while
    /// `transport_locked()` is refusing to move them, and the box then closes
    /// having dialled nothing.
    #[test]
    fn a_running_sweep_does_not_reopen_next_to_a_dial() {
        let mut app = test_app();
        app.state = ConnectionState::Connecting;
        app.pending_connect = Some(tokio::sync::oneshot::channel().1);
        app.pending_scan = Some(tokio::sync::oneshot::channel().1);

        begin_scan(&mut app);

        assert!(app.dialog.is_none(), "no picker next to a running dial");
        assert!(app.pending_scan.is_some(), "…and the sweep keeps running");
        let toast = app
            .notices
            .toasts
            .last()
            .expect("the refusal explains itself");
        assert_eq!(toast.text, t(CONN_CONNECTING, app.lang()));
    }

    /// The switches a reconnect dials with are the ones the settings hold.
    /// This literal used to be built from nothing, so `--debug-io` and the
    /// palette's controls both survived exactly one connect and then went
    /// quiet — which is the whole of "no switch for Debug I/O / chunk size".
    #[test]
    fn a_dial_carries_the_persisted_switches() {
        let mut app = test_app();
        app.state = ConnectionState::Disconnected;
        app.settings.transport = TransportChoice::Lan;
        app.sidebar.lan_host.set("192.168.0.104".to_string());

        let options = dial_options(&app).expect("a dialable LAN form");
        assert!(
            !options.debug_io,
            "off by default, the way the web checkbox starts"
        );
        assert_eq!(
            options.ble_write_size, 0,
            "0 is `--ble-write-size`'s auto: a plain 23-byte MTU comes out at the web's 20"
        );

        app.settings.debug_io = true;
        app.settings.ble_write_size = 180;
        let options = dial_options(&app).expect("a dialable LAN form");
        assert!(options.debug_io, "the next connect has to see the switch");
        assert_eq!(
            options.ble_write_size, 180,
            "and the chunk size the dialog saved"
        );
    }

    /// `begin` reports the state immediately; the outcome only ever lands
    /// through `poll`, so the UI keeps drawing while the radio works.
    #[test]
    fn begin_reports_connecting_before_the_radio_answers() {
        let mut app = test_app();
        app.state = ConnectionState::Disconnected;
        let opts = SessionOptions {
            transport: TransportSpec::Lan {
                host: "127.0.0.1:9".to_string(),
                token: None,
            },
            ble_write_size: 0,
            log_file: None,
            debug_io: false,
            geometry: false,
        };
        begin(&mut app, opts, SessionSetup::default());
        assert_eq!(app.state, ConnectionState::Connecting);
        assert!(app.detail.contains("connecting"), "{}", app.detail);
        assert!(app.pending_connect.is_some());
        assert!(!app.quit, "a connect in flight never quits the UI");
    }

    /// The contradiction on one screen: with a dial in flight the bottom bar
    /// reads `connecting...` (the session's own Connection event), so the
    /// corner must not claim the link is up. `pending_connect` also covers the
    /// startup deferred connect, which makes this the *first* press a user
    /// makes rather than only an impatient second one.
    #[test]
    fn a_press_while_a_dial_runs_never_says_already_connected() {
        let mut app = test_app();
        app.state = ConnectionState::Disconnected;
        let opts = SessionOptions {
            transport: TransportSpec::Lan {
                host: "127.0.0.1:9".to_string(),
                token: None,
            },
            ble_write_size: 0,
            log_file: None,
            debug_io: false,
            geometry: false,
        };
        begin(&mut app, opts, SessionSetup::default());
        assert!(app.pending_connect.is_some());
        let lang = app.lang();

        // What `connect` adds while a dial runs: nothing — the corner keeps
        // the first press's "connecting…" line instead of stacking another.
        let toasts = app.notices.toasts.len();
        connect(&mut app);
        assert_eq!(app.notices.toasts.len(), toasts, "no duplicate toast");

        // A caller going through `start` straight still gets the truth.
        let reason = start(&mut app).unwrap_err();
        assert_eq!(reason, t(CONN_CONNECTING, lang));
        assert_ne!(reason, t(CONN_ALREADY, lang));

        // A link that really is up keeps the original wording.
        app.pending_connect = None;
        app.state = ConnectionState::Connected;
        assert_eq!(start(&mut app).unwrap_err(), t(CONN_ALREADY, lang));
    }

    /// A5's core promise: a failed connect toasts and stays in the interface
    /// (the sidebar's Connect is the way back), it never exits.
    #[test]
    fn a_failed_connect_toasts_and_keeps_the_ui_running() {
        let mut app = test_app();
        app.state = ConnectionState::Connecting;
        app.quit = false;
        let (tx, rx) = tokio::sync::oneshot::channel();
        let _ = tx.send(Err(
            "device not found matching: 00:00:00:00:00:00".to_string()
        ));
        app.pending_connect = Some(rx);

        poll(&mut app);

        let lang = app.lang();
        assert_eq!(app.state, ConnectionState::Failed);
        assert!(!app.quit, "the interface must stay up");
        assert_eq!(app.detail, "device not found matching: 00:00:00:00:00:00");
        assert!(app.pending_connect.is_none(), "the attempt is spent");
        assert_eq!(
            t(CONN_FAILED, lang),
            "Connect failed: {}",
            "the reason is the template's argument"
        );
        assert!(
            app.notices
                .log
                .iter()
                .any(|(_, text)| text
                    == "Connect failed: device not found matching: 00:00:00:00:00:00"),
            "the failure reaches the notice log: {:?}",
            app.notices.log
        );
    }

    #[test]
    fn token_rule_matches_the_web_error_text() {
        assert_eq!(lan_token("", Lang::En).unwrap(), None);
        assert_eq!(
            lan_token("0123456789abcdef0123456789abcdef", Lang::En).unwrap(),
            Some("0123456789abcdef0123456789abcdef".to_string())
        );
        let uppercase = "0123456789ABCDEF0123456789ABCDEF";
        assert_eq!(
            lan_token(uppercase, Lang::En).unwrap_err(),
            "The access token must be 32 lowercase hex characters."
        );
        let short = "0123456789abcdef";
        assert_eq!(
            lan_token(short, Lang::En).unwrap_err(),
            "The access token must be 32 lowercase hex characters."
        );
        let nonhex = "0123456789abcdef0123456789abcdeg";
        assert_eq!(
            lan_token(nonhex, Lang::En).unwrap_err(),
            "The access token must be 32 lowercase hex characters."
        );
        assert_eq!(
            lan_token(short, Lang::Zh).unwrap_err(),
            t(CONN_TOKEN, Lang::Zh),
            "the same rule speaks Chinese too"
        );
    }

    #[test]
    fn every_connect_message_is_translated() {
        super::super::i18n::assert_bilingual(ALL);
        assert!(ALL.len() >= 10, "connect carries 10 messages");
    }

    /// The "Connected to …" sentence is built from two pieces so the label
    /// stays a value; both halves have to line up in each language.
    #[test]
    fn the_connected_sentence_reads_correctly_in_both_languages() {
        assert_eq!(tr!(t(CONN_CONNECTED, Lang::En), ""), "Connected.");
        assert_eq!(
            tr!(
                t(CONN_CONNECTED, Lang::En),
                tr!(t(CONN_CONNECTED_TO, Lang::En), "ttyS0")
            ),
            "Connected to ttyS0."
        );
        assert_eq!(
            tr!(
                t(CONN_CONNECTED, Lang::Zh),
                tr!(t(CONN_CONNECTED_TO, Lang::Zh), "ttyS0")
            ),
            "已连接到 ttyS0。"
        );
    }
}
