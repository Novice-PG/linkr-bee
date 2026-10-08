//! Session orchestration: owns the transport, framing codecs, geometry sync,
//! capability gating and the management request pipeline. One session serves
//! the CLI terminal loop, the TUI and the assistant at the same time.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{broadcast, mpsc, oneshot};

use crate::event::{ConnectionState, CoreEvent, NoticeLevel, RequestId};
use crate::protocol::mgmt::{
    MGMT_CAP_ASYNC_EVENTS, MGMT_CAP_WEBDAV, MGMT_CAP_WIFI, MGMT_RESPONSE_TIMEOUT_SECS,
};
use crate::protocol::validate::python_repr_bytes;
use crate::protocol::{MgmtCore, MgmtError, MgmtReply, TerminalGeometrySync, UartCodec};
use crate::transport::{Transport, TransportChannel, TransportEvent, TransportKind};

#[derive(Debug, Clone)]
pub enum TransportSpec {
    Ble {
        name: String,
        address: Option<String>,
        timeout: Duration,
    },
    Lan {
        host: String,
        token: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub struct SessionOptions {
    pub transport: TransportSpec,
    /// `--ble-write-size` override (0 = auto).
    pub ble_write_size: usize,
    /// Append raw UART RX bytes to this file.
    pub log_file: Option<PathBuf>,
    /// Emit `--debug-io` traces as `CoreEvent::Notice`.
    pub debug_io: bool,
    /// Enable geometry sync (only when a real TTY owns the terminal).
    pub geometry: bool,
}

/// Connect-time/output hints that do not belong in [`SessionOptions`] (whose
/// shape the contract fixes). [`spawn_session`] uses the defaults.
#[derive(Debug, Clone, Default)]
pub struct SessionSetup {
    /// `--pair`: ask the OS to bond while connecting (handled by
    /// `transport::ble`, which owns the connect sequence).
    pub pair: bool,
    /// `--json`: recorded on the management core so `MgmtCore::json()` tells
    /// the truth; the rendering itself happens in the UI layer.
    pub json: bool,
}

#[derive(Debug, Clone, Default)]
pub struct SessionInfo {
    pub kind: Option<TransportKind>,
    pub connected: bool,
    pub label: String,
    pub device_id: Option<String>,
    pub capabilities: u32,
    pub write_size: usize,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub baud: u64,
}

#[derive(Debug)]
enum SessionCommand {
    Uart {
        bytes: Vec<u8>,
    },
    Mgmt {
        cmd: String,
        wait_final: Option<Duration>,
        reply: oneshot::Sender<Result<MgmtReply, String>>,
    },
    TerminalSize {
        cols: u16,
        rows: u16,
    },
    Disconnect,
}

/// Cloneable handle every UI/agent component uses to reach the session.
#[derive(Clone)]
pub struct SessionHandle {
    tx: mpsc::UnboundedSender<SessionCommand>,
    info: Arc<Mutex<SessionInfo>>,
    bus: CoreBus,
}

impl SessionHandle {
    /// A handle with no session task behind it: every send fails, the bus is
    /// empty and `info()` reports a fresh (disconnected) state. The TUI opens
    /// with one of these when the CLI deferred the connect (A5: the interface
    /// must come up with the radio down), and UI tests use it to render panes
    /// without a transport (fields stay private, so this is the only way to
    /// build one outside `session.rs`).
    pub fn detached(bus: CoreBus) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        drop(rx);
        Self {
            tx,
            info: Arc::new(Mutex::new(SessionInfo::default())),
            bus,
        }
    }

    /// Queue raw UART bytes for the target (reliable framing applied by the
    /// session when the transport supports it; raw for LAN).
    pub fn send_uart(&self, bytes: Vec<u8>) -> anyhow::Result<()> {
        self.tx
            .send(SessionCommand::Uart { bytes })
            .map_err(|_| anyhow::anyhow!("session gone"))?;
        Ok(())
    }

    /// Send a management command; resolves with the response (and, when
    /// `wait_final` is set, after the matching FINAL event arrives).
    pub fn request_mgmt(
        &self,
        cmd: String,
        wait_final: Option<Duration>,
    ) -> oneshot::Receiver<Result<MgmtReply, String>> {
        let (reply, rx) = oneshot::channel();
        let _ = self.tx.send(SessionCommand::Mgmt {
            cmd,
            wait_final,
            reply,
        });
        rx
    }

    /// Report the local terminal size so geometry sync can react (mirrors the
    /// web client's debounced `stty` push).
    pub fn set_terminal_size(&self, cols: u16, rows: u16) {
        let _ = self.tx.send(SessionCommand::TerminalSize { cols, rows });
    }

    pub fn disconnect(&self) {
        let _ = self.tx.send(SessionCommand::Disconnect);
    }

    pub fn info(&self) -> SessionInfo {
        self.info.lock().expect("session info poisoned").clone()
    }

    pub fn bus(&self) -> &CoreBus {
        &self.bus
    }
}

#[cfg(test)]
impl SessionHandle {
    /// [`SessionHandle::detached`] on a fresh bus: what the view tests want.
    #[allow(dead_code)]
    pub fn test_detached() -> Self {
        SessionHandle::detached(CoreBus::new())
    }

