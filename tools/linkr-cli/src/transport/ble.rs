//! Bluetooth LE transport (btleplug): scanning, connect, handshake reads,
//! subscriptions and writes. See specs/PYTHON_CLI_SPEC.md sections 3, 5, 8.

use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use btleplug::api::{
    Central as _, CentralEvent, Characteristic, Manager as _, Peripheral as _, ScanFilter,
    ValueNotification, WriteType,
};
use btleplug::platform::{Adapter, Peripheral};
use futures::StreamExt;
use uuid::{uuid, Uuid};

use super::{DiscoveredDevice, EventHub, ProtocolInfo, ReliableState, Transport};
use crate::protocol::mgmt::{MGMT_API_MAJOR, MGMT_CAP_DEVICE_ID, MGMT_CAP_RELIABLE_UART};
use crate::protocol::validate::{match_device, normalize_name_prefix, to_hex};

/// Primary advertisement UUID of the Management Service — scans filter on it,
/// never on the name (docs/LINKR_BLE_API.zh-CN.md section 2).
pub const MGMT_SERVICE_UUID: Uuid = uuid!("4c4b0001-9a7e-4f4e-8b8a-3d6f12a0c001");
pub const MGMT_PROTOCOL_UUID: Uuid = uuid!("4c4b0002-9a7e-4f4e-8b8a-3d6f12a0c001");
pub const MGMT_DEVICE_ID_UUID: Uuid = uuid!("4c4b0003-9a7e-4f4e-8b8a-3d6f12a0c001");
pub const MGMT_COMMAND_UUID: Uuid = uuid!("4c4b0004-9a7e-4f4e-8b8a-3d6f12a0c001");
pub const MGMT_RESPONSE_UUID: Uuid = uuid!("4c4b0005-9a7e-4f4e-8b8a-3d6f12a0c001");
pub const RELIABLE_UART_RX_UUID: Uuid = uuid!("4c4b0011-9a7e-4f4e-8b8a-3d6f12a0c001");
pub const RELIABLE_UART_TX_UUID: Uuid = uuid!("4c4b0012-9a7e-4f4e-8b8a-3d6f12a0c001");
pub const RELIABLE_UART_STATE_UUID: Uuid = uuid!("4c4b0013-9a7e-4f4e-8b8a-3d6f12a0c001");
/// NUS RX (write-only; Python's dead constant is kept for the auto write-size
/// rule and third-party parity).
pub const NUS_RX_UUID: Uuid = uuid!("6e400002-b5a3-f393-e0a9-e50e24dcca9e");
pub const NUS_TX_UUID: Uuid = uuid!("6e400003-b5a3-f393-e0a9-e50e24dcca9e");

const SCAN_POLL: Duration = Duration::from_millis(200);
/// BlueZ answers `org.bluez.Error.InProgress` while another scan or connection
/// owns the adapter — a second `linkr`, a phone app, or a session of ours that
/// never tore down. The CLI used to surface that raw (`linkr: error: In
/// Progress`), so back off and try again before giving up.
const BUSY_RETRIES: u32 = 3;
/// Backoff between those attempts: long enough for the holder to let go of the
/// adapter, short enough that three of them stay invisible next to a scan.
const BUSY_BACKOFF: Duration = Duration::from_millis(750);
/// The wall-clock budget every busy retry gets: three attempts (2.25 s), then
/// the real error.
const BUSY_BUDGET: Duration =
    Duration::from_millis(BUSY_BACKOFF.as_millis() as u64 * BUSY_RETRIES as u64);
/// How often to look for a link somebody else is dialling while we wait for it
/// (only `Connect` waits instead of re-dialling — see [`connect_with_retry`]).
const BUSY_POLL: Duration = Duration::from_millis(250);

/// `true` when a failure is BlueZ saying "someone else is using this" — the
/// D-Bus message (`In Progress`), the error name (`…Error.InProgress`) or the
/// same words in any casing.
fn is_busy(text: &str) -> bool {
    let lowered = text.to_ascii_lowercase();
    lowered.contains("in progress") || lowered.contains("inprogress")
}

