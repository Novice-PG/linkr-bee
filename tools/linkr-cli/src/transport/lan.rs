//! LAN WebSocket bridge transport: `ws://host/ws`, token handshake and raw
//! binary UART frames. See docs/LINKR_BLE_API.zh-CN.md section 8.

use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

use super::{EventHub, Transport, TransportChannel, TransportEvent, TransportKind};

/// Web client `WS_CONNECT_TIMEOUT_MS`.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Web client `WS_HANDSHAKE_TIMEOUT_MS`.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

const AUTH_NONE: &str = "@ws auth=none";
const AUTH_REQUIRED: &str = "@ws auth=required";
const AUTH_OK: &str = "@ws auth=ok";

/// Web client's token validation message (WEB_UX_SPEC section 3.4).
pub const TOKEN_ERROR: &str = "The access token must be 32 lowercase hex characters.";
/// Web client's empty-host message.
pub const EMPTY_HOST_ERROR: &str = "Enter the device address first.";
/// Web client's missing-token message.
pub const TOKEN_REQUIRED: &str = "This bridge requires an access token. Read it with @s? over BLE.";
/// Web client's rejected-token message (bridge closed before `auth=ok`).
pub const TOKEN_REJECTED: &str =
    "LAN access token rejected. Read the current token with @s? over BLE.";
/// Web client's handshake timeout message.
pub const HANDSHAKE_TIMEOUT_MSG: &str =
    "The bridge did not confirm LAN access; check the token or firmware version.";
/// Web client's ignored pre-handshake frame message (logged as a warning).
pub const IGNORED_FRAME: &str = "ignored LAN frame before the access handshake";

/// `true` when `token` matches `^[0-9a-f]{32}$`.
pub fn validate_token(token: &str) -> Result<(), String> {
    if token.len() == 32
        && token
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        Ok(())
    } else {
        Err(TOKEN_ERROR.to_string())
    }
}

/// Normalize a host argument into a `ws://…/ws` URL: anything already shaped
/// like `ws://`/`wss://` is used verbatim, everything else gets the scheme and
/// the `/ws` path appended (web `connectWs` step 2).
pub fn normalize_url(host: &str) -> String {
    if host.starts_with("ws://") || host.starts_with("wss://") {
        host.to_string()
    } else {
        format!("ws://{host}/ws")
    }
}

fn unreachable(url: &str) -> String {
    format!("WebSocket {url} is unreachable.")
}

/// What the client must do after one text frame of the access handshake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandshakeAction {
    /// Keep listening.
    Continue,
    /// Send this text frame (the access token).
    Send(String),
    /// The bridge accepted us; serial traffic may flow.
    Done,
    /// A foreign text frame arrived before the handshake finished: warn.
    Ignore,
}

/// Text-frame half of the `@ws auth=` handshake from
/// docs/LINKR_BLE_API.zh-CN.md section 8. Kept free of I/O so it can be
/// driven directly by tests.
pub struct Handshake {
    token: Option<String>,
    token_sent: bool,
}

impl Handshake {
    /// `Err` when a token was supplied but is not `^[0-9a-f]{32}$`.
    pub fn new(token: Option<&str>) -> Result<Self, String> {
        if let Some(token) = token {
            validate_token(token)?;
        }
        Ok(Self {
            token: token.map(str::to_string),
            token_sent: false,
        })
    }

    /// Whether the token frame has already been sent (decides which message a
    /// close before `auth=ok` produces, web step 7).
    pub fn token_sent(&self) -> bool {
        self.token_sent
    }

    /// Feed one text frame received before the handshake completed.
    pub fn on_text(&mut self, raw: &str) -> Result<HandshakeAction, String> {
        let text = raw.trim_end_matches(['\r', '\n']);
        match text {
            AUTH_NONE | AUTH_OK => Ok(HandshakeAction::Done),
            AUTH_REQUIRED => match self.token.clone() {
                Some(token) => {
                    self.token_sent = true;
                    Ok(HandshakeAction::Send(token))
                }
                None => Err(TOKEN_REQUIRED.to_string()),
            },
            _ => Ok(HandshakeAction::Ignore),
        }
    }
}

