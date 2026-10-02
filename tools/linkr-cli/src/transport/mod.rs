//! Transports: Bluetooth LE (btleplug) and the LAN WebSocket bridge.

pub mod ble;
pub mod lan;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportKind {
    Ble,
    Lan,
}

#[derive(Debug, Clone)]
pub enum TransportEvent {
    /// Indication/write payload from a characteristic.
    Data {
        channel: TransportChannel,
        bytes: Vec<u8>,
    },
    Subscribed,
    Disconnected {
        reason: String,
    },
    /// Text frame received outside the LAN access handshake. The bridge uses
    /// binary frames for serial bytes, so a text frame after `@ws auth=ok`
    /// carries something the user should see as a notice.
    Text(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportChannel {
    MgmtResponse,
    UartTx,
    /// NUS TX notification (only used where NUS is subscribed).
    NusTx,
}

/// A connected transport. Writes are raw ATT chunks (BLE) or raw WebSocket
/// binary frames (LAN); framing lives in `protocol`.
#[async_trait::async_trait]
pub trait Transport: Send + Sync + 'static {
    fn kind(&self) -> TransportKind;
    /// Management channel write (BLE: write-with-response). LAN returns an error.
    async fn write_mgmt(&self, chunk: &[u8]) -> anyhow::Result<()>;
    /// UART channel write. LAN sends the bytes as one binary frame.
    async fn write_uart(&self, chunk: &[u8]) -> anyhow::Result<()>;
    async fn disconnect(&self) -> anyhow::Result<()>;
    /// ATT chunk size for UART writes (`--ble-write-size` override or auto).
    fn write_size(&self) -> usize;
    /// Receiver for this transport's event stream (indications, text frames,
    /// disconnects). The session subscribes once, right after connecting.
    /// The default keeps hypothetical external implementations compiling; the
    /// bundled transports always override it.
    fn events(&self) -> tokio::sync::broadcast::Receiver<TransportEvent> {
        let (tx, rx) = tokio::sync::broadcast::channel(1);
        drop(tx);
        rx
    }
}

#[derive(Debug, Clone)]
pub struct DiscoveredDevice {
    pub address: String,
    pub name: Option<String>,
    pub rssi: Option<i32>,
}

/// Parsed Management Protocol Info characteristic (10 bytes).
#[derive(Debug, Clone, Copy)]
pub struct ProtocolInfo {
    pub major: u8,
    pub minor: u8,
    pub max_payload: u16,
    pub capabilities: u32,
}

/// Parsed Reliable UART State characteristic (16 bytes).
#[derive(Debug, Clone, Copy)]
pub struct ReliableState {
    pub version: u8,
    pub flags: u8,
    pub max_payload: u16,
    pub tx_sequence: u32,
    pub rx_sequence: u32,
}

/// Fan-out hub for [`TransportEvent`]s.
///
/// Transports start their receive pump the moment they are connected, but the
/// session subscribes only after the handshake reads finished. Events emitted
/// in that window are buffered here and delivered to the first subscriber, so
/// a disconnect or an early indication cannot fall through the gap.
pub struct EventHub {
    tx: tokio::sync::broadcast::Sender<TransportEvent>,
    pending: std::sync::Mutex<Vec<TransportEvent>>,
}

impl EventHub {
    pub fn new() -> Self {
        let (tx, _) = tokio::sync::broadcast::channel(256);
        Self {
            tx,
            pending: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Deliver `event` to current subscribers, or buffer it for the first one.
    pub fn publish(&self, event: TransportEvent) {
        let mut pending = self.pending.lock().expect("event hub poisoned");
        if self.tx.send(event.clone()).is_err() {
            pending.push(event);
        }
    }

    /// Subscribe, then flush every event buffered before this point.
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<TransportEvent> {
        let mut pending = self.pending.lock().expect("event hub poisoned");
        let rx = self.tx.subscribe();
        for event in pending.drain(..) {
            let _ = self.tx.send(event);
        }
        rx
    }
}

impl Default for EventHub {
    fn default() -> Self {
        Self::new()
    }
}
