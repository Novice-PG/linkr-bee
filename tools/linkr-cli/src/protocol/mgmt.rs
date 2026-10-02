//! Management Service v1 framing and request correlation.
//! See docs/LINKR_BLE_API.zh-CN.md sections 4-6 and specs/PYTHON_CLI_SPEC.md.

use std::collections::HashMap;

use tokio::sync::oneshot;

use crate::event::{CoreEvent, MgmtKind, NoticeLevel, RequestId};
use crate::protocol::validate::python_repr_bytes;

pub const MGMT_HEADER_SIZE: usize = 12;
pub const MGMT_MAGIC: [u8; 2] = *b"LK";
pub const MGMT_API_MAJOR: u8 = 1;
pub const MGMT_FLAG_FINAL: u16 = 1 << 0;
pub const MGMT_FLAG_ERROR: u16 = 1 << 1;
pub const MGMT_CAP_WIFI: u32 = 1 << 0;
pub const MGMT_CAP_WEBDAV: u32 = 1 << 1;
pub const MGMT_CAP_WEBSOCKET: u32 = 1 << 2;
pub const MGMT_CAP_DEVICE_ID: u32 = 1 << 3;
pub const MGMT_CAP_ASYNC_EVENTS: u32 = 1 << 4;
pub const MGMT_CAP_RELIABLE_UART: u32 = 1 << 5;
/// Hard response timeout used by every request without a longer wait.
pub const MGMT_RESPONSE_TIMEOUT_SECS: f64 = 5.0;
pub const WIFI_OPERATION_TIMEOUT_SECS: f64 = 35.0;
pub const WIFI_SCAN_TIMEOUT_SECS: f64 = 35.0;

#[derive(Debug, thiserror::Error)]
pub enum MgmtError {
    #[error("management command is outside the advertised limit")]
    OutsideLimit,
    #[error("management request timed out")]
    Timeout,
    #[error("{0}")]
    Device(String),
    #[error("disconnected before management response")]
    Disconnected,
}

/// Result of one logical management request: the response lines plus every
/// intermediate event that carried the same request id.
#[derive(Debug, Clone)]
pub struct MgmtReply {
    pub id: RequestId,
    pub lines: Vec<String>,
    pub events: Vec<String>,
    pub raw: String,
    pub ok: bool,
}

/// Redacted display text of a command: `@w=ssid,secret` and `@d=...` become
/// `@w=<redacted>` / `@d=<redacted>` (prefix-triggered only — `@w off`,
/// `@w scan` and `@d off` stay in the clear). Used for the
/// `control -> {display}` line, JSON `command` fields and error messages.
pub fn display_command(command: &str) -> String {
    if command.starts_with("@w=") || command.starts_with("@d=") {
        format!("{}=<redacted>", &command[..2])
    } else {
        command.to_string()
    }
}

/// The `--json` line for one management message, byte-identical to Python's
/// `json.dumps(record, ensure_ascii=False)` with its default `", "` / `": "`
/// separators: `{"type": "response", "requestId": 1, ...}`.
pub fn json_record(
    kind: MgmtKind,
    id: RequestId,
    ok: bool,
    lines: &[String],
    command: Option<&str>,
) -> String {
    let kind = match kind {
        MgmtKind::Response => "response",
        MgmtKind::Event => "event",
    };
    let encoded: Vec<String> = lines
        .iter()
        .map(|line| serde_json::to_string(line).expect("string encodes"))
        .collect();
    let mut out = format!(
        "{{\"type\": {}, \"requestId\": {}, \"ok\": {}, \"lines\": [{}]",
        serde_json::to_string(kind).expect("string encodes"),
        id,
        ok,
        encoded.join(", ")
    );
    if let Some(command) = command {
        out.push_str(&format!(
            ", \"command\": {}",
            serde_json::to_string(command).expect("string encodes")
        ));
    }
    out.push('}');
    out
}

/// The non-JSON stderr line for one management message:
/// `response #1 <- fw=1.2.3` / `event #4 <- ERR ...`.
pub fn plain_line(kind: MgmtKind, id: RequestId, line: &str) -> String {
    let kind = match kind {
        MgmtKind::Response => "response",
        MgmtKind::Event => "event",
    };
    format!("{kind} #{id} <- {line}")
}

