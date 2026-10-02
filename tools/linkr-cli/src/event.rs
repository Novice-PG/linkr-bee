//! Cross-module event contract (owned by the integrator, see CONTRACTS.md).

use serde::{Deserialize, Serialize};

pub type RequestId = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Connected,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MgmtKind {
    Response,
    Event,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeLevel {
    Info,
    Warn,
    Error,
}

/// One event on the session bus. Every subscriber (CLI printer, TUI panes,
/// assistant) sees the same stream; UART bytes are duplicated per subscriber
/// on purpose, exactly like the web client keeps an xterm scrollback and a
/// separate serial journal.
#[derive(Debug, Clone)]
pub enum CoreEvent {
    /// Connection lifecycle. `detail` is the human-readable message the web
    /// client would log as `[connect] / [ready] / [disconnected]`.
    Connection {
        state: ConnectionState,
        detail: String,
    },
    /// Raw UART bytes received from the target, already de-framed.
    UartRx(Vec<u8>),
    /// A complete management message (response or FINAL/intermediate event).
    /// `command` is the redacted display text of the request, if tracked.
    MgmtMessage {
        kind: MgmtKind,
        id: RequestId,
        ok: bool,
        final_: bool,
        lines: Vec<String>,
        command: Option<String>,
    },
    Notice {
        level: NoticeLevel,
        text: String,
    },
}