enum LanCommand {
    Uart(Vec<u8>),
    Disconnect,
}

/// Largest single WS frame handed to the bridge. Also the burst the rate
/// limiter may spend at once, so it must stay under what the bridge queue can
/// absorb while empty.
const LAN_FRAME_MAX_BYTES: usize = 1024;
/// Bytes per second handed over: 8 KiB/s is 70% of the default line rate
/// (`LINKR_BLE_BRIDGE_UART_BAUD_RATE` 115200 → ≈11.5 KiB/s), so the queue
/// never grows faster than it drains.
const LAN_WRITE_BYTES_PER_SEC: f64 = 8.0 * 1024.0;

/// Token bucket for the LAN uplink (`dist/BACKLOG.md` G2).
///
/// The bridge queues inbound UART bytes in `ble_to_uart_queue` — 8 slots of
/// 244 B (`Kconfig` `LINKR_BLE_BRIDGE_BLE_TO_UART_QUEUE_DEPTH`) — and **drops
/// whole chunks** when it is full (`src/ws_bridge.c`: *UART queue full;
/// dropping*), while the queue only drains at UART line rate. A single
/// un-paced burst therefore loses everything above ~1.4 KiB: pasted 800 B came
/// back whole, 1600 B lost 136 B, 3000 B lost 1528 B (`dist/paste_integrity.py`
/// on real hardware). Pacing keeps the queue empty instead.
#[derive(Debug)]
struct Pace {
    state: tokio::sync::Mutex<PaceState>,
}

#[derive(Debug)]
struct PaceState {
    tokens: f64,
    last: tokio::time::Instant,
}

impl Pace {
    fn new() -> Self {
        Self {
            state: tokio::sync::Mutex::new(PaceState {
                tokens: LAN_FRAME_MAX_BYTES as f64,
                last: tokio::time::Instant::now(),
            }),
        }
    }

    /// Wait until `bytes` may go on the wire. A keystroke finds tokens waiting
    /// and costs nothing; only sustained bulk input feels the limit.
    async fn take(&self, bytes: usize) {
        if bytes == 0 {
            return;
        }
        let mut state = self.state.lock().await;
        let now = tokio::time::Instant::now();
        let elapsed = now.saturating_duration_since(state.last).as_secs_f64();
        state.last = now;
        state.tokens =
            (state.tokens + elapsed * LAN_WRITE_BYTES_PER_SEC).min(LAN_FRAME_MAX_BYTES as f64);

        let need = bytes as f64;
        if state.tokens >= need {
            state.tokens -= need;
            return;
        }
        // The wait *is* the payment for these bytes, so the clock restarts
        // empty once it ends: crediting the slept time again would hand out
        // the same bytes twice (measured: 3000 B in 125 ms instead of 241).
        // The lock is held across the wait, which also keeps concurrent
        // writers in call order instead of interleaving frames.
        let wait = (need - state.tokens) / LAN_WRITE_BYTES_PER_SEC;
        state.tokens = 0.0;
        tokio::time::sleep(Duration::from_secs_f64(wait)).await;
        state.last = tokio::time::Instant::now();
    }
}

/// A connected LAN bridge: UART bytes travel as binary frames, management
/// commands are rejected (the bridge has no Management Service).
pub struct LanTransport {
    commands: mpsc::UnboundedSender<LanCommand>,
    hub: Arc<EventHub>,
    pace: Pace,
}

#[async_trait::async_trait]
impl Transport for LanTransport {
    fn kind(&self) -> TransportKind {
        TransportKind::Lan
    }

    async fn write_mgmt(&self, _chunk: &[u8]) -> anyhow::Result<()> {
        Err(anyhow::anyhow!(
            "management commands are not available over the LAN bridge; connect over BLE to run them"
        ))
    }