/// BlueZ's transient `Connect` failures: the attempt died before any link
/// came up — another client won the race, the controller gave up, or the
/// accessory stopped answering mid-handshake. Nothing succeeded, so starting
/// over is safe. btleplug's `Not connected` belongs here too: the link died
/// between two calls (usually because the other instance released it), and
/// dialling again is the only fix. The permanent refusals
/// (`br-connection-rej-security`, `br-connection-params-rejected`, …) are
/// deliberately left out.
fn is_transient_connect(text: &str) -> bool {
    const TRANSIENT: [&str; 5] = [
        "br-connection-canceled",
        "br-connection-timeout",
        "br-connection-failed",
        "br-connection-adv-timeout",
        "Not connected",
    ];
    TRANSIENT.iter().any(|needle| text.contains(needle))
}

/// A busy failure gets a sentence a person can act on, and so does a link that
/// dropped while we were on it; every other failure keeps its original message
/// and source chain untouched.
fn adapter_error(error: btleplug::Error) -> anyhow::Error {
    let text = error.to_string();
    if is_busy(&text) {
        anyhow::anyhow!(
            "Bluetooth adapter is busy: {text} — another scan or connection is still \
             running; retry in a second"
        )
    } else if is_transient_connect(&text) {
        anyhow::anyhow!(
            "BLE link dropped: {text} — the accessory or another client let go of the \
             connection; retry in a second"
        )
    } else {
        anyhow::Error::new(error)
    }
}

/// Run `operation` while BlueZ reports a failure `retryable` recognises and
/// `budget` has not run out, back off between the attempts, then hand whatever
/// is left to [`adapter_error`].
///
/// `retryable` must only accept failures where the operation demonstrably did
/// **not** happen: `In Progress` (`org.bluez.Error.InProgress`) means BlueZ
/// never started it — a second `linkr`, a phone app or a session of ours that
/// never tore down holds the adapter — and the transient `br-connection-*`
/// answers mean the attempt never produced a link. Retrying those cannot
/// duplicate anything, which is why the CLI can now stand two instances
/// instead of failing the loser with a bare `linkr: error: In Progress`.
async fn retry_when<T, R, F, Fut>(
    budget: Duration,
    retryable: R,
    mut operation: F,
) -> anyhow::Result<T>
where
    R: Fn(&str) -> bool,
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, btleplug::Error>>,
{
    let deadline = tokio::time::Instant::now() + budget;
    loop {
        match operation().await {
            Ok(value) => return Ok(value),
            Err(error)
                if retryable(&error.to_string()) && tokio::time::Instant::now() < deadline =>
            {
                tokio::time::sleep(BUSY_BACKOFF).await;
            }
            Err(error) => return Err(adapter_error(error)),
        }
    }
}

/// [`retry_when`] for everything that must stay busy-only: a write, read or
/// notify that already landed must never be replayed.
async fn retry_busy<T, F, Fut>(operation: F) -> anyhow::Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, btleplug::Error>>,
{
    retry_when(BUSY_BUDGET, is_busy, operation).await
}

/// `StartScan` with that backoff (3 × 750 ms before the real error).
async fn start_scan_with_retry(adapter: &Adapter) -> anyhow::Result<()> {
    retry_busy(|| async {
        adapter
            .start_scan(ScanFilter {
                services: vec![MGMT_SERVICE_UUID],
            })
            .await
    })
    .await
}