/// `--debug-io` trace for one ATT chunk: `MGMT TX #1 b'...'` or
/// `MGMT TX #1 <redacted 20 bytes>`.
pub fn tx_trace(id: RequestId, chunk: &[u8], sensitive: bool) -> String {
    if sensitive {
        format!("MGMT TX #{id} <redacted {} bytes>", chunk.len())
    } else {
        format!("MGMT TX #{id} {}", python_repr_bytes(chunk))
    }
}

/// Python `str.splitlines()` narrowed to the line breaks the spec names for
/// management payloads (\n, \r\n, \r). The empty result collapses to `[""]`.
fn split_lines(text: &str) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\n' => lines.push(std::mem::take(&mut current)),
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                lines.push(std::mem::take(&mut current));
            }
            other => current.push(other),
        }
    }
    if !text.ends_with('\n') && !text.ends_with('\r') {
        lines.push(current);
    }
    lines
}

struct PartialMessage {
    message_type: u8,
    request_id: RequestId,
    expected: usize,
    flags: u16,
    payload: Vec<u8>,
}

/// Pure state machine: request-id allocation, request chunking, indication
/// reassembly, pending-future resolution and JSON/display emission.
///
/// `feed()` is called for every indication received on the management
/// characteristic; it returns the display event (if any) and resolves pending
/// requests internally, exactly like the Python `ManagementChannel`.
pub struct MgmtCore {
    max_payload: u16,
    json: bool,
    write_size: usize,
    next_request_id: RequestId,
    current: Option<PartialMessage>,
    pending: HashMap<RequestId, oneshot::Sender<Result<MgmtReply, MgmtError>>>,
    pending_final: HashMap<RequestId, oneshot::Sender<Result<MgmtReply, MgmtError>>>,
    /// Command text per in-flight request, so a JSON record can say what it
    /// answers. Sensitive commands are stored already redacted.
    commands: HashMap<RequestId, String>,
    /// Lines of intermediate (non-FINAL) events seen while a caller waits.
    events: HashMap<RequestId, Vec<String>>,
}

impl MgmtCore {
    pub fn new(max_payload: u16, json: bool) -> Self {
        Self {
            max_payload,
            json,
            write_size: 20,
            next_request_id: 1,
            current: None,
            pending: HashMap::new(),
            pending_final: HashMap::new(),
            commands: HashMap::new(),
            events: HashMap::new(),
        }
    }

    /// Build a channel sized by the negotiated management payload
    /// (`handshake.protocol.max_payload`) with `--json` off: `new(payload, false)`.
    /// Additive helper for `session.rs`, which has no JSON source yet.
    pub fn new_sized(max_payload: u16) -> Self {
        Self::new(max_payload, false)
    }

    /// Whether this core was created for `--json` output (the session still
    /// renders through `json_record`/`plain_line`).
    pub fn json(&self) -> bool {
        self.json
    }

    /// ATT chunk size for outgoing frames: Python's `max(20, write_size)`.
    /// The management path is deliberately not clamped to 244.
    pub fn set_write_size(&mut self, size: usize) {
        self.write_size = size;
    }

    /// Allocate a request id, encode `cmd` into ATT chunks (first chunk holds
    /// the full 12-byte header) and register the reply receivers.
    /// Returns `(id, chunks, response_rx, final_rx_opt)`.
    #[allow(clippy::type_complexity)]
    pub fn begin(
        &mut self,
        cmd: &str,
        wait_final: bool,
    ) -> Result<
        (
            RequestId,
            Vec<Vec<u8>>,
            oneshot::Receiver<Result<MgmtReply, MgmtError>>,
            Option<oneshot::Receiver<Result<MgmtReply, MgmtError>>>,
        ),
        MgmtError,
    > {
        let bytes = cmd.as_bytes();
        if bytes.is_empty() || bytes.len() > self.max_payload as usize {
            return Err(MgmtError::OutsideLimit);
        }

        let display = display_command(cmd);
        let request_id = self.next_request_id;
        // Wrap: 0xFFFFFFFF -> 1 (never 0).
        self.next_request_id = if request_id == u32::MAX {
            1
        } else {
            request_id + 1
        };
        self.commands.insert(request_id, display);

        let mut frame = Vec::with_capacity(MGMT_HEADER_SIZE + bytes.len());
        frame.extend_from_slice(&MGMT_MAGIC);
        frame.push(MGMT_API_MAJOR);
        frame.push(1); // message type: host command
        frame.extend_from_slice(&request_id.to_le_bytes());
        frame.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
        frame.extend_from_slice(&0u16.to_le_bytes()); // flags: 0 on TX
        frame.extend_from_slice(bytes);
        let size = self.write_size.max(20);
        let chunks = frame.chunks(size).map(<[u8]>::to_vec).collect();

        // Waiters are registered before any GATT write, so a response that
        // arrives mid-write still resolves the request.
        let (tx, rx) = oneshot::channel();
        self.pending.insert(request_id, tx);
        let final_rx = if wait_final {
            let (final_tx, final_rx) = oneshot::channel();
            self.pending_final.insert(request_id, final_tx);
            Some(final_rx)
        } else {
            None
        };
        Ok((request_id, chunks, rx, final_rx))
    }