    async fn write_uart(&self, chunk: &[u8]) -> anyhow::Result<()> {
        // G2: split to bridge-sized frames and spend the rate limiter on each,
        // so a bulk paste can never outrun the device's UART queue.
        for piece in chunk.chunks(LAN_FRAME_MAX_BYTES) {
            self.pace.take(piece.len()).await;
            self.commands
                .send(LanCommand::Uart(piece.to_vec()))
                .map_err(|_| anyhow::anyhow!("LAN bridge closed"))?;
        }
        Ok(())
    }

    async fn disconnect(&self) -> anyhow::Result<()> {
        let _ = self.commands.send(LanCommand::Disconnect);
        Ok(())
    }

    fn write_size(&self) -> usize {
        // One UART write is one binary frame; there is no ATT chunking.
        0
    }

    fn events(&self) -> tokio::sync::broadcast::Receiver<TransportEvent> {
        self.hub.subscribe()
    }
}

/// Connect to the bridge with the default web-client timeouts (15 s connect,
/// 5 s handshake).
pub async fn connect(host: &str, token: Option<&str>) -> anyhow::Result<Arc<dyn Transport>> {
    connect_with_timeouts(host, token, CONNECT_TIMEOUT, HANDSHAKE_TIMEOUT).await
}

/// Connect with explicit timeouts (tests use short ones).
///
/// The bridge rejects a fresh connection while it is still reaping the client
/// that just left (`WS_AUTH_TIMEOUT_MS`, `src/ws_bridge.c`): it stops calling
/// `accept`, the queue fills and the kernel answers new dials with **RST**, so
/// the caller sees `… is unreachable.` even though the host is fine — measured
/// on hardware at 5/8 refused dials and 11/20 failed back-to-back connects
/// (`dist/BACKLOG.md` G1). Both shapes that follow from it are retried: the
/// refused connect, and an upgrade that dies. An unroutable address, an HTTP
/// status, a rejected token and the handshake timeout keep failing fast with
/// the exact same message as before.
pub async fn connect_with_timeouts(
    host: &str,
    token: Option<&str>,
    connect_timeout: Duration,
    handshake_timeout: Duration,
) -> anyhow::Result<Arc<dyn Transport>> {
    if host.trim().is_empty() {
        return Err(anyhow::anyhow!(EMPTY_HOST_ERROR));
    }
    // A malformed token is a usage error; check it once, before the network.
    Handshake::new(token).map_err(anyhow::Error::msg)?;

    let mut attempt = 0usize;
    loop {
        match dial_once(host, token, connect_timeout, handshake_timeout).await {
            Ok(transport) => return Ok(transport),
            Err(DialError::Fatal(err)) => return Err(err),
            Err(DialError::Transient(err)) => match RETRY_DELAYS.get(attempt) {
                Some(delay) => {
                    crate::cli::warn(format!(
                        "LAN bridge is not accepting yet; retrying in {} ms (attempt {})",
                        delay.as_millis(),
                        attempt + 2
                    ));
                    tokio::time::sleep(*delay).await;
                    attempt += 1;
                }
                None => return Err(err),
            },
        }
    }
}

/// A dial failure, split by whether another attempt could succeed.
enum DialError {
    /// The bridge was not ready for us: it refused the connect (it stopped
    /// calling `accept` while reaping the previous client) or took the TCP
    /// connection and hung up before the access handshake finished. Either
    /// way its slots are still busy and a later attempt can succeed.
    Transient(anyhow::Error),
    /// Nothing a retry would change (no route, bad token, timeout).
    Fatal(anyhow::Error),
}

/// Backoff between [`DialError::Transient`] attempts: three steps cover the
/// bridge's 3 s auth window (`WS_AUTH_TIMEOUT_MS`) with room to spare.
const RETRY_DELAYS: [Duration; 3] = [
    Duration::from_millis(700),
    Duration::from_millis(1600),
    Duration::from_millis(3000),
];

