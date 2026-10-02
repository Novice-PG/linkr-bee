//! Connection flow of the sidebar / palette `connect` action.
//!
//! The (re)connect runs on the TUI's own runtime and reports back through a
//! oneshot the event loop polls, so the UI keeps drawing while the radio or
//! the WebSocket handshake is in flight.

use std::time::Duration;

use crate::event::{ConnectionState, NoticeLevel};
use crate::session::{spawn_session, SessionHandle, SessionOptions, TransportSpec};
use crate::transport::TransportKind;

use super::settings::TransportChoice;
use super::state::App;

/// Web `WS_CONNECT_TIMEOUT_MS` is 15 s; BLE keeps the CLI default scan
/// timeout of 8 s (`--timeout 8.0`).
const BLE_TIMEOUT: Duration = Duration::from_secs(8);

fn lan_token(raw: &str) -> Result<Option<String>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let valid = trimmed.len() == 32
        && !trimmed.chars().any(char::is_uppercase)
        && trimmed.chars().all(|c| c.is_ascii_hexdigit());
    if !valid {
        return Err("The access token must be 32 lowercase hex characters.".to_string());
    }
    Ok(Some(trimmed.to_string()))
}

/// Validate the current form and start connecting. Returns the reason when the
/// input is not usable (the caller toasts it).
pub fn start(app: &mut App) -> Result<(), String> {
    if app.connected() || app.pending_connect.is_some() {
        return Err("Already connected.".to_string());
    }
    let transport = app.transport_choice();
    let spec = match transport {
        TransportChoice::Lan => {
            let host = app.sidebar.lan_host.text.trim().to_string();
            if host.is_empty() {
                return Err("Enter the device address first.".to_string());
            }
            TransportSpec::Lan {
                host,
                token: lan_token(&app.sidebar.lan_token.text)?,
            }
        }
        TransportChoice::Ble => TransportSpec::Ble {
            name: app.sidebar.ble_name.text.clone(),
            address: None,
            timeout: BLE_TIMEOUT,
        },
    };
    let opts = SessionOptions {
        transport: spec,
        ble_write_size: 0,
        log_file: None,
        debug_io: false,
        geometry: false,
    };
    app.state = ConnectionState::Connecting;
    app.detail = format!("connecting over {}…", transport.label());

    let bus = app.bus.clone();
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.rt.spawn(async move {
        let outcome = spawn_session(opts, bus)
            .await
            .map_err(|err| err.to_string());
        let _ = tx.send(outcome);
    });
    app.pending_connect = Some(rx);
    Ok(())
}

/// Sidebar / palette entry point: toast the reason on invalid input.
pub fn connect(app: &mut App) {
    match start(app) {
        Ok(()) => app
            .notices
            .push(NoticeLevel::Info, "connecting…".to_string()),
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
                app.detail = "connect task stopped".to_string();
                app.notices.push(
                    NoticeLevel::Error,
                    "Connect failed: task stopped.".to_string(),
                );
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
            app.notices
                .push(NoticeLevel::Error, format!("Connect failed: {message}"));
        }
    }
}

/// Take ownership of a freshly connected session.
fn adopt(app: &mut App, session: SessionHandle) {
    app.session = session;
    app.refresh_info();
    if app.info.kind == Some(TransportKind::Lan) {
        app.settings.transport = TransportChoice::Lan;
    }
    app.state = ConnectionState::Connected;
    app.detail = app.info.label.clone();
    app.terminal.set_autoscroll(app.settings.autoscroll);
    app.notices.push(
        NoticeLevel::Info,
        format!(
            "Connected{}.",
            if app.info.label.is_empty() {
                String::new()
            } else {
                format!(" to {}", app.info.label)
            }
        ),
    );
    // Geometry sync: push the current pane size to the new session.
    let (cols, rows) = app.terminal.dims;
    app.session.set_terminal_size(cols, rows);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_rule_matches_the_web_error_text() {
        assert_eq!(lan_token("").unwrap(), None);
        assert_eq!(
            lan_token("0123456789abcdef0123456789abcdef").unwrap(),
            Some("0123456789abcdef0123456789abcdef".to_string())
        );
        let uppercase = "0123456789ABCDEF0123456789ABCDEF";
        assert_eq!(
            lan_token(uppercase).unwrap_err(),
            "The access token must be 32 lowercase hex characters."
        );
        let short = "0123456789abcdef";
        assert_eq!(
            lan_token(short).unwrap_err(),
            "The access token must be 32 lowercase hex characters."
        );
        let nonhex = "0123456789abcdef0123456789abcdeg";
        assert_eq!(
            lan_token(nonhex).unwrap_err(),
            "The access token must be 32 lowercase hex characters."
        );
    }
}