/// `Connect`, with the rule that stopped two instances of the CLI from killing
/// each other's link:
///
/// * a link that is already up (ours, or a second `linkr` that got there
///   first) is joined instead of dialled again;
/// * `In Progress` means another client is dialling right now — and *our*
///   `Connect` request is exactly what makes BlueZ cancel theirs — so we stop
///   asking and watch for their link instead, joining it when it appears;
/// * the transient `br-connection-*` answers mean our own attempt died, so the
///   leftovers are cleared and we dial again within the budget;
/// * a permanent refusal (or a budget that ran out) fails at once, worded by
///   [`adapter_error`] instead of BlueZ's raw status.
async fn connect_with_retry(peripheral: &Peripheral, timeout: Duration) -> anyhow::Result<()> {
    let deadline = tokio::time::Instant::now() + BUSY_BUDGET;
    loop {
        if link_is_up(peripheral).await {
            return Ok(());
        }
        let error = match peripheral.connect_with_timeout(timeout).await {
            Ok(()) => return Ok(()),
            Err(error) => error,
        };
        let text = error.to_string();
        if is_transient_connect(&text) && tokio::time::Instant::now() < deadline {
            // Best effort: BlueZ answers `Not Connected` when there is nothing
            // to clear.
            let _ = peripheral.disconnect().await;
            tokio::time::sleep(BUSY_BACKOFF).await;
            continue;
        }
        if is_busy(&text) {
            while tokio::time::Instant::now() < deadline {
                tokio::time::sleep(BUSY_POLL).await;
                if link_is_up(peripheral).await {
                    return Ok(());
                }
            }
        }
        return Err(adapter_error(error));
    }
}

/// `true` when BlueZ already reports a link to the accessory — whether this
/// process owns it or another `linkr` does.
async fn link_is_up(peripheral: &Peripheral) -> bool {
    peripheral.is_connected().await.unwrap_or(false)
}

/// A GATT write with the same backoff: a busy chunk is what used to kill the
/// very first management command of a second instance.
async fn write_with_retry(
    peripheral: &Peripheral,
    characteristic: &Characteristic,
    data: &[u8],
    write_type: WriteType,
) -> anyhow::Result<()> {
    retry_busy(|| async { peripheral.write(characteristic, data, write_type).await }).await
}

/// `StartNotify` with the same backoff (a pending notification setup from
/// another client answers `In Progress` too).
async fn subscribe_with_retry(
    peripheral: &Peripheral,
    characteristic: &Characteristic,
) -> anyhow::Result<()> {
    retry_busy(|| async { peripheral.subscribe(characteristic).await }).await
}

/// `ReadValue` with the same backoff (reads are idempotent).
async fn read_with_retry(
    peripheral: &Peripheral,
    characteristic: &Characteristic,
) -> anyhow::Result<Vec<u8>> {
    retry_busy(|| async { peripheral.read(characteristic).await }).await
}

/// `DiscoverServices` with the same backoff (it just re-runs discovery).
async fn discover_with_retry(peripheral: &Peripheral) -> anyhow::Result<()> {
    retry_busy(|| async { peripheral.discover_services().await }).await
}

async fn default_adapter() -> anyhow::Result<Adapter> {
    let manager = btleplug::platform::Manager::new().await?;
    let adapters = manager.adapters().await?;
    adapters
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("no Bluetooth adapter found"))
}

/// Discover peripherals advertising the Management Service until `done` says
/// stop or `timeout` runs out.
async fn scan_until(
    adapter: &Adapter,
    timeout: Duration,
    mut done: impl FnMut(&[Peripheral]) -> bool,
) -> anyhow::Result<Vec<Peripheral>> {
    start_scan_with_retry(adapter).await?;
    let deadline = tokio::time::Instant::now() + timeout;
    // The loop only leaves through the break below, so `found` is always set.
    let mut found: Vec<Peripheral>;
    loop {
        found = adapter.peripherals().await?;
        if done(&found) || tokio::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(SCAN_POLL).await;
    }
    let _ = adapter.stop_scan().await;
    Ok(found)
}

fn is_zero_address(peripheral: &Peripheral) -> bool {
    peripheral.address().to_string() == "00:00:00:00:00:00"
}

/// The address string users see and match against: the MAC on Linux/Windows,
/// the peripheral id (a UUID) on platforms where no MAC exists.
fn display_address(peripheral: &Peripheral) -> String {
    if is_zero_address(peripheral) {
        peripheral.id().to_string()
    } else {
        peripheral.address().to_string()
    }
}

fn matches_address(peripheral: &Peripheral, target: &str) -> bool {
    peripheral
        .address()
        .to_string()
        .eq_ignore_ascii_case(target)
        || peripheral.id().to_string().eq_ignore_ascii_case(target)
}