/// The bridge hung up mid-handshake: fatal once our token went out (it has
/// answered, and the answer was no), transient before that.
fn hangup(url: &str, handshake: &Handshake) -> DialError {
    let message = anyhow::anyhow!(close_message(url, handshake.token_sent()));
    if handshake.token_sent() {
        DialError::Fatal(message)
    } else {
        DialError::Transient(message)
    }
}

/// Whether a failed dial is worth attempting again.
///
/// The bridge shows both shapes while it is still reaping the client that just
/// left — 5 refused and 3 mid-handshake out of 8 back-to-back dials on the same
/// run (`dist/BACKLOG.md` G1): it stops calling `accept`, so the kernel RSTs a
/// fresh connect (`ECONNREFUSED`), and a connect that did get in is closed
/// without a status before `@ws auth=` (`linkr_ws_setup` → `-ENOENT`). Both are
/// transient. An unroutable address, a bad local address, a handshake timeout,
/// an HTTP status and a rejected token are not: a retry cannot change them, and
/// a host that is really gone answers with a route error rather than a refusal.
fn handshake_error_is_transient(err: &tokio_tungstenite::tungstenite::Error) -> bool {
    use std::io::ErrorKind as IoKind;
    use tokio_tungstenite::tungstenite::Error as WsError;

    match err {
        WsError::Protocol(_) | WsError::AlreadyClosed => true,
        WsError::Io(io) => !matches!(
            io.kind(),
            IoKind::AddrNotAvailable
                | IoKind::AddrInUse
                | IoKind::NotConnected
                | IoKind::TimedOut
                | IoKind::InvalidInput
                | IoKind::NetworkUnreachable
                | IoKind::HostUnreachable
        ),
        _ => false,
    }
}

/// One dial attempt. Kept separate from [`connect_with_timeouts`] so the retry
/// loop stays a loop and this function reads exactly like it used to.
async fn dial_once(
    host: &str,
    token: Option<&str>,
    connect_timeout: Duration,
    handshake_timeout: Duration,
) -> Result<Arc<dyn Transport>, DialError> {
    let mut handshake =
        Handshake::new(token).map_err(|err| DialError::Fatal(anyhow::anyhow!(err)))?;
    let url = normalize_url(host);

    let connected =
        tokio::time::timeout(connect_timeout, tokio_tungstenite::connect_async(&url)).await;
    let (stream, _) = match connected {
        Err(_) => {
            return Err(DialError::Fatal(anyhow::anyhow!(
                "WebSocket connection timed out after {} seconds.",
                connect_timeout.as_secs()
            )))
        }
        Ok(Err(err)) => {
            let message = anyhow::anyhow!(unreachable(&url));
            return Err(if handshake_error_is_transient(&err) {
                DialError::Transient(message)
            } else {
                DialError::Fatal(message)
            });
        }
        Ok(Ok(pair)) => pair,
    };

    // --- access handshake (web connectWs steps 4-5) -----------------------
    let mut stream = stream;
    let mut buffered: Vec<Vec<u8>> = Vec::new();
    let deadline = tokio::time::Instant::now() + handshake_timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(DialError::Fatal(anyhow::anyhow!(HANDSHAKE_TIMEOUT_MSG)));
        }
        let next = tokio::time::timeout(remaining, stream.next()).await;
        let frame = match next {
            Err(_) => return Err(DialError::Fatal(anyhow::anyhow!(HANDSHAKE_TIMEOUT_MSG))),
            // Gone before our token went out: retryable (G1).
            Ok(None) | Ok(Some(Err(_))) => return Err(hangup(&url, &handshake)),
            Ok(Some(Ok(frame))) => frame,
        };
        match frame {
            Message::Text(text) => match handshake
                .on_text(text.as_str())
                .map_err(|err| DialError::Fatal(anyhow::anyhow!(err)))?
            {
                HandshakeAction::Done => break,
                HandshakeAction::Send(token) => {
                    stream
                        .send(Message::text(token))
                        .await
                        .map_err(|_| hangup(&url, &handshake))?;
                }
                HandshakeAction::Ignore => crate::cli::warn(IGNORED_FRAME),
                HandshakeAction::Continue => {}
            },
            Message::Binary(bytes) => buffered.push(bytes.to_vec()),
            Message::Close(_) => return Err(hangup(&url, &handshake)),
            // Ping/Pong before the handshake carries nothing useful yet.
            _ => {}
        }
    }

    // --- serial pump -------------------------------------------------------
    let hub = Arc::new(EventHub::new());
    let (commands_tx, commands_rx) = mpsc::unbounded_channel();
    let (sink, source) = stream.split();
    tokio::spawn(pump(
        sink,
        source,
        commands_rx,
        hub.clone(),
        buffered,
        url.clone(),
    ));
    Ok(Arc::new(LanTransport {
        commands: commands_tx,
        hub,
        pace: Pace::new(),
    }))
}