    /// Feed one indication from the management characteristic. Returns the
    /// `CoreEvent::MgmtMessage` to publish (pending requests resolve here too).
    /// Reassembly warnings come back as `CoreEvent::Notice`.
    pub fn feed(&mut self, bytes: &[u8]) -> Option<CoreEvent> {
        let mut fragment = bytes;
        if self.current.is_none() {
            if fragment.len() < MGMT_HEADER_SIZE || fragment[..2] != MGMT_MAGIC {
                return Some(CoreEvent::Notice {
                    level: NoticeLevel::Warn,
                    text: "management <- orphaned response fragment".to_string(),
                });
            }
            let version = fragment[2];
            let message_type = fragment[3];
            let request_id = u32::from_le_bytes(fragment[4..8].try_into().expect("4 bytes"));
            let expected = u16::from_le_bytes(fragment[8..10].try_into().expect("2 bytes"));
            let flags = u16::from_le_bytes(fragment[10..12].try_into().expect("2 bytes"));
            if version != MGMT_API_MAJOR
                || !matches!(message_type, 2 | 3)
                || expected == 0
                || expected > self.max_payload
            {
                return Some(CoreEvent::Notice {
                    level: NoticeLevel::Warn,
                    text: "management <- invalid response header".to_string(),
                });
            }
            self.current = Some(PartialMessage {
                message_type,
                request_id,
                expected: expected as usize,
                flags,
                payload: Vec::with_capacity(expected as usize),
            });
            fragment = &fragment[MGMT_HEADER_SIZE..];
        }

        let oversized = {
            let current = self.current.as_mut().expect("current message");
            current.payload.len() + fragment.len() > current.expected
        };
        if oversized {
            // Oversized: drop the whole message, header included.
            self.current = None;
            return Some(CoreEvent::Notice {
                level: NoticeLevel::Warn,
                text: "management <- oversized response".to_string(),
            });
        }
        let current = self.current.as_mut().expect("current message");
        current.payload.extend_from_slice(fragment);
        if current.payload.len() != current.expected {
            return None; // wait for more fragments
        }
        let message = self.current.take().expect("complete message");
        self.complete(message)
    }

