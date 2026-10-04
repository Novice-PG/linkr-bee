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
use crate::transport::TransportKind;

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
    if app.connected() || app.pending_connect.is_some() {
        return Err(t(CONN_ALREADY, app.lang()).to_string());
    }
    // The token a BLE session captured lives in the store until the dial
    // starts; the web fills its field from `lanTokens` before connecting too.
    if matches!(app.transport_choice(), TransportChoice::Lan) && app.sidebar.lan_token.is_empty() {
        let store = crate::lan_token_store::TokenStore::load();
        super::sidebar::fill_token_from_store(app, &store);
    }
    let opts = SessionOptions {
        transport: spec(app)?,
        ble_write_size: 0,
        log_file: None,
        debug_io: false,
        geometry: false,
    };
    begin(app, opts, SessionSetup::default());
    Ok(())
}

/// The target the form describes, or the reason it is not dialable yet (pure:
/// `start` dials what this returns, tests read it without touching a radio).
fn spec(app: &App) -> Result<TransportSpec, String> {
    let lang = app.lang();
    match app.transport_choice() {
        TransportChoice::Lan => {
            let host = app.sidebar.lan_host.text.trim().to_string();
            if host.is_empty() {
                return Err(t(CONN_NO_HOST, lang).to_string());
            }
            Ok(TransportSpec::Lan {
                host,
                token: lan_token(&app.sidebar.lan_token.text, lang)?,
            })
        }
        TransportChoice::Ble => Ok(TransportSpec::Ble {
            name: app.sidebar.ble_name.text.clone(),
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
            app.sidebar.ble_name.text = name.clone();
            app.settings.last_ble_address = address.clone().unwrap_or_default();
        }
        TransportSpec::Lan { host, token } => {
            app.settings.transport = TransportChoice::Lan;
            app.sidebar.lan_host.text = host.clone();
            app.sidebar.lan_token.text = token.clone().unwrap_or_default();
            app.settings.last_lan_host = host.clone();
        }
    }
}

/// Sidebar / palette entry point: toast the reason on invalid input.
pub fn connect(app: &mut App) {
    let lang = app.lang();
    match start(app) {
        Ok(()) => app
            .notices
            .push(NoticeLevel::Info, t(CONN_CONNECTING, lang).to_string()),
        Err(message) => app.notices.push(NoticeLevel::Warn, message),
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
    app.state = ConnectionState::Connected;
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
        assert_eq!(app.sidebar.ble_name.text, "Linkr BLE UART-3");
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
        assert_eq!(lan_app.sidebar.lan_host.text, "192.168.0.104");
        assert_eq!(
            lan_app.sidebar.lan_token.text,
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