fn close_message(url: &str, token_sent: bool) -> String {
    if token_sent {
        TOKEN_REJECTED.to_string()
    } else {
        unreachable(url)
    }
}

async fn pump(
    mut sink: futures::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        Message,
    >,
    mut source: futures::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    >,
    mut commands: mpsc::UnboundedReceiver<LanCommand>,
    hub: Arc<EventHub>,
    mut buffered: Vec<Vec<u8>>,
    url: String,
) {
    hub.publish(TransportEvent::Subscribed);
    for bytes in buffered.drain(..) {
        hub.publish(TransportEvent::Data {
            channel: TransportChannel::UartTx,
            bytes,
        });
    }
    loop {
        tokio::select! {
            command = commands.recv() => match command {
                None | Some(LanCommand::Disconnect) => {
                    let _ = sink.close().await;
                    hub.publish(TransportEvent::Disconnected {
                        reason: "LAN bridge disconnected".to_string(),
                    });
                    return;
                }
                Some(LanCommand::Uart(bytes)) => {
                    if sink.send(Message::binary(bytes)).await.is_err() {
                        hub.publish(TransportEvent::Disconnected {
                            reason: "LAN bridge closed the connection".to_string(),
                        });
                        return;
                    }
                }
            },
            frame = source.next() => match frame {
                None | Some(Err(_)) => {
                    hub.publish(TransportEvent::Disconnected {
                        reason: format!("LAN bridge closed {url}"),
                    });
                    return;
                }
                Some(Ok(Message::Binary(bytes))) => {
                    hub.publish(TransportEvent::Data {
                        channel: TransportChannel::UartTx,
                        bytes: bytes.to_vec(),
                    });
                }
                Some(Ok(Message::Text(text))) => {
                    hub.publish(TransportEvent::Text(text.to_string()));
                }
                Some(Ok(_)) => {}
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_url_adds_scheme_and_path() {
        assert_eq!(normalize_url("192.168.1.23"), "ws://192.168.1.23/ws");
        assert_eq!(
            normalize_url("192.168.1.23:8080"),
            "ws://192.168.1.23:8080/ws"
        );
        assert_eq!(normalize_url("bee.local/ws"), "ws://bee.local/ws/ws");
    }

    #[test]
    fn normalize_url_keeps_explicit_schemes_verbatim() {
        assert_eq!(
            normalize_url("ws://192.168.1.23/ws"),
            "ws://192.168.1.23/ws"
        );
        assert_eq!(
            normalize_url("wss://bee.example/ws"),
            "wss://bee.example/ws"
        );
        assert_eq!(normalize_url("ws://host"), "ws://host");
    }

    #[test]
    fn token_validation_matches_the_web_rule() {
        assert!(validate_token(&"a".repeat(32)).is_ok());
        assert!(validate_token("0123456789abcdef0123456789abcdef").is_ok());
        assert_eq!(validate_token(""), Err(TOKEN_ERROR.to_string()));
        assert_eq!(
            validate_token(&"A".repeat(32)),
            Err(TOKEN_ERROR.to_string())
        );
        assert_eq!(
            validate_token("0123456789abcdef0123456789abcde"),
            Err(TOKEN_ERROR.to_string())
        );
        assert_eq!(
            validate_token("0123456789abcdef0123456789abcdef0"),
            Err(TOKEN_ERROR.to_string())
        );
        assert_eq!(
            validate_token("0123456789abcdef0123456789abcdeg"),
            Err(TOKEN_ERROR.to_string())
        );
    }

    #[test]
    fn handshake_without_token_completes_on_auth_none() {
        let mut hs = Handshake::new(None).unwrap();
        assert_eq!(
            hs.on_text("@ws auth=none\r\n").unwrap(),
            HandshakeAction::Done
        );
        assert!(!hs.token_sent());
    }

    #[test]
    fn handshake_sends_token_when_required() {
        let token = "0123456789abcdef0123456789abcdef";
        let mut hs = Handshake::new(Some(token)).unwrap();
        assert_eq!(
            hs.on_text("@ws auth=required\r\n").unwrap(),
            HandshakeAction::Send(token.to_string())
        );
        assert!(hs.token_sent());
        assert_eq!(
            hs.on_text("@ws auth=ok\r\n").unwrap(),
            HandshakeAction::Done
        );
    }

    #[test]
    fn handshake_without_token_reports_the_web_message() {
        let mut hs = Handshake::new(None).unwrap();
        assert_eq!(
            hs.on_text("@ws auth=required\r\n"),
            Err(TOKEN_REQUIRED.to_string())
        );
        assert!(!hs.token_sent());
    }

    #[test]
    fn handshake_warns_on_foreign_frames_and_accepts_auth_ok_alone() {
        let mut hs = Handshake::new(None).unwrap();
        assert_eq!(hs.on_text("hello").unwrap(), HandshakeAction::Ignore);
        assert_eq!(hs.on_text("@ws auth=ok").unwrap(), HandshakeAction::Done);
    }

    #[test]
    fn handshake_rejects_bad_token_format_up_front() {
        let error = Handshake::new(Some("xyz")).err().expect("must reject");
        assert_eq!(error, TOKEN_ERROR);
    }

    // ---------------------------------------------------------------------
    // in-process bridge server driving `connect`
    // ---------------------------------------------------------------------

    use tokio::net::{TcpListener, TcpStream};
    use tokio_tungstenite::tungstenite::Message as WSMessage;

    async fn serve_once(listener: TcpListener) -> tokio_tungstenite::WebSocketStream<TcpStream> {
        let (stream, _) = listener.accept().await.unwrap();
        tokio_tungstenite::accept_async(stream).await.unwrap()
    }

    fn free_addr() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        addr.to_string()
    }

    #[tokio::test]
    async fn connect_completes_when_no_token_is_required() {
        let addr = free_addr();
        let listener = TcpListener::bind(&addr).await.unwrap();
        let server = tokio::spawn(async move {
            let mut ws = serve_once(listener).await;
            ws.send(WSMessage::text("@ws auth=none\r\n")).await.unwrap();
            // Echo one serial frame so the pump is exercised.
            let frame = ws.next().await.unwrap().unwrap();
            if let WSMessage::Binary(bytes) = frame {
                ws.send(WSMessage::binary(bytes)).await.unwrap();
            }
        });

        let transport = connect(&addr, None).await.unwrap();
        assert_eq!(transport.kind(), TransportKind::Lan);
        assert_eq!(transport.write_size(), 0);
        let mut events = transport.events();
        transport.write_uart(b"hello").await.unwrap();
        let got = tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .unwrap()
            .unwrap();
        match got {
            TransportEvent::Subscribed => {
                let got = tokio::time::timeout(Duration::from_secs(5), events.recv())
                    .await
                    .unwrap()
                    .unwrap();
                assert!(matches!(
                    got,
                    TransportEvent::Data {
                        channel: TransportChannel::UartTx,
                        bytes
                    } if bytes == b"hello"
                ));
            }
            TransportEvent::Data {
                channel: TransportChannel::UartTx,
                bytes,
            } => assert_eq!(bytes, b"hello"),
            other => panic!("unexpected event: {other:?}"),
        }
        server.await.unwrap();
    }

    #[tokio::test]
    async fn connect_sends_the_token_when_the_bridge_requires_it() {
        let addr = free_addr();
        let token = "0123456789abcdef0123456789abcdef";
        let listener = TcpListener::bind(&addr).await.unwrap();
        let server = tokio::spawn(async move {
            let mut ws = serve_once(listener).await;
            ws.send(WSMessage::text("@ws auth=required\r\n"))
                .await
                .unwrap();
            let frame = ws.next().await.unwrap().unwrap();
            assert_eq!(frame.into_text().unwrap().as_str(), token);
            ws.send(WSMessage::text("@ws auth=ok\r\n")).await.unwrap();
        });

        let transport = connect(&addr, Some(token)).await.unwrap();
        assert_eq!(transport.kind(), TransportKind::Lan);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn connect_reports_a_rejected_token_when_the_bridge_closes() {
        let addr = free_addr();
        let token = "0123456789abcdef0123456789abcdef";
        let listener = TcpListener::bind(&addr).await.unwrap();
        let server = tokio::spawn(async move {
            let mut ws = serve_once(listener).await;
            ws.send(WSMessage::text("@ws auth=required\r\n"))
                .await
                .unwrap();
            let _frame = ws.next().await.unwrap().unwrap();
            // Wrong token: the bridge drops the connection without a frame.
            ws.close(None).await.unwrap();
        });

        let started = tokio::time::Instant::now();
        let err = connect(&addr, Some(token))
            .await
            .err()
            .expect("a wrong token must fail the handshake");
        assert_eq!(err.to_string(), TOKEN_REJECTED);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "a rejected token is a final answer, not a busy bridge: it must not be retried (took {:?})",
            started.elapsed()
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn connect_times_out_when_the_bridge_stays_silent() {
        let addr = free_addr();
        let listener = TcpListener::bind(&addr).await.unwrap();
        let _server = tokio::spawn(async move {
            let _ws = serve_once(listener).await;
            tokio::time::sleep(Duration::from_secs(10)).await;
        });

        let err = connect_with_timeouts(
            &addr,
            None,
            Duration::from_secs(5),
            Duration::from_millis(150),
        )
        .await
        .err()
        .expect("a silent bridge must time out");
        assert_eq!(err.to_string(), HANDSHAKE_TIMEOUT_MSG);
    }

    #[tokio::test]
    async fn a_refused_dial_is_retried_until_the_bridge_starts_listening() {
        // The bridge stops calling `accept` while it is still reaping the
        // client that just left, so the kernel answers a fresh dial with RST —
        // 5 of 8 back-to-back dials on hardware (`dist/BACKLOG.md` G1).
        // Refusal therefore means "busy", not "gone": a host that is gone
        // answers with a route error, which stays fatal.
        let addr = free_addr();
        let started = tokio::time::Instant::now();
        let dial = tokio::spawn({
            let addr = addr.clone();
            async move { connect(&addr, None).await }
        });

        // Attempt one lands on an empty port and backs off; the bridge comes up
        // inside that window, so only a retry can possibly succeed.
        tokio::time::sleep(Duration::from_millis(500)).await;
        let listener = TcpListener::bind(&addr).await.unwrap();
        let _server = tokio::spawn(async move {
            let mut ws = serve_once(listener).await;
            ws.send(WSMessage::text("@ws auth=none\r\n")).await.unwrap();
            // Stay open while the retryer finishes its handshake; a close here
            // would race the frame above and turn this back into a hang-up test.
            tokio::time::sleep(Duration::from_secs(2)).await;
            ws
        });

        let transport = dial
            .await
            .unwrap()
            .expect("a refused dial must be retried until the bridge listens");
        assert_eq!(transport.kind(), TransportKind::Lan);
        assert!(
            started.elapsed() >= RETRY_DELAYS[0],
            "success must come from a retry, not from the first dial: {:?}",
            started.elapsed()
        );
        drop(transport);
    }

    #[tokio::test]
    async fn a_dropped_handshake_is_retried_until_the_bridge_answers() {
        let addr = free_addr();
        let listener = TcpListener::bind(&addr).await.unwrap();
        let server = tokio::spawn(async move {
            // First contact: take the TCP connection and hang up before the
            // upgrade, which is what a bridge whose client slots are still
            // busy does (src/ws_bridge.c `linkr_ws_setup` → -ENOENT).
            let (first, _) = listener.accept().await.unwrap();
            drop(first);
            // The retry lands on a healthy bridge.
            let mut ws = serve_once(listener).await;
            ws.send(WSMessage::text("@ws auth=none\r\n")).await.unwrap();
        });

        let started = tokio::time::Instant::now();
        let transport = connect(&addr, None)
            .await
            .expect("a hang-up before the handshake must be retried");
        assert_eq!(transport.kind(), TransportKind::Lan);
        assert!(
            started.elapsed() >= Duration::from_millis(600),
            "attempts are backed off, not hammered: {:?}",
            started.elapsed()
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn a_bulk_lan_write_is_split_and_paced_to_the_bridge() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let transport = LanTransport {
            commands: tx,
            hub: Arc::new(EventHub::new()),
            pace: Pace::new(),
        };

        let started = tokio::time::Instant::now();
        transport.write_uart(&vec![b'A'; 3000]).await.unwrap();
        let elapsed = started.elapsed();

        let mut sizes = Vec::new();
        while let Ok(command) = rx.try_recv() {
            match command {
                LanCommand::Uart(bytes) => sizes.push(bytes.len()),
                LanCommand::Disconnect => panic!("a write must not disconnect"),
            }
        }
        assert_eq!(
            sizes,
            vec![LAN_FRAME_MAX_BYTES, LAN_FRAME_MAX_BYTES, 952],
            "bulk input is cut into frames the bridge queue can take"
        );
        assert!(
            elapsed >= Duration::from_millis(200),
            "frames must be spaced out, not fired at once: {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn a_single_keystroke_is_not_held_back_by_the_rate_limiter() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let transport = LanTransport {
            commands: tx,
            hub: Arc::new(EventHub::new()),
            pace: Pace::new(),
        };

        let started = tokio::time::Instant::now();
        transport.write_uart(b"x").await.unwrap();
        assert!(rx.try_recv().is_ok(), "the byte must go out immediately");
        assert!(
            started.elapsed() < Duration::from_millis(50),
            "typing must never wait for the limiter: {:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn connect_rejects_an_empty_host_like_the_web_client() {
        let err = connect("   ", None)
            .await
            .err()
            .expect("an empty host must be rejected");
        assert_eq!(err.to_string(), EMPTY_HOST_ERROR);
    }

    #[tokio::test]
    async fn connect_validates_the_token_before_touching_the_network() {
        let err = connect("127.0.0.1:1", Some("nope"))
            .await
            .err()
            .expect("a malformed token must be rejected up front");
        assert_eq!(err.to_string(), TOKEN_ERROR);
    }

    #[test]
    fn management_writes_are_rejected_with_a_clear_error() {
        // The transport-level backstop; the session rejects earlier with the
        // same message.
        let hub = Arc::new(EventHub::new());
        let (tx, _rx) = mpsc::unbounded_channel();
        let transport = LanTransport {
            commands: tx,
            hub,
            pace: Pace::new(),
        };
        let result = futures::executor::block_on(transport.write_mgmt(b"@i?"));
        assert_eq!(
            result.unwrap_err().to_string(),
            "management commands are not available over the LAN bridge; connect over BLE to run them"
        );
    }
}