    /// Complete-message handling (PYTHON_CLI_SPEC §2.5).
    fn complete(&mut self, message: PartialMessage) -> Option<CoreEvent> {
        let body = message.payload;
        let request_id = message.request_id;
        let failed = message.flags & MGMT_FLAG_ERROR != 0;
        let final_ = message.flags & MGMT_FLAG_FINAL != 0;
        let kind = if message.message_type == 3 {
            MgmtKind::Event
        } else {
            MgmtKind::Response
        };

        let lossy = String::from_utf8_lossy(&body).into_owned();
        let text: String = lossy.trim_end_matches(['\r', '\n']).to_string();
        let lines = split_lines(&text);
        let command = self.commands.remove(&request_id);

        let event = CoreEvent::MgmtMessage {
            kind,
            id: request_id,
            ok: !failed,
            final_,
            lines: lines.clone(),
            command,
        };

        // Correlation: `type == 2` is an `if`, the FINAL branch is an
        // `elif`. A response that also carries FINAL resolves ONLY the
        // response waiter; a caller waiting for a type-3 FINAL event keeps
        // waiting (and still succeeds if such an event arrives later).
        if message.message_type == 2 {
            let events = self.events.remove(&request_id).unwrap_or_default();
            if let Some(tx) = self.pending.remove(&request_id) {
                let result = if failed {
                    Err(MgmtError::Device(if text.is_empty() {
                        "management request failed".to_string()
                    } else {
                        text
                    }))
                } else {
                    Ok(MgmtReply {
                        id: request_id,
                        lines,
                        events,
                        raw: lossy,
                        ok: !failed,
                    })
                };
                let _ = tx.send(result);
            }
        } else if final_ {
            let events = self.events.remove(&request_id).unwrap_or_default();
            if let Some(tx) = self.pending_final.remove(&request_id) {
                let result = if failed {
                    Err(MgmtError::Device(if text.is_empty() {
                        "management operation failed".to_string()
                    } else {
                        text
                    }))
                } else {
                    Ok(MgmtReply {
                        id: request_id,
                        lines,
                        events,
                        raw: lossy,
                        ok: !failed,
                    })
                };
                let _ = tx.send(result);
            }
        } else if self.pending.contains_key(&request_id)
            || self.pending_final.contains_key(&request_id)
        {
            // Intermediate event while somebody waits: keep its lines for the
            // eventual reply.
            self.events.entry(request_id).or_default().extend(lines);
        }
        Some(event)
    }

    /// Drop every pending request, failing them with `MgmtError::Disconnected`.
    pub fn fail_all(&mut self) {
        self.current = None;
        for (_, tx) in std::mem::take(&mut self.pending) {
            let _ = tx.send(Err(MgmtError::Disconnected));
        }
        for (_, tx) in std::mem::take(&mut self.pending_final) {
            let _ = tx.send(Err(MgmtError::Disconnected));
        }
        self.commands.clear();
        self.events.clear();
    }

    /// Forget a request after its caller is done (the Python `send()`
    /// `finally` block): safe to call for already-resolved ids.
    pub fn finish(&mut self, id: RequestId) {
        self.pending.remove(&id);
        self.pending_final.remove(&id);
        self.commands.remove(&id);
        self.events.remove(&id);
    }
}

