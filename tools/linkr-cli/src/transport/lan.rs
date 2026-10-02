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

/// A connected LAN bridge: UART bytes travel as binary frames, management
/// commands are rejected (the bridge has no Management Service).
pub struct LanTransport {
    commands: mpsc::UnboundedSender<LanCommand>,
    hub: Arc<EventHub>,
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
        self.commands
            .send(LanCommand::Uart(chunk.to_vec()))
            .map_err(|_| anyhow::anyhow!("LAN bridge closed"))
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
pub async fn connect_with_timeouts(
    host: &str,
    token: Option<&str>,
    connect_timeout: Duration,
    handshake_timeout: Duration,
) -> anyhow::Result<Arc<dyn Transport>> {
    if host.trim().is_empty() {
        return Err(anyhow::anyhow!(EMPTY_HOST_ERROR));
    }
    let mut handshake = Handshake::new(token).map_err(anyhow::Error::msg)?;
    let url = normalize_url(host);

    let connected =
        tokio::time::timeout(connect_timeout, tokio_tungstenite::connect_async(&url)).await;
    let (stream, _) = match connected {
        Err(_) => {
            return Err(anyhow::anyhow!(
                "WebSocket connection timed out after {} seconds.",
                connect_timeout.as_secs()
            ))
        }
        Ok(Err(_)) => return Err(anyhow::anyhow!(unreachable(&url))),
        Ok(Ok(pair)) => pair,
    };

    // --- access handshake (web connectWs steps 4-5) -----------------------
    let mut stream = stream;
    let mut buffered: Vec<Vec<u8>> = Vec::new();
    let deadline = tokio::time::Instant::now() + handshake_timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(anyhow::anyhow!(HANDSHAKE_TIMEOUT_MSG));
        }
        let next = tokio::time::timeout(remaining, stream.next()).await;
        let frame = match next {
            Err(_) => return Err(anyhow::anyhow!(HANDSHAKE_TIMEOUT_MSG)),
            Ok(None) => return Err(anyhow::anyhow!(close_message(&url, handshake.token_sent()))),
            Ok(Some(Err(_))) => {
                return Err(anyhow::anyhow!(close_message(&url, handshake.token_sent())))
            }
            Ok(Some(Ok(frame))) => frame,
        };
        match frame {
            Message::Text(text) => match handshake
                .on_text(text.as_str())
                .map_err(anyhow::Error::msg)?
            {
                HandshakeAction::Done => break,
                HandshakeAction::Send(token) => {
                    stream.send(Message::text(token)).await.map_err(|_| {
                        anyhow::anyhow!(close_message(&url, handshake.token_sent()))
                    })?;
                }
                HandshakeAction::Ignore => crate::cli::warn(IGNORED_FRAME),
                HandshakeAction::Continue => {}
            },
            Message::Binary(bytes) => buffered.push(bytes.to_vec()),
            Message::Close(_) => {
                return Err(anyhow::anyhow!(close_message(&url, handshake.token_sent())))
            }
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

        let err = connect(&addr, Some(token))
            .await
            .err()
            .expect("a wrong token must fail the handshake");
        assert_eq!(err.to_string(), TOKEN_REJECTED);
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
    async fn connect_reports_unreachable_when_nothing_listens() {
        let addr = free_addr();
        let err = connect(&addr, None)
            .await
            .err()
            .expect("an unreachable bridge must fail");
        assert_eq!(err.to_string(), unreachable(&normalize_url(&addr)));
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
        let transport = LanTransport { commands: tx, hub };
        let result = futures::executor::block_on(transport.write_mgmt(b"@i?"));
        assert_eq!(
            result.unwrap_err().to_string(),
            "management commands are not available over the LAN bridge; connect over BLE to run them"
        );
    }
}