async fn to_devices(peripherals: &[Peripheral]) -> Vec<DiscoveredDevice> {
    let mut devices = Vec::new();
    for peripheral in peripherals {
        let properties = peripheral.properties().await.ok().flatten();
        if let Some(properties) = &properties {
            // The scan was already filtered by UUID; ignore (parts of) the
            // filter only when the platform reported a service list without
            // the Management Service in it.
            if !properties.services.is_empty() && !properties.services.contains(&MGMT_SERVICE_UUID)
            {
                continue;
            }
        }
        let name = properties.as_ref().and_then(|properties| {
            properties
                .local_name
                .clone()
                .or_else(|| properties.advertisement_name.clone())
        });
        let rssi = properties
            .as_ref()
            .and_then(|properties| properties.rssi)
            .map(i32::from);
        devices.push(DiscoveredDevice {
            address: display_address(peripheral),
            name,
            rssi,
        });
    }
    devices
}

/// Sort key of `--scan` output: unnamed devices last (`\u{ffff}`), otherwise
/// case-insensitive name, then address.
pub fn device_sort_key(device: &DiscoveredDevice) -> (String, String) {
    let name = device
        .name
        .clone()
        .unwrap_or_else(|| "\u{ffff}".to_string());
    (name.to_lowercase(), device.address.clone())
}

/// One `--scan` line: `address\tname\tsignal`.
pub fn format_scan_line(device: &DiscoveredDevice) -> String {
    let name = device.name.as_deref().unwrap_or("(unknown)");
    let signal = match device.rssi {
        Some(rssi) => format!("{rssi} dBm"),
        None => "-".to_string(),
    };
    format!("{}\t{}\t{}", device.address, name, signal)
}

/// Sort a `--scan` listing the way the Python CLI prints it.
pub fn sort_for_display(devices: &mut [DiscoveredDevice]) {
    // `sort_by_cached_key`: the key clones and lowercases the name, and
    // `sort_by_key` would rebuild it for every comparison in the sort.
    devices.sort_by_cached_key(device_sort_key);
}

/// Scan for Linkr accessories, filtered by the Management Service UUID.
pub async fn scan(timeout: Duration) -> anyhow::Result<Vec<DiscoveredDevice>> {
    let adapter = default_adapter().await?;
    let peripherals = scan_until(&adapter, timeout, |_| false).await?;
    Ok(to_devices(&peripherals).await)
}

/// Everything the session reads once right after connecting.
pub struct BleHandshake {
    pub protocol: ProtocolInfo,
    pub device_id: [u8; 16],
    pub reliable: ReliableState,
    pub write_size: usize,
    /// Device name (or address when the advertisement carries none) — the
    /// label shown in `SessionInfo`.
    pub label: String,
}

fn find_characteristic(peripheral: &Peripheral, uuid: Uuid) -> Option<Characteristic> {
    peripheral
        .characteristics()
        .into_iter()
        .find(|characteristic| characteristic.uuid == uuid)
}

fn missing(uuid: Uuid) -> anyhow::Error {
    anyhow::anyhow!("device is missing GATT characteristic {uuid}")
}

/// `configure_ble_write_size` from PYTHON_CLI_SPEC section 5.2. btleplug does
/// not expose the characteristic's "max write without response" size, so the
/// `reported` step falls through to the negotiated MTU exactly like the
/// Python fallback does on platforms without the property.
fn configure_write_size(peripheral: &Peripheral, requested: usize) -> usize {
    if requested > 0 {
        let size = requested.min(244);
        crate::cli::info(format!("BLE write chunk size: {size} bytes (manual)"));
        return size;
    }
    let mtu = peripheral.mtu();
    let size = if mtu > 3 { usize::from(mtu - 3) } else { 0 };
    let size = max20_min244(if size != 0 { size } else { 20 });
    crate::cli::info(format!("BLE write chunk size: {size} bytes"));
    size
}

fn max20_min244(size: usize) -> usize {
    size.clamp(20, 244)
}