impl Default for MgmtCore {
    fn default() -> Self {
        Self::new(512, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mgmt_frame(message_type: u8, request_id: u32, payload: &[u8], flags: u16) -> Vec<u8> {
        let mut frame = Vec::new();
        frame.extend_from_slice(&MGMT_MAGIC);
        frame.push(MGMT_API_MAJOR);
        frame.push(message_type);
        frame.extend_from_slice(&request_id.to_le_bytes());
        frame.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        frame.extend_from_slice(&flags.to_le_bytes());
        frame.extend_from_slice(payload);
        frame
    }

    fn message(event: Option<CoreEvent>) -> CoreEvent {
        event.expect("management message")
    }

    fn mgmt_message(
        event: CoreEvent,
    ) -> (MgmtKind, RequestId, bool, bool, Vec<String>, Option<String>) {
        match event {
            CoreEvent::MgmtMessage {
                kind,
                id,
                ok,
                final_,
                lines,
                command,
            } => (kind, id, ok, final_, lines, command),
            other => panic!("expected MgmtMessage, got {other:?}"),
        }
    }

    fn notice(event: Option<CoreEvent>) -> (NoticeLevel, String) {
        match event.expect("notice") {
            CoreEvent::Notice { level, text } => (level, text),
            other => panic!("expected Notice, got {other:?}"),
        }
    }

    // ---- ManagementChannelTests -----------------------------------------

    #[test]
    fn response_resolves_the_matching_future() {
        let mut core = MgmtCore::new(512, false);
        let (id, chunks, mut rx, final_rx) = core.begin("@i?", false).unwrap();
        assert_eq!(id, 1);
        assert!(final_rx.is_none());
        // One ATT chunk: 12-byte header + 3-byte command.
        assert_eq!(chunks.len(), 1);
        assert_eq!(&chunks[0][..2], &MGMT_MAGIC);
        assert_eq!(chunks[0][2], MGMT_API_MAJOR);
        assert_eq!(chunks[0][3], 1);
        assert_eq!(&chunks[0][4..8], &1u32.to_le_bytes());
        assert_eq!(&chunks[0][8..10], &3u16.to_le_bytes());
        assert_eq!(&chunks[0][10..12], &0u16.to_le_bytes());
        assert_eq!(&chunks[0][12..], b"@i?");

        let event = message(core.feed(&mgmt_frame(2, id, b"OK uptime=1\r\n", 0)));
        let (kind, id, ok, final_, lines, command) = mgmt_message(event);
        assert_eq!(kind, MgmtKind::Response);
        assert_eq!(id, 1);
        assert!(ok);
        assert!(!final_);
        assert_eq!(lines, vec!["OK uptime=1"]);
        assert_eq!(command.as_deref(), Some("@i?"));

        let reply = rx.try_recv().expect("resolved").expect("ok reply");
        assert_eq!(reply.id, 1);
        assert_eq!(reply.raw, "OK uptime=1\r\n");
        assert_eq!(reply.lines, vec!["OK uptime=1"]);
        assert!(reply.events.is_empty());
        assert!(reply.ok);
    }

    #[test]
    fn error_flag_raises() {
        let mut core = MgmtCore::new(512, false);
        let (id, _, mut rx, _) = core.begin("@u?", false).unwrap();
        let event = message(core.feed(&mgmt_frame(2, id, b"ERR format\r\n", MGMT_FLAG_ERROR)));
        let (_, _, ok, _, _, _) = mgmt_message(event);
        assert!(!ok);
        let error = rx.try_recv().expect("resolved").expect_err("device error");
        assert_eq!(error.to_string(), "ERR format");

        // An error whose text strips away falls back to the catalog message
        // (an empty payload would be an invalid header: expected == 0).
        let mut core = MgmtCore::new(512, false);
        let (id, _, mut rx, _) = core.begin("@u?", false).unwrap();
        core.feed(&mgmt_frame(2, id, b"\r\n", MGMT_FLAG_ERROR));
        let error = rx.try_recv().expect("resolved").expect_err("device error");
        assert_eq!(error.to_string(), "management request failed");
    }

    #[test]
    fn final_event_error_uses_the_operation_message() {
        let mut core = MgmtCore::new(512, false);
        let (id, _, _, final_rx) = core.begin("@w scan", true).unwrap();
        let mut final_rx = final_rx.expect("final waiter");
        core.feed(&mgmt_frame(
            3,
            id,
            b"\r\n",
            MGMT_FLAG_ERROR | MGMT_FLAG_FINAL,
        ));
        let error = final_rx.try_recv().expect("resolved").expect_err("error");
        assert_eq!(error.to_string(), "management operation failed");
    }

    #[test]
    fn fragmented_response_is_reassembled() {
        let mut core = MgmtCore::new(512, false);
        let (id, _, mut rx, _) = core.begin("@i?", false).unwrap();
        let frame = mgmt_frame(2, id, b"OK a=1\r\nb=2\r\n", 0);
        assert!(core.feed(&frame[..16]).is_none());
        assert!(
            rx.try_recv().is_err(),
            "still pending after a partial frame"
        );
        let event = message(core.feed(&frame[16..]));
        let (_, _, _, _, lines, _) = mgmt_message(event);
        assert_eq!(lines, vec!["OK a=1", "b=2"]);
        let reply = rx.try_recv().expect("resolved").expect("ok reply");
        assert_eq!(reply.raw, "OK a=1\r\nb=2\r\n");
    }

    #[test]
    fn orphaned_response_header_is_ignored() {
        let mut core = MgmtCore::new(512, false);
        let (level, text) = notice(core.feed(b"garbage"));
        assert_eq!(level, NoticeLevel::Warn);
        assert_eq!(text, "management <- orphaned response fragment");

        // A header-looking fragment shorter than 12 bytes is orphaned too.
        let (level, text) = notice(core.feed(&b"LK\x01"[..]));
        assert_eq!(level, NoticeLevel::Warn);
        assert_eq!(text, "management <- orphaned response fragment");

        // Bad magic in a full-size fragment.
        let mut frame = mgmt_frame(2, 1, b"x", 0);
        frame[0] = b'X';
        let (_, text) = notice(core.feed(&frame));
        assert_eq!(text, "management <- orphaned response fragment");
    }

    #[test]
    fn invalid_response_header_is_warned() {
        let mut core = MgmtCore::new(512, false);
        // Version 2.
        let mut frame = mgmt_frame(2, 1, b"x", 0);
        frame[2] = 2;
        let (level, text) = notice(core.feed(&frame));
        assert_eq!(level, NoticeLevel::Warn);
        assert_eq!(text, "management <- invalid response header");
        // message type 1 (host command) never comes back from the device.
        let frame = mgmt_frame(1, 1, b"x", 0);
        let (_, text) = notice(core.feed(&frame));
        assert_eq!(text, "management <- invalid response header");
        // expected == 0.
        let mut frame = mgmt_frame(2, 1, b"x", 0);
        frame[8] = 0;
        frame[9] = 0;
        let (_, text) = notice(core.feed(&frame));
        assert_eq!(text, "management <- invalid response header");
        // expected > max_payload.
        let mut core = MgmtCore::new(8, false);
        let frame = mgmt_frame(2, 1, b"123456789", 0);
        let (_, text) = notice(core.feed(&frame));
        assert_eq!(text, "management <- invalid response header");
    }

    #[test]
    fn oversized_response_is_dropped_whole() {
        let mut core = MgmtCore::new(512, false);
        // Header claims 5 bytes, then 6 bytes arrive.
        let mut frame = mgmt_frame(2, 1, b"12345", 0);
        frame.push(b'6');
        let (level, text) = notice(core.feed(&frame));
        assert_eq!(level, NoticeLevel::Warn);
        assert_eq!(text, "management <- oversized response");
        // State is idle again: the next header parses normally.
        let event = message(core.feed(&mgmt_frame(2, 1, b"OK\r\n", 0)));
        let (_, id, _, _, _, _) = mgmt_message(event);
        assert_eq!(id, 1);
    }

    #[test]
    fn response_with_final_resolves_only_the_response_waiter() {
        // PYTHON_CLI_SPEC §2.5: `if type == 2 / elif FINAL`. A type-2
        // message carrying FINAL must NOT settle the wait_final future.
        let mut core = MgmtCore::new(512, false);
        let (id, _, mut rx, final_rx) = core.begin("@w scan", true).unwrap();
        let mut final_rx = final_rx.expect("final waiter");

        core.feed(&mgmt_frame(2, id, b"OK scan=0\r\n", MGMT_FLAG_FINAL));
        let reply = rx.try_recv().expect("response resolved").expect("ok");
        assert_eq!(reply.lines, vec!["OK scan=0"]);
        assert!(
            final_rx.try_recv().is_err(),
            "a type-2 FINAL must not resolve pending_final"
        );

        // The real type-3 FINAL event still settles it afterwards.
        let event = message(core.feed(&mgmt_frame(
            3,
            id,
            b"OK event scan done\r\n",
            MGMT_FLAG_FINAL,
        )));
        let (kind, _, _, final_, _, command) = mgmt_message(event);
        assert_eq!(kind, MgmtKind::Event);
        assert!(final_);
        assert_eq!(command, None, "command text is popped by the first message");
        let reply = final_rx.try_recv().expect("final resolved").expect("ok");
        assert_eq!(reply.lines, vec!["OK event scan done"]);
    }

    #[test]
    fn intermediate_events_are_collected_for_the_final_reply() {
        let mut core = MgmtCore::new(512, false);
        let (id, _, mut rx, final_rx) = core.begin("@w=ssid,secret", true).unwrap();
        let mut final_rx = final_rx.expect("final waiter");

        core.feed(&mgmt_frame(2, id, b"OK wifi=accepted\r\n", 0));
        let reply = rx.try_recv().expect("resolved").expect("ok");
        assert_eq!(reply.lines, vec!["OK wifi=accepted"]);

        core.feed(&mgmt_frame(3, id, b"OK event wifi result=0\r\n", 0));
        core.feed(&mgmt_frame(
            3,
            id,
            b"OK event wifi final\r\n",
            MGMT_FLAG_FINAL,
        ));
        let reply = final_rx.try_recv().expect("resolved").expect("ok");
        assert_eq!(reply.lines, vec!["OK event wifi final"]);
        assert_eq!(reply.events, vec!["OK event wifi result=0"]);
    }

    #[test]
    fn event_without_final_resolves_nothing() {
        let mut core = MgmtCore::new(512, false);
        let (id, _, mut rx, _) = core.begin("@i?", false).unwrap();
        let event = message(core.feed(&mgmt_frame(3, id, b"note\r\n", 0)));
        let (kind, _, _, final_, lines, _) = mgmt_message(event);
        assert_eq!(kind, MgmtKind::Event);
        assert!(!final_);
        assert_eq!(lines, vec!["note"]);
        assert!(rx.try_recv().is_err(), "events do not resolve the request");

        // The response for the same id still arrives afterwards.
        core.feed(&mgmt_frame(2, id, b"OK\r\n", 0));
        assert!(rx.try_recv().expect("resolved").is_ok());
    }

    #[test]
    fn json_output_redacts_secrets_and_shapes_records() {
        let mut core = MgmtCore::new(512, true);
        assert!(core.json());

        let (id, _, mut rx, _) = core.begin("@w=ssid,secret", false).unwrap();
        let event = message(core.feed(&mgmt_frame(2, id, b"OK wifi=accepted\r\n", 0)));
        let (kind, id, ok, _, lines, command) = mgmt_message(event);
        assert_eq!(command.as_deref(), Some("@w=<redacted>"));
        rx.try_recv().expect("resolved").expect("ok");

        let record = json_record(kind, id, ok, &lines, command.as_deref());
        assert_eq!(
            record,
            r#"{"type": "response", "requestId": 1, "ok": true, "lines": ["OK wifi=accepted"], "command": "@w=<redacted>"}"#
        );
        assert!(!record.contains("secret"));

        // An event record for an unknown id omits `command`.
        let event = message(core.feed(&mgmt_frame(
            3,
            9,
            b"OK event wifi result=0\r\n",
            MGMT_FLAG_FINAL,
        )));
        let (kind, id, ok, _, lines, command) = mgmt_message(event);
        assert_eq!(command, None);
        assert_eq!(
            json_record(kind, id, ok, &lines, command.as_deref()),
            r#"{"type": "event", "requestId": 9, "ok": true, "lines": ["OK event wifi result=0"]}"#
        );
    }

    #[test]
    fn redaction_is_prefix_based() {
        assert_eq!(display_command("@w=ssid,secret"), "@w=<redacted>");
        assert_eq!(display_command("@d=https://user:pass@x/"), "@d=<redacted>");
        assert_eq!(display_command("@w off"), "@w off");
        assert_eq!(display_command("@w scan"), "@w scan");
        assert_eq!(display_command("@d off"), "@d off");
        assert_eq!(display_command("@i?"), "@i?");
        assert!(!display_command("@w scan").contains("secret"));
    }

    #[test]
    fn oversized_command_is_rejected_before_any_write() {
        let mut core = MgmtCore::new(8, false);
        let error = core.begin("@i?xxxxxxxx", false).expect_err("too long");
        assert!(matches!(error, MgmtError::OutsideLimit));
        assert_eq!(
            error.to_string(),
            "management command is outside the advertised limit"
        );
        let error = core.begin("", false).expect_err("empty");
        assert!(matches!(error, MgmtError::OutsideLimit));
        // Exactly the limit is fine.
        assert!(core.begin("@i?xxx", false).is_ok());
    }

    #[test]
    fn new_sized_sizes_the_channel_to_the_negotiated_payload() {
        // `session.rs` builds the channel from `handshake.protocol.max_payload`
        // with `--json` off: same rules as `new`, one argument.
        let mut core = MgmtCore::new_sized(64);
        assert!(!core.json());
        assert!(core.begin("", false).is_err());
        assert!(core.begin(&"x".repeat(65), false).is_err());
        let (id, chunks, _, _) = core.begin(&"x".repeat(64), false).expect("at the limit");
        assert_eq!(id, 1);
        // 12-byte header + 64-byte body = 76 bytes at the default 20-byte chunk.
        assert_eq!(
            chunks.iter().map(Vec::len).collect::<Vec<_>>(),
            vec![20, 20, 20, 16]
        );
    }

    #[test]
    fn requests_are_chunked_with_max_20_write_size() {
        let mut core = MgmtCore::new(512, false);
        // A manual write size below 20 still writes 20-byte chunks.
        core.set_write_size(5);
        let (id, chunks, _, _) = core.begin(&"x".repeat(30), false).unwrap();
        assert_eq!(id, 1);
        assert_eq!(
            chunks.iter().map(Vec::len).collect::<Vec<_>>(),
            vec![20, 20, 2]
        );
        assert_eq!(&chunks[0][..2], &MGMT_MAGIC);
        let joined: Vec<u8> = chunks.iter().flat_map(|chunk| chunk.clone()).collect();
        assert_eq!(joined.len(), MGMT_HEADER_SIZE + 30);
        assert_eq!(&joined[..2], &MGMT_MAGIC);
        assert_eq!(&joined[MGMT_HEADER_SIZE..], b"x".repeat(30).as_slice());

        // A large write size is not clamped to 244 on the management path.
        let mut core = MgmtCore::new(512, false);
        core.set_write_size(600);
        let (_, chunks, _, _) = core.begin(&"x".repeat(300), false).unwrap();
        // header + 300 = 312 bytes fit into a single 600-byte chunk.
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].len(), MGMT_HEADER_SIZE + 300);
    }