    /// The same handle with the link reported up: the agent's send path opens
    /// on `info().connected`, so a test that has to reach the approval window
    /// — and the guard that runs when the answer arrives — needs a session
    /// that says yes first.
    #[allow(dead_code)]
    pub fn test_connected() -> Self {
        let handle = SessionHandle::detached(CoreBus::new());
        {
            let mut info = handle.info.lock().expect("session info poisoned");
            info.connected = true;
            info.label = "test-device".to_string();
        }
        handle
    }
}

/// Broadcast bus carrying `CoreEvent`s to every subscriber.
#[derive(Clone)]
pub struct CoreBus {
    tx: broadcast::Sender<CoreEvent>,
}

impl CoreBus {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(1024);
        Self { tx }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<CoreEvent> {
        self.tx.subscribe()
    }

    pub fn publish(&self, event: CoreEvent) {
        let _ = self.tx.send(event);
    }
}

impl Default for CoreBus {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Command helpers (pure, unit-tested)
// ---------------------------------------------------------------------------

/// Display form of a management command for `control -> …` lines and JSON
/// `command` fields: `@w=`/`@d=` prefixes are redacted, everything else
/// travels in the clear (PYTHON_CLI_SPEC section 3.2).
pub(crate) fn redact_command(command: &str) -> String {
    if command.starts_with("@w=") || command.starts_with("@d=") {
        format!("{}=<redacted>", &command[..2])
    } else {
        command.to_string()
    }
}

/// Capabilities a command needs; `0` when it needs none.
pub(crate) fn required_capabilities(command: &str) -> u32 {
    if command.starts_with("@w=") || command == "@w scan" || command == "@w off" {
        return MGMT_CAP_WIFI | MGMT_CAP_ASYNC_EVENTS;
    }
    if command == "@w?" {
        return MGMT_CAP_WIFI;
    }
    if command.starts_with("@d=") || command == "@d off" || command == "@d?" {
        return MGMT_CAP_WEBDAV;
    }
    0
}

/// Capability gate per command with the exact Python handshake strings, in
/// the Python check order (WiFi, then async events, then WebDAV).
pub(crate) fn check_capabilities(command: &str, capabilities: u32) -> Result<(), String> {
    let required = required_capabilities(command);
    if required & MGMT_CAP_WIFI != 0 && capabilities & MGMT_CAP_WIFI == 0 {
        return Err("device does not advertise WiFi support".to_string());
    }
    if required & MGMT_CAP_ASYNC_EVENTS != 0 && capabilities & MGMT_CAP_ASYNC_EVENTS == 0 {
        return Err("device does not advertise async event support".to_string());
    }
    if required & MGMT_CAP_WEBDAV != 0 && capabilities & MGMT_CAP_WEBDAV == 0 {
        return Err("device does not advertise WebDAV support".to_string());
    }
    Ok(())
}

/// Best-effort baud extraction from a `@u=`/`@u?` exchange (status bar).
pub(crate) fn parse_baud_line(line: &str) -> Option<u64> {
    for token in line.split_whitespace() {
        let token = token
            .strip_prefix("uart=")
            .or_else(|| token.strip_prefix("baud="))
            .unwrap_or(token);
        let head = token.split(',').next().unwrap_or("");
        if let Ok(baud) = head.parse::<u64>() {
            return Some(baud);
        }
    }
    None
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

// ---------------------------------------------------------------------------
// The session task
// ---------------------------------------------------------------------------

/// Handshake results shaped for the task.
struct HandshakeData {
    info: SessionInfo,
    detail: String,
    mgmt_max: Option<u16>,
    uart: Option<(u16, u32, u32)>,
}

struct PreparedMgmt {
    id: RequestId,
    sensitive: bool,
    chunks: Vec<Vec<u8>>,
    response: oneshot::Receiver<Result<MgmtReply, MgmtError>>,
    final_rx: Option<oneshot::Receiver<Result<MgmtReply, MgmtError>>>,
}

struct SessionTask {
    transport: Arc<dyn Transport>,
    bus: CoreBus,
    info: Arc<Mutex<SessionInfo>>,
    kind: TransportKind,
    mgmt: Option<MgmtCore>,
    mgmt_max: u16,
    uart: Option<UartCodec>,
    geometry: Option<TerminalGeometrySync>,
    log: Option<std::fs::File>,
    debug_io: bool,
    /// Serializes whole management requests (chunks + waits): the firmware
    /// reassembles one Command at a time.
    write_lock: Arc<tokio::sync::Mutex<()>>,
}

impl SessionTask {
    async fn run(
        mut self,
        mut commands: mpsc::UnboundedReceiver<SessionCommand>,
        mut events: broadcast::Receiver<TransportEvent>,
    ) {
        loop {
            tokio::select! {
                command = commands.recv() => match command {
                    None => {
                        self.teardown("session closed").await;
                        return;
                    }
                    Some(command) => {
                        if !self.handle_command(command).await {
                            return;
                        }
                    }
                },
                event = events.recv() => match event {
                    Ok(event) => {
                        if !self.handle_transport_event(event).await {
                            return;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => {
                        self.teardown("transport gone").await;
                        return;
                    }
                },
            }
        }
    }

    /// Returns `false` when the task must stop.
    async fn handle_command(&mut self, command: SessionCommand) -> bool {
        match command {
            SessionCommand::Uart { bytes } => {
                // Typed input invalidates the idle prompt we resize from
                // (Python `send_payload`).
                if let Some(geometry) = &mut self.geometry {
                    geometry.mark_busy();
                }
                if let Err(error) = self.write_uart(&bytes).await {
                    self.bus.publish(CoreEvent::Notice {
                        level: NoticeLevel::Error,
                        text: error.to_string(),
                    });
                }
                true
            }
            SessionCommand::Mgmt {
                cmd,
                wait_final,
                reply,
            } => {
                self.handle_mgmt(cmd, wait_final, reply);
                true
            }
            SessionCommand::TerminalSize { cols, rows } => {
                if let Some(geometry) = &mut self.geometry {
                    geometry.set_size(u32::from(cols), u32::from(rows));
                }
                true
            }
            SessionCommand::Disconnect => {
                self.teardown("disconnected").await;
                false
            }
        }
    }

    /// Validation, capability gate and request registration. Runs inline so
    /// `control -> …` is published before any byte hits the wire.
    fn prepare_mgmt(&mut self, command: &str, with_final: bool) -> Result<PreparedMgmt, String> {
        if self.kind == TransportKind::Lan {
            return Err(
                "management commands are not available over the LAN bridge; connect over BLE to run them"
                    .to_string(),
            );
        }
        {
            let capabilities = self
                .info
                .lock()
                .expect("session info poisoned")
                .capabilities;
            check_capabilities(command, capabilities)?;
        }
        if command.is_empty() || command.len() > usize::from(self.mgmt_max) {
            return Err(MgmtError::OutsideLimit.to_string());
        }
        let display = redact_command(command);
        let sensitive = display.ends_with("=<redacted>");
        self.bus.publish(CoreEvent::Notice {
            level: NoticeLevel::Info,
            text: format!("control -> {display}"),
        });
        let mgmt = self
            .mgmt
            .as_mut()
            .ok_or_else(|| "management channel unavailable".to_string())?;
        let (id, chunks, response, final_rx) = mgmt
            .begin(command, with_final)
            .map_err(|error| error.to_string())?;
        Ok(PreparedMgmt {
            id,
            sensitive,
            chunks,
            response,
            final_rx,
        })
    }

    /// Write the chunks and wait for the response/final events in a separate
    /// task: this loop must keep feeding indications while the request waits.
    fn handle_mgmt(
        &mut self,
        command: String,
        wait_final: Option<Duration>,
        reply: oneshot::Sender<Result<MgmtReply, String>>,
    ) {
        let prepared = match self.prepare_mgmt(&command, wait_final.is_some()) {
            Ok(prepared) => prepared,
            Err(error) => {
                let _ = reply.send(Err(error));
                return;
            }
        };
        let PreparedMgmt {
            id,
            sensitive,
            chunks,
            response,
            final_rx,
        } = prepared;
        let transport = Arc::clone(&self.transport);
        let bus = self.bus.clone();
        let info = Arc::clone(&self.info);
        let lock = Arc::clone(&self.write_lock);
        let debug = self.debug_io;
        tokio::spawn(async move {
            let _guard = lock.lock().await;
            let result = run_mgmt_request(
                transport, bus, info, debug, id, sensitive, &command, chunks, response, final_rx,
                wait_final,
            )
            .await;
            let _ = reply.send(result);
        });
    }

    async fn handle_transport_event(&mut self, event: TransportEvent) -> bool {
        match event {
            TransportEvent::Subscribed => true,
            TransportEvent::Data { channel, bytes } => {
                match channel {
                    TransportChannel::MgmtResponse => {
                        if let Some(mgmt) = &mut self.mgmt {
                            if let Some(event) = mgmt.feed(&bytes) {
                                self.bus.publish(event);
                            }
                        }
                    }
                    TransportChannel::UartTx => self.on_uart_rx(bytes, true).await,
                    TransportChannel::NusTx => self.on_uart_rx(bytes, false).await,
                }
                true
            }
            TransportEvent::Text(text) => {
                self.bus.publish(CoreEvent::Notice {
                    level: NoticeLevel::Info,
                    text,
                });
                true
            }
            TransportEvent::Disconnected { reason } => {
                self.teardown(&reason).await;
                false
            }
        }
    }

    /// Inbound UART payload: codec notices, debug trace, log file, geometry
    /// observe, publish (PYTHON_CLI_SPEC 6.4 order), then a geometry `stty` if
    /// one is due.
    async fn on_uart_rx(&mut self, raw: Vec<u8>, framed: bool) {
        let payload = if framed {
            match &mut self.uart {
                Some(codec) => codec.feed(&raw),
                None => Ok(Some(raw)),
            }
        } else {
            Ok(Some(raw))
        };
        // Python prints the channel's warnings from inside `on_indication`,
        // i.e. before the payload reaches the data handler.
        self.drain_uart_notices();
        let payload = match payload {
            Ok(payload) => payload,
            Err(gap) => {
                self.bus.publish(CoreEvent::Notice {
                    level: NoticeLevel::Warn,
                    text: format!(
                        "reliable UART sequence gap: expected {}, got {}",
                        gap.expected, gap.got
                    ),
                });
                return;
            }
        };
        let Some(payload) = payload else {
            return;
        };
        if self.debug_io {
            self.bus.publish(CoreEvent::Notice {
                level: NoticeLevel::Info,
                text: format!("RX {}", python_repr_bytes(&payload)),
            });
        }
        if let Some(log) = &mut self.log {
            let _ = std::io::Write::write_all(log, &payload);
        }
        if let Some(geometry) = &mut self.geometry {
            geometry.observe(&String::from_utf8_lossy(&payload));
        }
        self.info.lock().expect("session info poisoned").rx_bytes += payload.len() as u64;
        self.bus.publish(CoreEvent::UartRx(payload));
        self.geometry_tick().await;
    }

    /// Publish every warning the framing codec collected while reassembling.
    fn drain_uart_notices(&mut self) {
        let notices = match &mut self.uart {
            Some(codec) => codec.take_notices(),
            None => Vec::new(),
        };
        for (level, text) in notices {
            self.bus.publish(CoreEvent::Notice { level, text });
        }
    }

    /// Fire the pending `stty` command when the geometry machine says one is
    /// due (fires only at an idle shell prompt — see protocol::geometry).
    async fn geometry_tick(&mut self) {
        let pending = match &mut self.geometry {
            Some(geometry) => geometry.take_pending_command(),
            None => return,
        };
        let Some(command) = pending else {
            return;
        };
        let result = self.write_uart(command.as_bytes()).await;
        match &mut self.geometry {
            Some(geometry) if result.is_ok() => geometry.confirm_sent(),
            Some(geometry) => geometry.abort_sent(),
            None => {}
        }
        if let Err(error) = result {
            self.bus.publish(CoreEvent::Notice {
                level: NoticeLevel::Warn,
                text: format!("terminal size sync failed: {error}"),
            });
        }
    }

    /// Frame (BLE) or pass through (LAN) `data` and write it to the wire.
    async fn write_uart(&mut self, data: &[u8]) -> anyhow::Result<()> {
        match self.kind {
            TransportKind::Lan => {
                self.transport.write_uart(data).await?;
                self.info.lock().expect("session info poisoned").tx_bytes += data.len() as u64;
            }
            TransportKind::Ble => {
                let codec = self
                    .uart
                    .as_mut()
                    .ok_or_else(|| anyhow::anyhow!("reliable UART unavailable"))?;
                let att = self.transport.write_size().clamp(20, 244);
                let header = crate::protocol::uart::UART_HEADER_SIZE;
                // Payload carried by each logical frame, in write order.
                let frames: Vec<usize> =
                    data.chunks(codec.max_payload()).map(<[u8]>::len).collect();
                let progress = tx_progress(&frames, header, att);
                let chunks = codec.encode_write(data, att);
                let mut sequence = 0u32;
                let mut counted = 0u64;
                for (chunk, cumulative) in chunks.iter().zip(progress) {
                    if self.debug_io {
                        if chunk.len() >= 12 && &chunk[..2] == b"LR" {
                            sequence = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]);
                        }
                        self.bus.publish(CoreEvent::Notice {
                            level: NoticeLevel::Info,
                            text: format!("UART TX #{sequence} {}", python_repr_bytes(chunk)),
                        });
                    }
                    self.transport.write_uart(chunk).await?;
                    // Count the payload that just hit the wire, not the whole
                    // payload up front: the web bumps `txBytes` per chunk
                    // (app.js writeBytes), so a long paste must not freeze the
                    // counters for its whole duration (it used to jump once,
                    // ~12 s after the paste started).
                    if cumulative > counted {
                        self.info.lock().expect("session info poisoned").tx_bytes +=
                            cumulative - counted;
                        counted = cumulative;
                    }
                }
            }
        }
        Ok(())
    }

    /// Fail pending requests, close the log and publish the disconnect once.
    async fn teardown(&mut self, reason: &str) {
        if let Some(mgmt) = &mut self.mgmt {
            mgmt.fail_all();
        }
        let first_time = {
            let mut info = self.info.lock().expect("session info poisoned");
            let was_connected = info.connected;
            info.connected = false;
            was_connected
        };
        let _ = self.transport.disconnect().await;
        self.log = None;
        if first_time {
            self.bus.publish(CoreEvent::Connection {
                state: ConnectionState::Disconnected,
                detail: reason.to_string(),
            });
        }
    }
}

/// Cumulative user payload written after each ATT chunk, in write order.
///
/// `frames` are the logical frame payload sizes the codec produced (each frame
/// is a `header`-byte LR header plus payload, split into `att`-sized writes).
/// The result zips 1:1 with the chunk list, so progress counters can advance as
/// every write lands instead of once for the whole payload. Only payload bytes
/// are counted — headers are framing overhead, never user data (matching the
/// web's `state.txBytes += chunk.length`).
fn tx_progress(frames: &[usize], header: usize, att: usize) -> Vec<u64> {
    let mut out = Vec::new();
    let mut total = 0u64;
    for &payload_len in frames {
        let frame_len = header + payload_len;
        let mut pos = 0usize;
        while pos < frame_len {
            let start = pos.max(header);
            let end = (pos + att).min(frame_len);
            if end > start {
                total += (end - start) as u64;
            }
            pos += att;
            out.push(total);
        }
    }
    out
}

/// The request pipeline: chunked writes, 5 s response timeout, optional
/// wait-final window (PYTHON_CLI_SPEC 2.6). The parameters are the request's
/// own fields; grouping them would just move them one struct away.
#[allow(clippy::too_many_arguments)]
async fn run_mgmt_request(
    transport: Arc<dyn Transport>,
    bus: CoreBus,
    info: Arc<Mutex<SessionInfo>>,
    debug: bool,
    id: RequestId,
    sensitive: bool,
    command: &str,
    chunks: Vec<Vec<u8>>,
    response: oneshot::Receiver<Result<MgmtReply, MgmtError>>,
    final_rx: Option<oneshot::Receiver<Result<MgmtReply, MgmtError>>>,
    wait_final: Option<Duration>,
) -> Result<MgmtReply, String> {
    for chunk in &chunks {
        if debug {
            let trace = if sensitive {
                format!("MGMT TX #{id} <redacted {} bytes>", chunk.len())
            } else {
                format!("MGMT TX #{id} {}", python_repr_bytes(chunk))
            };
            bus.publish(CoreEvent::Notice {
                level: NoticeLevel::Info,
                text: trace,
            });
        }
        transport
            .write_mgmt(chunk)
            .await
            .map_err(|e| e.to_string())?;
    }
    let response_timeout = Duration::from_secs_f64(MGMT_RESPONSE_TIMEOUT_SECS);
    let reply = await_reply(response, response_timeout).await?;
    if let Some(final_rx) = final_rx {
        let final_timeout = wait_final.unwrap_or(response_timeout);
        let _final = await_reply(final_rx, final_timeout).await?;
    }
    note_baud(&info, command, &reply);
    Ok(reply)
}

/// A timeout produces an empty error string, which is what Python's
/// `str(asyncio.TimeoutError())` prints as `linkr: error: `.
async fn await_reply(
    response: oneshot::Receiver<Result<MgmtReply, MgmtError>>,
    timeout: Duration,
) -> Result<MgmtReply, String> {
    match tokio::time::timeout(timeout, response).await {
        Err(_) => Err(String::new()),
        Ok(Err(_closed)) => Err(MgmtError::Disconnected.to_string()),
        Ok(Ok(Ok(reply))) => Ok(reply),
        Ok(Ok(Err(error))) => Err(error.to_string()),
    }
}

fn note_baud(info: &Arc<Mutex<SessionInfo>>, command: &str, reply: &MgmtReply) {
    let mut info = info.lock().expect("session info poisoned");
    if let Some(spec) = command.strip_prefix("@u=") {
        if let Some(baud) = spec
            .split(',')
            .next()
            .and_then(|value| value.trim().parse::<u64>().ok())
        {
            info.baud = baud;
            return;
        }
    }
    if command == "@u?" {
        for line in &reply.lines {
            if let Some(baud) = parse_baud_line(line) {
                info.baud = baud;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Spawning
// ---------------------------------------------------------------------------

fn spawn_on(
    transport: Arc<dyn Transport>,
    data: HandshakeData,
    opts: &SessionOptions,
    setup: &SessionSetup,
    log: Option<std::fs::File>,
    bus: CoreBus,
) -> SessionHandle {
    let kind = transport.kind();
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let handle = SessionHandle {
        tx: cmd_tx,
        info: Arc::new(Mutex::new(data.info.clone())),
        bus: bus.clone(),
    };
    let geometry = if opts.geometry {
        let (cols, rows) = crate::term::terminal_size().unwrap_or((80, 24));
        Some(TerminalGeometrySync::new(u32::from(cols), u32::from(rows)))
    } else {
        None
    };
    let mgmt = data.mgmt_max.map(|max| {
        let mut core = MgmtCore::new(max, setup.json);
        // Python's ManagementChannel chunks with `max(20, cfg.write_size)`.
        let write_size = transport.write_size();
        if write_size > 0 {
            core.set_write_size(write_size);
        }
        core
    });
    let uart = data.uart.map(|(max_payload, tx_sequence, rx_sequence)| {
        UartCodec::new(max_payload, tx_sequence, rx_sequence)
    });
    let task = SessionTask {
        transport,
        bus: bus.clone(),
        info: Arc::clone(&handle.info),
        kind,
        mgmt,
        mgmt_max: data.mgmt_max.unwrap_or(0),
        uart,
        geometry,
        log,
        debug_io: opts.debug_io,
        write_lock: Arc::new(tokio::sync::Mutex::new(())),
    };
    let events = task.transport.events();
    bus.publish(CoreEvent::Connection {
        state: ConnectionState::Connected,
        detail: data.detail,
    });
    tokio::spawn(task.run(cmd_rx, events));
    handle
}

/// Spawn the session task. Returns once the transport is connected and the
/// handshake reads succeeded. Equivalent to [`spawn_session_with`] with the
/// default [`SessionSetup`].
pub async fn spawn_session(opts: SessionOptions, bus: CoreBus) -> anyhow::Result<SessionHandle> {
    spawn_session_with(opts, bus, SessionSetup::default()).await
}

/// [`spawn_session`] with the `--pair`/`--json` hints.
pub async fn spawn_session_with(
    opts: SessionOptions,
    bus: CoreBus,
    setup: SessionSetup,
) -> anyhow::Result<SessionHandle> {
    bus.publish(CoreEvent::Connection {
        state: ConnectionState::Connecting,
        detail: "connecting...".to_string(),
    });
    // The log file opens before the radio work (PYTHON_CLI_SPEC 6.9).
    let log = match &opts.log_file {
        Some(path) => Some(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .map_err(|error| {
                    anyhow::anyhow!("cannot open log file {}: {error}", path.display())
                })?,
        ),
        None => None,
    };
    let (transport, data) = match opts.transport.clone() {
        TransportSpec::Ble {
            name,
            address,
            timeout,
        } => {
            let (transport, handshake) = crate::transport::ble::connect(
                &name,
                address.as_deref(),
                timeout,
                opts.ble_write_size,
                setup.pair,
            )
            .await?;
            let device_id = to_hex(&handshake.device_id);
            let data = HandshakeData {
                info: SessionInfo {
                    kind: Some(TransportKind::Ble),
                    connected: true,
                    label: handshake.label,
                    device_id: Some(device_id.clone()),
                    capabilities: handshake.protocol.capabilities,
                    write_size: handshake.write_size,
                    rx_bytes: 0,
                    tx_bytes: 0,
                    baud: 0,
                },
                detail: format!(
                    "API v{}.{} device={device_id}",
                    handshake.protocol.major, handshake.protocol.minor
                ),
                mgmt_max: Some(handshake.protocol.max_payload),
                uart: Some((
                    handshake.reliable.max_payload,
                    handshake.reliable.tx_sequence,
                    handshake.reliable.rx_sequence,
                )),
            };
            (transport, data)
        }
        TransportSpec::Lan { host, token } => {
            let transport = crate::transport::lan::connect(&host, token.as_deref()).await?;
            let data = HandshakeData {
                info: SessionInfo {
                    kind: Some(TransportKind::Lan),
                    connected: true,
                    label: host.clone(),
                    device_id: None,
                    capabilities: 0,
                    write_size: 0,
                    rx_bytes: 0,
                    tx_bytes: 0,
                    baud: 0,
                },
                detail: format!("LAN bridge {host}"),
                mgmt_max: None,
                uart: None,
            };
            (transport, data)
        }
    };
    Ok(spawn_on(transport, data, &opts, &setup, log, bus))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::MgmtKind;
    use crate::transport::{EventHub, TransportChannel, TransportEvent};

    /// A transport that records every frame and can inject indications.
    /// `writes` holds `(consumed, frame)` pairs from the chunked writer.
    type Written = Arc<Mutex<Vec<(bool, Vec<u8>)>>>;

    struct MockTransport {
        kind: TransportKind,
        hub: Arc<EventHub>,
        writes: Written,
        write_size: usize,
    }

    #[async_trait::async_trait]
    impl Transport for MockTransport {
        fn kind(&self) -> TransportKind {
            self.kind
        }

        async fn write_mgmt(&self, chunk: &[u8]) -> anyhow::Result<()> {
            self.writes
                .lock()
                .expect("mock writes")
                .push((true, chunk.to_vec()));
            Ok(())
        }

        async fn write_uart(&self, chunk: &[u8]) -> anyhow::Result<()> {
            self.writes
                .lock()
                .expect("mock writes")
                .push((false, chunk.to_vec()));
            Ok(())
        }

        async fn disconnect(&self) -> anyhow::Result<()> {
            Ok(())
        }

        fn write_size(&self) -> usize {
            self.write_size
        }

        fn events(&self) -> broadcast::Receiver<TransportEvent> {
            self.hub.subscribe()
        }
    }

    struct Mock {
        session: SessionHandle,
        bus: CoreBus,
        writes: Written,
        hub: Arc<EventHub>,
    }

    fn spawn_mock(kind: TransportKind, capabilities: u32, geometry: bool) -> Mock {
        let hub = Arc::new(EventHub::new());
        let writes = Arc::new(Mutex::new(Vec::new()));
        let transport: Arc<dyn Transport> = Arc::new(MockTransport {
            kind,
            hub: Arc::clone(&hub),
            writes: Arc::clone(&writes),
            write_size: 20,
        });
        let data = HandshakeData {
            info: SessionInfo {
                kind: Some(kind),
                connected: true,
                label: "mock".to_string(),
                device_id: Some("ab".repeat(16)),
                capabilities,
                write_size: 20,
                rx_bytes: 0,
                tx_bytes: 0,
                baud: 0,
            },
            detail: "API v1.0 device=ab".to_string(),
            mgmt_max: if kind == TransportKind::Ble {
                Some(100)
            } else {
                None
            },
            uart: if kind == TransportKind::Ble {
                Some((200, 1, 1))
            } else {
                None
            },
        };
        let opts = SessionOptions {
            transport: TransportSpec::Ble {
                name: "mock".to_string(),
                address: None,
                timeout: Duration::from_secs(1),
            },
            ble_write_size: 0,
            log_file: None,
            debug_io: false,
            geometry,
        };
        let bus = CoreBus::new();
        let session = spawn_on(
            transport,
            data,
            &opts,
            &SessionSetup::default(),
            None,
            bus.clone(),
        );
        Mock {
            session,
            bus,
            writes,
            hub,
        }
    }

    async fn request(session: &SessionHandle, command: &str) -> Result<MgmtReply, String> {
        session
            .request_mgmt(command.to_string(), None)
            .await
            .expect("reply channel")
    }

    async fn wait_for(check: impl Fn() -> bool) -> bool {
        for _ in 0..400 {
            if check() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        false
    }

    fn frames(mock: &Mock) -> Vec<(bool, Vec<u8>)> {
        mock.writes.lock().expect("mock writes").clone()
    }

    /// One management indication: `LK` header + payload (PYTHON_CLI_SPEC 2.1).
    fn mgmt_frame(message_type: u8, id: u32, flags: u16, payload: &[u8]) -> Vec<u8> {
        let mut frame = Vec::new();
        frame.extend_from_slice(b"LK");
        frame.push(1);
        frame.push(message_type);
        frame.extend_from_slice(&id.to_le_bytes());
        frame.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        frame.extend_from_slice(&flags.to_le_bytes());
        frame.extend_from_slice(payload);
        frame
    }

    // ---- command helpers --------------------------------------------------

    #[test]
    fn redaction_only_touches_the_at_w_and_at_d_prefixes() {
        assert_eq!(redact_command("@w=ssid,secret"), "@w=<redacted>");
        assert_eq!(redact_command("@d=http://user:pass@host/"), "@d=<redacted>");
        assert_eq!(redact_command("@w off"), "@w off");
        assert_eq!(redact_command("@w scan"), "@w scan");
        assert_eq!(redact_command("@d off"), "@d off");
        assert_eq!(redact_command("@i?"), "@i?");
    }

    #[test]
    fn required_capabilities_match_the_command_table() {
        assert_eq!(required_capabilities("@i?"), 0);
        assert_eq!(required_capabilities("@u=115200,8,n,1,n"), 0);
        assert_eq!(required_capabilities("@u?"), 0);
        assert_eq!(
            required_capabilities("@w=ssid,pw"),
            MGMT_CAP_WIFI | MGMT_CAP_ASYNC_EVENTS
        );
        assert_eq!(
            required_capabilities("@w off"),
            MGMT_CAP_WIFI | MGMT_CAP_ASYNC_EVENTS
        );
        assert_eq!(
            required_capabilities("@w scan"),
            MGMT_CAP_WIFI | MGMT_CAP_ASYNC_EVENTS
        );
        assert_eq!(required_capabilities("@w?"), MGMT_CAP_WIFI);
        assert_eq!(required_capabilities("@d=ssh://host/"), MGMT_CAP_WEBDAV);
        assert_eq!(required_capabilities("@d off"), MGMT_CAP_WEBDAV);
        assert_eq!(required_capabilities("@d?"), MGMT_CAP_WEBDAV);
    }

    #[test]
    fn baud_is_read_from_uart_replies() {
        assert_eq!(parse_baud_line("uart=115200,8,n,1,n"), Some(115200));
        assert_eq!(parse_baud_line("baud=9600"), Some(9600));
        assert_eq!(parse_baud_line("fw=1.2.3"), None);
        assert_eq!(parse_baud_line("ok"), None);
    }

    // ---- session behaviour ------------------------------------------------

    #[tokio::test]
    async fn management_over_the_lan_bridge_is_rejected_before_the_wire() {
        let mock = spawn_mock(TransportKind::Lan, 0, false);
        let result = request(&mock.session, "@i?").await;
        assert_eq!(
            result.unwrap_err(),
            "management commands are not available over the LAN bridge; connect over BLE to run them"
        );
        assert!(frames(&mock).is_empty());
    }

    #[tokio::test]
    async fn capability_errors_use_the_python_strings() {
        let none = spawn_mock(TransportKind::Ble, 0, false);
        assert_eq!(
            request(&none.session, "@w=ssid,pw").await.unwrap_err(),
            "device does not advertise WiFi support"
        );
        assert_eq!(
            request(&none.session, "@w off").await.unwrap_err(),
            "device does not advertise WiFi support"
        );
        assert_eq!(
            request(&none.session, "@w?").await.unwrap_err(),
            "device does not advertise WiFi support"
        );
        assert_eq!(
            request(&none.session, "@d=ssh://host/").await.unwrap_err(),
            "device does not advertise WebDAV support"
        );

        let wifi_only = spawn_mock(TransportKind::Ble, MGMT_CAP_WIFI, false);
        assert_eq!(
            request(&wifi_only.session, "@w scan").await.unwrap_err(),
            "device does not advertise async event support"
        );

        let webdav_only = spawn_mock(TransportKind::Ble, MGMT_CAP_WEBDAV, false);
        // The WebDAV capability is not a WiFi capability: a WiFi command is
        // still rejected with the WiFi wording.
        assert_eq!(
            request(&webdav_only.session, "@w?").await.unwrap_err(),
            "device does not advertise WiFi support"
        );
        // ...and a WebDAV command on the same device reaches the wire.
        let pending = webdav_only.session.request_mgmt("@d?".to_string(), None);
        assert!(
            wait_for(|| !frames(&webdav_only).is_empty()).await,
            "@d? must be written when WebDAV is advertised"
        );
        webdav_only.session.disconnect();
        let _ = pending.await;
        assert!(frames(&none).is_empty());
    }

    #[tokio::test]
    async fn ungated_commands_reach_the_wire_and_fail_on_disconnect() {
        let mock = spawn_mock(TransportKind::Ble, 0, false);
        let reply = mock.session.request_mgmt("@i?".to_string(), None);
        assert!(
            wait_for(|| !frames(&mock).is_empty()).await,
            "the request was never written"
        );
        let written = frames(&mock);
        assert!(written[0].0, "management frames use write_mgmt");
        assert_eq!(&written[0].1[..2], b"LK");
        assert!(written[0].1.ends_with(b"@i?"));

        mock.session.disconnect();
        assert_eq!(
            reply.await.expect("reply channel").unwrap_err(),
            "disconnected before management response"
        );
    }

    #[tokio::test]
    async fn commands_above_the_advertised_limit_stay_off_the_wire() {
        let mock = spawn_mock(TransportKind::Ble, MGMT_CAP_WEBDAV, false);
        let long = format!("@d={}", "x".repeat(200));
        assert_eq!(
            request(&mock.session, &long).await.unwrap_err(),
            "management command is outside the advertised limit"
        );
        assert!(frames(&mock).is_empty());
    }

    #[tokio::test]
    async fn uart_sends_are_framed_and_counted() {
        let mock = spawn_mock(TransportKind::Ble, 0, false);
        mock.session.send_uart(b"hello".to_vec()).expect("queued");
        assert!(
            wait_for(|| mock.session.info().tx_bytes == 5).await,
            "the UART frame was never written"
        );
        let written = frames(&mock);
        assert_eq!(written.len(), 1);
        assert!(!written[0].0, "UART frames use write_uart");
        assert_eq!(&written[0].1[..2], b"LR");
        assert!(written[0].1.ends_with(b"hello"));
        mock.session.disconnect();
    }

    /// The progress ledger must count payload only (never the 12-byte LR
    /// header), advance on every chunk, and end at exactly the payload length.
    #[test]
    fn tx_progress_counts_payload_per_chunk() {
        // Two frames: 200 B and 100 B payload, 20 B ATT writes.
        let frames = [200usize, 100usize];
        let progress = tx_progress(&frames, 12, 20);
        // ceil((12 + 200) / 20) + ceil((12 + 100) / 20) = 11 + 6 chunks.
        assert_eq!(progress.len(), 17);
        assert!(
            progress.windows(2).all(|w| w[0] < w[1]),
            "must grow every chunk: {progress:?}"
        );
        assert_eq!(
            *progress.last().expect("non-empty"),
            300,
            "payload only, no headers"
        );
        assert_eq!(
            progress[0], 8,
            "the first write carries 20 - 12 header bytes"
        );
        assert_eq!(*progress.iter().max().expect("non-empty"), 300);
    }

    #[tokio::test]
    async fn a_bulk_uart_write_counts_every_chunk_as_it_lands() {
        let mock = spawn_mock(TransportKind::Ble, 0, false);
        let payload = vec![b'x'; 300];
        mock.session.send_uart(payload.clone()).expect("queued");
        assert!(
            wait_for(|| mock.session.info().tx_bytes == payload.len() as u64).await,
            "the counter must reach the payload length (got {})",
            mock.session.info().tx_bytes
        );
        // Frames are 200 B + 100 B, so the wire carries 17 ATT writes: the
        // counter has to have been raised once per chunk, not once at the end.
        let written = frames(&mock);
        assert_eq!(written.len(), 17, "300 B must be framed into 17 ATT writes");
        assert!(
            written.iter().all(|(mgmt, _)| !mgmt),
            "UART uses write_uart"
        );
        mock.session.disconnect();
    }

    #[tokio::test]
    async fn an_idle_prompt_pushes_the_terminal_size() {
        let mock = spawn_mock(TransportKind::Ble, 0, true);
        mock.hub.publish(TransportEvent::Data {
            channel: TransportChannel::NusTx,
            bytes: b"$ ".to_vec(),
        });
        // The `stty` line leaves as one reliable-UART frame split into 20-byte
        // ATT chunks, so the text only shows up across the whole sequence.
        let joined = || -> Vec<u8> {
            frames(&mock)
                .iter()
                .flat_map(|(_, frame)| frame.iter().copied())
                .collect()
        };
        assert!(
            wait_for(|| {
                joined()
                    .windows(b"stty rows".len())
                    .any(|window| window == b"stty rows")
            })
            .await,
            "no stty command was sent"
        );
        let sent_bytes = joined();
        let text = String::from_utf8_lossy(&sent_bytes);
        assert!(text.contains("stty rows "), "{text}");
        assert!(text.contains(" cols "), "{text}");
        assert!(text.ends_with(">/dev/null 2>&1\r"), "{text}");
        mock.session.disconnect();
    }

    #[tokio::test]
    async fn teardown_publishes_exactly_one_disconnect() {
        let mock = spawn_mock(TransportKind::Ble, 0, false);
        let mut events = mock.bus.subscribe();
        mock.session.disconnect();
        mock.session.disconnect();
        let mut seen = 0;
        loop {
            match tokio::time::timeout(Duration::from_millis(250), events.recv()).await {
                Ok(Ok(CoreEvent::Connection {
                    state: ConnectionState::Disconnected,
                    ..
                })) => seen += 1,
                Ok(Ok(_)) => {}
                Ok(Err(_)) => break,
                Err(_) => break,
            }
        }
        assert_eq!(seen, 1);
        assert!(!mock.session.info().connected);
    }

    #[tokio::test]
    async fn a_wait_final_request_resolves_only_on_the_final_event() {
        let caps = MGMT_CAP_WIFI | MGMT_CAP_ASYNC_EVENTS;
        let mock = spawn_mock(TransportKind::Ble, caps, false);
        let mut events = mock.bus.subscribe();
        let reply = mock
            .session
            .request_mgmt("@w scan".to_string(), Some(Duration::from_secs(5)));
        assert!(
            wait_for(|| !frames(&mock).is_empty()).await,
            "the request was never written"
        );

        // A type-2 response resolves only `pending`; the FINAL waiter lives on
        // (PYTHON_CLI_SPEC 2.5: `if` response / `elif` FINAL).
        mock.hub.publish(TransportEvent::Data {
            channel: TransportChannel::MgmtResponse,
            bytes: mgmt_frame(2, 1, 0, b"scanning"),
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        let mut reply = reply;
        assert!(matches!(
            reply.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));

        // Intermediate events published while the caller waits are rendered on
        // the bus; the FINAL event is what lets the request resolve.
        mock.hub.publish(TransportEvent::Data {
            channel: TransportChannel::MgmtResponse,
            bytes: mgmt_frame(3, 1, 0, b"ERR none found"),
        });
        mock.hub.publish(TransportEvent::Data {
            channel: TransportChannel::MgmtResponse,
            bytes: mgmt_frame(3, 1, 0x0001, b"done"),
        });
        let result = reply.await.expect("reply channel").expect("final arrived");
        // Python `send()` returns the response body; the FINAL only gates how
        // long the call blocks.
        assert_eq!(result.lines, vec!["scanning".to_string()]);
        assert!(result.ok);

        let (mut saw_intermediate, mut saw_final) = (false, false);
        while !(saw_intermediate && saw_final) {
            match tokio::time::timeout(Duration::from_millis(500), events.recv()).await {
                Ok(Ok(CoreEvent::MgmtMessage {
                    kind: MgmtKind::Event,
                    final_,
                    lines,
                    ..
                })) => {
                    saw_intermediate |= !final_ && lines == ["ERR none found"];
                    saw_final |= final_ && lines == ["done"];
                }
                Ok(Ok(_)) => {}
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
                Ok(Err(_)) => break,
                Err(_) => panic!(
                    "the FINAL events never showed up (intermediate: {saw_intermediate}, final: {saw_final})"
                ),
            }
        }
        mock.session.disconnect();
    }
}