/// Connect to `address` (or match `name` when `address` is `None`), complete
/// the handshake reads and subscribe to management + reliable UART indications.
///
/// `pair` is the `--pair` flag: macOS pairs while reading the encrypted
/// service, so the only action is the hint; everywhere else btleplug 0.13
/// exposes no `pair()` and bonding happens through the same encrypted read.
pub async fn connect(
    name: &str,
    address: Option<&str>,
    timeout: Duration,
    write_size_override: usize,
    pair: bool,
) -> anyhow::Result<(Arc<dyn Transport>, BleHandshake)> {
    let adapter = default_adapter().await?;

    // 1. Resolve the address to connect to (PYTHON_CLI_SPEC 8.5).
    let target = match address {
        Some(address) => address.to_string(),
        None => {
            let peripherals = scan_until(&adapter, timeout, |_| false).await?;
            let devices = to_devices(&peripherals).await;
            let pairs: Vec<(Option<String>, String)> = devices
                .iter()
                .map(|device| (device.name.clone(), device.address.clone()))
                .collect();
            match match_device(&pairs, name) {
                Some(address) => address,
                None => {
                    return Err(anyhow::anyhow!(
                        "device not found matching: {}*",
                        normalize_name_prefix(name)
                    ))
                }
            }
        }
    };

    // 2. Locate the peripheral: use whatever is already known, scan only when
    //    it is missing (an `--scan` just before this usually cached it).
    let mut located = find_peripheral(&adapter, &target).await?;
    if located.is_none() {
        scan_until(&adapter, timeout, |peripherals| {
            peripherals.iter().any(|p| matches_address(p, &target))
        })
        .await?;
        located = find_peripheral(&adapter, &target).await?;
    }
    let peripheral =
        located.ok_or_else(|| anyhow::anyhow!("device not found matching: {target}"))?;

    connect_with_retry(&peripheral, timeout).await?;
    crate::cli::info(format!("connected: {target}"));
    if pair && cfg!(target_os = "macos") {
        crate::cli::info(
            "macOS requests pairing when the encrypted service is read; accept the system dialog.",
        );
    }
    // Python calls `client.pair()` here; btleplug 0.13 has no pairing
    // call, and macOS/Linux both bond on the encrypted read below.
    discover_with_retry(&peripheral).await?;

    // 3. Write size, then the handshake reads in the documented order
    //    (docs/LINKR_BLE_API.zh-CN.md section 9).
    let write_size = configure_write_size(&peripheral, write_size_override);

    // Create the notification stream before subscribing so an indication that
    // races the second subscribe cannot be lost.
    let notifications = peripheral.notifications().await?;

    let protocol_char = find_characteristic(&peripheral, MGMT_PROTOCOL_UUID)
        .ok_or_else(|| missing(MGMT_PROTOCOL_UUID))?;
    let protocol = read_with_retry(&peripheral, &protocol_char).await?;
    if protocol.len() < 10 || protocol[0] != MGMT_API_MAJOR {
        return Err(anyhow::anyhow!("unsupported Linkr Management API version"));
    }
    let management_max_payload = u16::from_le_bytes([protocol[2], protocol[3]]);
    let capabilities = u32::from_le_bytes([protocol[4], protocol[5], protocol[6], protocol[7]]);
    if management_max_payload == 0 {
        return Err(anyhow::anyhow!("invalid Linkr Management payload limit"));
    }
    if capabilities & MGMT_CAP_DEVICE_ID == 0 {
        return Err(anyhow::anyhow!(
            "device does not advertise Device ID support"
        ));
    }
    if capabilities & MGMT_CAP_RELIABLE_UART == 0 {
        return Err(anyhow::anyhow!(
            "device does not advertise Reliable UART support"
        ));
    }

    let device_id_char = find_characteristic(&peripheral, MGMT_DEVICE_ID_UUID)
        .ok_or_else(|| missing(MGMT_DEVICE_ID_UUID))?;
    let device_id = read_with_retry(&peripheral, &device_id_char).await?;
    if device_id.len() != 16 {
        return Err(anyhow::anyhow!("invalid Linkr Device ID length"));
    }
    crate::cli::info(format!(
        "management API v{}.{}, device ID {}",
        protocol[0],
        protocol[1],
        to_hex(&device_id)
    ));

    let response_char = find_characteristic(&peripheral, MGMT_RESPONSE_UUID)
        .ok_or_else(|| missing(MGMT_RESPONSE_UUID))?;
    subscribe_with_retry(&peripheral, &response_char).await?;

    let state_char = find_characteristic(&peripheral, RELIABLE_UART_STATE_UUID)
        .ok_or_else(|| missing(RELIABLE_UART_STATE_UUID))?;
    let state = read_with_retry(&peripheral, &state_char).await?;
    if state.len() != 16 || state[0] != 1 {
        return Err(anyhow::anyhow!("unsupported Reliable UART version"));
    }
    let reliable_max_payload = u16::from_le_bytes([state[2], state[3]]);
    let tx_sequence = u32::from_le_bytes([state[4], state[5], state[6], state[7]]);
    let rx_sequence = u32::from_le_bytes([state[8], state[9], state[10], state[11]]);
    if reliable_max_payload == 0 || tx_sequence == 0 || rx_sequence == 0 {
        return Err(anyhow::anyhow!("invalid Reliable UART state"));
    }

    let uart_tx_char = find_characteristic(&peripheral, RELIABLE_UART_TX_UUID)
        .ok_or_else(|| missing(RELIABLE_UART_TX_UUID))?;
    subscribe_with_retry(&peripheral, &uart_tx_char).await?;

    let label = peripheral
        .properties()
        .await
        .ok()
        .flatten()
        .and_then(|properties| properties.local_name.or(properties.advertisement_name))
        .unwrap_or_else(|| target.clone());

    // 4. Indication pump.
    let hub = Arc::new(EventHub::new());
    tokio::spawn(pump(
        peripheral.clone(),
        adapter,
        notifications,
        hub.clone(),
    ));

    let command_char = find_characteristic(&peripheral, MGMT_COMMAND_UUID)
        .ok_or_else(|| missing(MGMT_COMMAND_UUID))?;
    let uart_rx_char = find_characteristic(&peripheral, RELIABLE_UART_RX_UUID)
        .ok_or_else(|| missing(RELIABLE_UART_RX_UUID))?;

    let transport = BleTransport {
        peripheral,
        command: command_char,
        uart_rx: uart_rx_char,
        write_size,
        hub,
    };
    let handshake = BleHandshake {
        protocol: ProtocolInfo {
            major: protocol[0],
            minor: protocol[1],
            max_payload: management_max_payload,
            capabilities,
        },
        device_id: device_id.as_slice().try_into().expect("16 bytes checked"),
        reliable: ReliableState {
            version: state[0],
            flags: state[1],
            max_payload: reliable_max_payload,
            tx_sequence,
            rx_sequence,
        },
        write_size,
        label,
    };
    Ok((Arc::new(transport), handshake))
}