    #[test]
    fn request_ids_wrap_from_max_to_one() {
        let mut core = MgmtCore::new(512, false);
        core.next_request_id = u32::MAX;
        let (id, _, _, _) = core.begin("@i?", false).unwrap();
        assert_eq!(id, u32::MAX);
        let (id, _, _, _) = core.begin("@i?", false).unwrap();
        assert_eq!(id, 1);
        let (id, _, _, _) = core.begin("@i?", false).unwrap();
        assert_eq!(id, 2);
    }

    #[test]
    fn fail_all_disconnects_every_waiter() {
        let mut core = MgmtCore::new(512, false);
        let (id, _, mut rx, final_rx) = core.begin("@w scan", true).unwrap();
        let mut final_rx = final_rx.expect("final waiter");
        core.fail_all();
        let error = rx.try_recv().expect("resolved").expect_err("disconnected");
        assert!(matches!(error, MgmtError::Disconnected));
        assert_eq!(error.to_string(), "disconnected before management response");
        let error = final_rx
            .try_recv()
            .expect("resolved")
            .expect_err("disconnected");
        assert!(matches!(error, MgmtError::Disconnected));
        // State was reset: no stale current message, no stale command text.
        assert!(core.commands.is_empty());
        assert!(core.events.is_empty());
        let _ = id;
    }

    #[test]
    fn finish_forgets_request_state() {
        let mut core = MgmtCore::new(512, false);
        let (id, _, _, _) = core.begin("@i?", false).unwrap();
        assert_eq!(core.commands.len(), 1);
        core.finish(id);
        assert!(core.commands.is_empty());
        assert!(core.pending.is_empty());
        // Ids that never existed are fine to finish too.
        core.finish(4242);
    }

    #[test]
    fn lines_split_like_python_splitlines() {
        assert_eq!(split_lines(""), vec![""]);
        assert_eq!(split_lines("one"), vec!["one"]);
        assert_eq!(split_lines("a\nb"), vec!["a", "b"]);
        assert_eq!(split_lines("a\r\nb"), vec!["a", "b"]);
        assert_eq!(split_lines("a\rb"), vec!["a", "b"]);
        assert_eq!(split_lines("a\n\n"), vec!["a", ""]);
        assert_eq!(split_lines("\n"), vec![""]);
        assert_eq!(split_lines("a\n"), vec!["a"]);
    }

    #[test]
    fn traces_and_plain_lines_carry_the_catalog_text() {
        assert_eq!(tx_trace(1, b"@i?", false), "MGMT TX #1 b'@i?'");
        assert_eq!(
            tx_trace(7, &[0u8; 20], true),
            "MGMT TX #7 <redacted 20 bytes>"
        );
        assert_eq!(
            plain_line(MgmtKind::Response, 1, "fw=1.2.3"),
            "response #1 <- fw=1.2.3"
        );
        assert_eq!(
            plain_line(MgmtKind::Event, 4, "ERR wifi"),
            "event #4 <- ERR wifi"
        );
    }
}