async fn find_peripheral(adapter: &Adapter, target: &str) -> anyhow::Result<Option<Peripheral>> {
    for peripheral in adapter.peripherals().await? {
        if matches_address(&peripheral, target) {
            return Ok(Some(peripheral));
        }
    }
    Ok(None)
}

async fn pump(
    peripheral: Peripheral,
    adapter: Adapter,
    mut notifications: Pin<Box<dyn futures::Stream<Item = ValueNotification> + Send>>,
    hub: Arc<EventHub>,
) {
    hub.publish(super::TransportEvent::Subscribed);
    let mut central_events = match adapter.events().await {
        Ok(stream) => stream,
        Err(_) => return,
    };
    let mut central_alive = true;
    loop {
        tokio::select! {
            item = notifications.next() => match item {
                None => {
                    hub.publish(super::TransportEvent::Disconnected {
                        reason: "BLE disconnected".to_string(),
                    });
                    return;
                }
                Some(notification) => {
                    let channel = if notification.uuid == MGMT_RESPONSE_UUID {
                        super::TransportChannel::MgmtResponse
                    } else if notification.uuid == RELIABLE_UART_TX_UUID {
                        super::TransportChannel::UartTx
                    } else if notification.uuid == NUS_TX_UUID {
                        super::TransportChannel::NusTx
                    } else {
                        continue;
                    };
                    hub.publish(super::TransportEvent::Data {
                        channel,
                        bytes: notification.value,
                    });
                }
            },
            event = central_events.next(), if central_alive => match event {
                None => central_alive = false,
                Some(CentralEvent::DeviceDisconnected(id)) if id == peripheral.id() => {
                    hub.publish(super::TransportEvent::Disconnected {
                        reason: "BLE disconnected".to_string(),
                    });
                    return;
                }
                Some(_) => {}
            },
        }
    }
}

struct BleTransport {
    peripheral: Peripheral,
    command: Characteristic,
    uart_rx: Characteristic,
    write_size: usize,
    hub: Arc<EventHub>,
}

#[async_trait::async_trait]
impl Transport for BleTransport {
    fn kind(&self) -> super::TransportKind {
        super::TransportKind::Ble
    }

    async fn write_mgmt(&self, chunk: &[u8]) -> anyhow::Result<()> {
        // BLE management writes are always write-with-response.
        write_with_retry(
            &self.peripheral,
            &self.command,
            chunk,
            WriteType::WithResponse,
        )
        .await
    }

    async fn write_uart(&self, chunk: &[u8]) -> anyhow::Result<()> {
        write_with_retry(
            &self.peripheral,
            &self.uart_rx,
            chunk,
            WriteType::WithResponse,
        )
        .await
    }

    async fn disconnect(&self) -> anyhow::Result<()> {
        self.peripheral.disconnect().await.map_err(Into::into)
    }

    fn write_size(&self) -> usize {
        self.write_size
    }

    fn events(&self) -> tokio::sync::broadcast::Receiver<super::TransportEvent> {
        self.hub.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(address: &str, name: Option<&str>, rssi: Option<i32>) -> DiscoveredDevice {
        DiscoveredDevice {
            address: address.to_string(),
            name: name.map(str::to_string),
            rssi,
        }
    }

    #[test]
    fn scan_line_matches_the_python_format() {
        let named = device("AA:BB:CC:DD:EE:FF", Some("Linkr BLE UART-3"), Some(-58));
        assert_eq!(
            format_scan_line(&named),
            "AA:BB:CC:DD:EE:FF\tLinkr BLE UART-3\t-58 dBm"
        );
        let anonymous = device("11:22:33:44:55:66", None, None);
        assert_eq!(
            format_scan_line(&anonymous),
            "11:22:33:44:55:66\t(unknown)\t-"
        );
    }

    #[test]
    fn scan_sort_puts_unnamed_devices_last() {
        let mut devices = vec![
            device("00:00:00:00:00:03", None, None),
            device("00:00:00:00:00:02", Some("bee"), None),
            device("00:00:00:00:00:01", Some("Bee"), None),
        ];
        sort_for_display(&mut devices);
        let order: Vec<&str> = devices
            .iter()
            .map(|device| device.address.as_str())
            .collect();
        // "Bee" and "bee" lowercase to the same key; the address breaks ties.
        assert_eq!(
            order,
            vec![
                "00:00:00:00:00:01",
                "00:00:00:00:00:02",
                "00:00:00:00:00:03"
            ]
        );
    }

    #[test]
    fn scan_sort_keeps_named_devices_before_unnamed_ones() {
        let mut devices = vec![
            device("00:00:00:00:00:09", None, None),
            device("00:00:00:00:00:01", Some("zzz"), None),
        ];
        sort_for_display(&mut devices);
        assert_eq!(devices[0].name.as_deref(), Some("zzz"));
        assert!(devices[1].name.is_none());
    }

    #[test]
    fn device_sort_key_uses_the_unicode_max_name() {
        let unnamed = device("00:01", None, None);
        assert_eq!(device_sort_key(&unnamed).0, "\u{ffff}");
        let named = device("00:02", Some("Linkr BLE UART"), None);
        assert_eq!(device_sort_key(&named).0, "linkr ble uart");
    }

    #[test]
    fn write_size_auto_rule_follows_the_python_clamps() {
        // btleplug reports the negotiated MTU (or the 23-byte default).
        assert_eq!(max20_min244(23 - 3), 20);
        assert_eq!(max20_min244(247 - 3), 244);
        assert_eq!(max20_min244(5), 20);
        assert_eq!(max20_min244(509), 244);
        assert_eq!(max20_min244(20), 20);
    }

    #[test]
    fn scan_filter_uses_the_management_service_uuid() {
        assert_eq!(
            MGMT_SERVICE_UUID.to_string(),
            "4c4b0001-9a7e-4f4e-8b8a-3d6f12a0c001"
        );
        assert_eq!(
            MGMT_RESPONSE_UUID.to_string(),
            "4c4b0005-9a7e-4f4e-8b8a-3d6f12a0c001"
        );
        assert_eq!(
            RELIABLE_UART_STATE_UUID.to_string(),
            "4c4b0013-9a7e-4f4e-8b8a-3d6f12a0c001"
        );
        assert_eq!(
            NUS_RX_UUID.to_string(),
            "6e400002-b5a3-f393-e0a9-e50e24dcca9e"
        );
    }

    /// B1: `org.bluez.Error.InProgress` (second linkr holding the adapter) has
    /// to be recognised in every spelling BlueZ and the D-Bus layer produce.
    #[test]
    fn busy_failures_are_recognised_in_every_spelling() {
        assert!(is_busy("In Progress"), "the D-Bus message");
        assert!(is_busy("org.bluez.Error.InProgress"), "the error name");
        assert!(is_busy("IN PROGRESS"), "casing");
        assert!(!is_busy("device not found matching: AA:BB:CC:DD:EE:FF"));
        assert!(!is_busy("Timed out after 8s"));
        assert!(!is_busy(""));
    }

    /// …and only those get the extra sentence; every other failure keeps the
    /// message (and the chain) the transport produced.
    #[test]
    fn only_busy_failures_gain_the_adapter_hint() {
        let busy = adapter_error(btleplug::Error::Other("In Progress".into()));
        let busy = busy.to_string();
        assert!(busy.contains("Bluetooth adapter is busy"), "{busy}");
        assert!(busy.contains("In Progress"), "{busy}");
        assert!(busy.contains("retry in a second"), "{busy}");

        // `Not connected` is the one *transient* failure that is not a BlueZ
        // status string, so it gets the same readable wording.
        let dropped_link = adapter_error(btleplug::Error::NotConnected).to_string();
        assert!(dropped_link.contains("BLE link dropped"), "{dropped_link}");
        assert!(dropped_link.contains("Not connected"), "{dropped_link}");

        let other = adapter_error(btleplug::Error::DeviceNotFound);
        assert_eq!(other.to_string(), "Device not found");
    }

    /// B1, second family: BlueZ cancels the *loser's* `Connect` outright when
    /// two instances dial the same accessory (`br-connection-canceled`), and
    /// that one has to be retried too — while a permanent refusal must not be.
    #[test]
    fn transient_link_drops_are_retried_and_explained() {
        assert!(is_transient_connect("br-connection-canceled"));
        assert!(is_transient_connect("br-connection-timeout"));
        assert!(is_transient_connect("br-connection-failed"));
        assert!(is_transient_connect("br-connection-adv-timeout"));
        assert!(
            is_transient_connect("Not connected"),
            "the link died mid-run"
        );
        assert!(!is_transient_connect("br-connection-rej-security"));
        assert!(!is_transient_connect("In Progress"));
        assert!(!is_transient_connect("device not found matching: AA:BB"));

        // …and after the retries run out the message is still readable.
        let dropped = adapter_error(btleplug::Error::Other("br-connection-canceled".into()));
        let dropped = dropped.to_string();
        assert!(dropped.contains("BLE link dropped"), "{dropped}");
        assert!(dropped.contains("br-connection-canceled"), "{dropped}");
        assert!(dropped.contains("retry in a second"), "{dropped}");

        let busy = adapter_error(btleplug::Error::Other("In Progress".into())).to_string();
        assert!(busy.contains("Bluetooth adapter is busy"), "{busy}");

        // A permanent refusal keeps BlueZ's own words.
        let refused = adapter_error(btleplug::Error::Other("br-connection-rej-security".into()));
        assert_eq!(refused.to_string(), "br-connection-rej-security");
    }
}
