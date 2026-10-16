//! Bluetooth Low Energy, speaking the Nordic UART Service.
//!
//! This is the link the product actually ships on: the Linkr Bee is an ESP32
//! that advertises one GATT profile and nothing else, and the bytes a user
//! types go down a single characteristic while the target's reply comes back
//! up another. There is no framing and no handshake on top — the radio is a
//! cable with a discoverable name, which is the entire point of the device.
//!
//! Two things about BLE shape this file more than the protocol does.
//!
//! **A radio needs a reactor, and the interface does not have one.** Every
//! GATT library worth using is async; `App` is a synchronous loop on a 250 ms
//! beat, deliberately, because half-drawn frames are worse than a late byte.
//! Rather than making the whole interface async — which would put every view,
//! every dialog and every test behind a runtime — the link builds its own
//! Tokio reactor on a thread of its own and meets the interface at a channel.
//! [`LinkReader::read`] is allowed to block, and it is called on a thread
//! dedicated to reading, so this costs nothing and changes nothing upstream.
//!
//! **Writes are sized, not streamed.** An ATT write is a single packet whose
//! size was agreed when the connection was made, and a payload longer than
//! that is refused rather than carried. So the write side segments every
//! payload into [`MTU_WRITE_LIMIT`] pieces. That number is deliberately
//! conservative: asking the stack for the negotiated MTU is possible, but an
//! answer that is wrong by a few bytes fails at every write rather than
//! nowhere, so a fixed ceiling we know fits is the safer default.

use std::collections::VecDeque;
use std::io;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use btleplug::api::{Central, Manager as _, Peripheral as _, ScanFilter, WriteType};
use btleplug::platform::{Manager, Peripheral};
use futures::StreamExt;
use uuid::Uuid;

use super::{Link, LinkKind, LinkReader, LinkWriter};

/// The Nordic UART Service, and the two characteristics it is made of.
///
/// These are the numbers in every NUS implementation going; they are written
/// out in full rather than built from a 16-bit base because they are not
/// short form — there is no `0x0001` shortcut for this one — and a constant
/// that looks derived invites someone to "simplify" it.
pub const NUS_SERVICE: Uuid = Uuid::from_u128(0x6e400001_b5a3_f393_e0a9_e50e24dcca9e);
/// What this host writes: bytes going out to the target.
pub const NUS_RX: Uuid = Uuid::from_u128(0x6e400002_b5a3_f393_e0a9_e50e24dcca9e);
/// What this host subscribes to: bytes coming in from the target.
pub const NUS_TX: Uuid = Uuid::from_u128(0x6e400003_b5a3_f393_e0a9_e50e24dcca9e);

/// These five belong to the bridge's **management** service, and each is its
/// own characteristic rather than another byte inside Nordic UART.
///
/// That separation is the whole design. A management question — what firmware
/// is this, what speed is its UART running, has it joined a network — has to
/// be answered by the bridge *while the target keeps printing*, and an answer
/// delivered down the same pipe as the target's output could be told apart
/// from it only by guessing at the shape of the bytes. Guessing is exactly
/// what a log line containing `wifi=` would defeat. The reference client
/// (`tools/linkr_ble_terminal.py`) speaks to these same five.
pub const MGMT_SERVICE: Uuid = Uuid::from_u128(0x4c4b0001_9a7e_4f4e_8b8a_3d6f12a0c001);
/// Read: the negotiated protocol version, size limits and capability bits.
pub const MGMT_PROTOCOL: Uuid = Uuid::from_u128(0x4c4b0002_9a7e_4f4e_8b8a_3d6f12a0c001);
/// Read: this board's stable identifier.
pub const MGMT_DEVICE_ID: Uuid = Uuid::from_u128(0x4c4b0003_9a7e_4f4e_8b8a_3d6f12a0c001);
/// Write: one management request, framed as `protocol` encodes it.
pub const MGMT_COMMAND: Uuid = Uuid::from_u128(0x4c4b0004_9a7e_4f4e_8b8a_3d6f12a0c001);
/// Notify: the responses, as one `protocol::ManagementReassembler` stream.
pub const MGMT_RESPONSE: Uuid = Uuid::from_u128(0x4c4b0005_9a7e_4f4e_8b8a_3d6f12a0c001);

/// Ceiling for one ATT write.
///
/// 250 is the largest attribute value BLE 4.2 can carry and 3 bytes of that
/// is write-command overhead. Every link the device opens negotiates at or
/// below 247, so 244 leaves room without ever asking. See the module note.
pub const MTU_WRITE_LIMIT: usize = 244;

/// A Bluetooth address is twelve hexadecimal digits, as in `EE:C7:42:34:48:CF`.
///
/// Twelve is a floor and not the expected length, because CoreBluetooth
/// answers with a UUID instead, which is longer — but anything shorter is not
/// an address. The check matters more than it looks: "not-an-address" contains
/// `a`, `d` and `e`, all of which are hexadecimal, so a rule that merely asked
/// "does this contain any digits at all" would accept it and spend ten
/// seconds scanning.
const ADDRESS_DIGITS: usize = 12;

/// How long to listen before deciding nothing is advertising.
///
/// Ten seconds is long enough to catch a device that advertises on a slow
/// interval — and these bridge firmwares do, to save power — and short enough
/// that "wrong board" is answered rather than waited out.
const SCAN_TIMEOUT: Duration = Duration::from_secs(10);

/// How long to wait for the connection to complete once one is chosen.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

/// How much data may be waiting to go out before the sender blocks.
///
/// Small on purpose. This channel is the only place write back-pressure can
/// be felt, and a deep one would let the interface queue a megabyte of typed
/// characters towards a device that has stopped listening.
const WRITE_BACKLOG: usize = 64;

/// Which device on the air to connect to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    /// The first device advertising the Nordic UART Service. Right for a
    /// bench with one board on it, and the reason a desk with two is a
    /// problem to be named rather than guessed at.
    Any,
    /// Connect to the device whose advertised name contains this. Case is
    /// ignored, because names differ in it between firmware revisions of
    /// the same board.
    Name(String),
    /// Connect to the address printed by `bluetoothctl` as
    /// `EE:C7:42:34:48:CF`. Colons and case are ignored so a value copied
    /// out of a log works as it stands.
    Address(String),
}

impl Selection {
    /// Nothing filled in means "whichever one you find".
    pub fn any() -> Selection {
        Selection::Any
    }

    /// How the sidebar and the notices finish the sentence "ble:" with.
    pub fn label(&self) -> String {
        match self {
            Selection::Any => "find the nearest device".to_string(),
            Selection::Name(name) => name.clone(),
            Selection::Address(address) => address.to_ascii_uppercase(),
        }
    }

    /// Whether a device could ever be dialled with this.
    ///
    /// This is the cheap refusal, asked before anything touches BlueZ. It
    /// exists because an address that cannot be parsed must not be treated as
    /// "scan for ten seconds and find nothing" — that is indistinguishable
    /// from the board being switched off, which sends the user looking at the
    /// wrong thing.
    pub fn reachable(&self) -> Result<(), String> {
        match self {
            Selection::Address(address) => {
                let digits = normalise_address(address);
                if digits.len() < ADDRESS_DIGITS {
                    return Err(format!(
                        "“{address}” is not a Bluetooth address; those are twelve \
                         hexadecimal digits, as in EE:C7:42:34:48:CF"
                    ));
                }
                Ok(())
            }
            Selection::Name(name) if name.trim().is_empty() => Err(
                "a device was named by nothing — give --ble-name something to look for".to_string(),
            ),
            _ => Ok(()),
        }
    }

    /// Whether a candidate is the one being asked for.
    ///
    /// Split out from the async scan so the rules can be asserted without a
    /// radio. A device is named several ways depending on the platform — a
    /// BlueZ identifier is the MAC, a CoreBluetooth one is a UUID that says
    /// nothing about the address — so both are offered and any match counts.
    fn accepts(&self, name: Option<&str>, id: &str, address: Option<&str>) -> bool {
        match self {
            Selection::Any => true,
            Selection::Name(want) => {
                let want = want.to_ascii_lowercase();
                name.is_some_and(|found| found.to_ascii_lowercase().contains(&want))
            }
            Selection::Address(want) => {
                let want = normalise_address(want);
                // An empty filter would otherwise match every candidate and
                // silently dial whichever was seen first, which is how a typo
                // turns into a connection to the wrong board.
                if want.is_empty() {
                    return false;
                }
                normalise_address(id) == want
                    || address.is_some_and(|found| normalise_address(found) == want)
            }
        }
    }
}

/// An address reduced to what identifies it: hexadecimal digits, one case.
///
/// Kept as a function rather than done inline because it has to behave the
/// same on both sides of the comparison, and "the same" is easy to get wrong
/// once there are four call sites guessing about colons.
fn normalise_address(address: &str) -> String {
    address
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

/// Split a payload into writes the bearer will accept.
///
/// An empty payload becomes no writes at all, which matters more than it
/// looks: a keep-alive that wrote a zero-length packet would be inventing
/// traffic the device never asked to see.
fn segment(payload: &[u8], limit: usize) -> Vec<Vec<u8>> {
    if payload.is_empty() || limit == 0 {
        return Vec::new();
    }
    payload.chunks(limit).map(|piece| piece.to_vec()).collect()
}

/// One thing the interface asks the radio to send.
///
/// Two variants rather than one, because management frames are not longer
/// keystrokes: they go to a different characteristic and the device answers
/// them on another. Keeping them apart here is what lets a bridge's answer
/// never be mistaken for the target's output — see the note above
/// [`MGMT_SERVICE`].
enum Frame {
    /// Characters typed into the target.
    Stream(Vec<u8>),
    /// A whole management request, already framed by `protocol`.
    Management(Vec<u8>),
}

/// A BLE connection that has not been opened yet.
///
/// Nothing here touches the radio. Describing a connection has to be possible
/// with no adapter present — the command line builds one of these before it
/// has decided whether one can be made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BleLink {
    selection: Selection,
}

impl BleLink {
    /// Look for `selection`. No radio is touched until [`Link::open`].
    pub fn new(selection: Selection) -> BleLink {
        BleLink { selection }
    }

    /// Connect to whichever device turns up.
    pub fn discovering() -> BleLink {
        BleLink {
            selection: Selection::Any,
        }
    }

    /// Connect to a particular device, by name.
    pub fn named(name: impl Into<String>) -> BleLink {
        BleLink {
            selection: Selection::Name(name.into()),
        }
    }

    /// Connect to a particular device, by address.
    pub fn addressed(address: impl Into<String>) -> BleLink {
        BleLink {
            selection: Selection::Address(address.into()),
        }
    }

    /// The device this link is looking for.
    pub fn selection(&self) -> &Selection {
        &self.selection
    }
}

impl Link for BleLink {
    fn open(self: Box<Self>) -> io::Result<(Box<dyn LinkReader>, Box<dyn LinkWriter>)> {
        // Every channel here is synchronous except the outbound one, which has
        // to be a Tokio channel because the thing draining it lives in async
        // code. The bridge thread below meets those two halves.
        let (inbound_tx, inbound_rx) = mpsc::channel::<Option<Vec<u8>>>();
        let (outbound_tx, outbound_rx) = mpsc::channel::<Frame>();
        // Management responses ride here and nowhere else, so that the
        // interface can hand them to the pump as management and not as target
        // output. Their own channel, like their own characteristic.
        let (management_tx, management_rx) = mpsc::channel::<Vec<u8>>();
        let (ready_tx, ready_rx) = mpsc::channel::<io::Result<()>>();
        let (to_radio_tx, to_radio_rx) = tokio::sync::mpsc::channel::<Frame>(WRITE_BACKLOG);

        let selection = self.selection.clone();
        thread::spawn(move || {
            radio(&selection, ready_tx, inbound_tx, to_radio_rx, management_tx);
        });
        // The bytes this interface writes out are handed across to the reactor
        // by their own thread, because the writer here is a synchronous one
        // sitting in `spawn_writer`'s loop and `blocking_send` may not be
        // called from inside a runtime — this thread is outside every runtime.
        thread::spawn(move || {
            while let Ok(bytes) = outbound_rx.recv() {
                if to_radio_tx.blocking_send(bytes).is_err() {
                    break;
                }
            }
        });

        // Open synchronously. `open` returning means "connected" everywhere
        // else in the product, and a link that reported itself open while the
        // stack was still turning over would have the interface painting
        // before there was anything to paint.
        match ready_rx.recv_timeout(CONNECT_TIMEOUT) {
            Err(why) => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "the BLE link did not come up within {} s ({why})",
                    CONNECT_TIMEOUT.as_secs()
                ),
            )),
            Ok(Err(why)) => Err(why),
            Ok(Ok(())) => Ok((
                Box::new(BleReader {
                    inbound: inbound_rx,
                    management: Some(management_rx),
                    leftover: VecDeque::new(),
                    done: false,
                }),
                Box::new(BleWriter {
                    outbound: outbound_tx,
                }),
            )),
        }
    }

    fn kind(&self) -> LinkKind {
        LinkKind::Ble
    }

    fn describe(&self) -> String {
        format!("ble:{}", self.selection.label())
    }
}

/// The reactor thread: find, connect, subscribe, and then shuttle bytes until
/// something ends.
///
/// A format string rather than an error value, because the interesting
/// failures here are all of the "nothing like that on the air" shape and the
/// caller needs to say which nothing.
fn radio(
    selection: &Selection,
    ready: Sender<io::Result<()>>,
    inbound: Sender<Option<Vec<u8>>>,
    outbound: tokio::sync::mpsc::Receiver<Frame>,
    management: Sender<Vec<u8>>,
) {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        // Every timer and every BlueZ socket the link uses wants one of these,
        // and there is nothing else on this runtime to fight with.
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(why) => {
            let _ = ready.send(Err(io::Error::other(format!(
                "the BLE runtime could not be started: {why}"
            ))));
            return;
        }
    };

    let outcome = runtime.block_on(async move {
        let peripheral = match find(selection).await {
            Ok(peripheral) => peripheral,
            Err(why) => return why,
        };
        let target = match peripheral
            .properties()
            .await
            .map_err(|why| format!("the device would not say what it is: {why}"))
        {
            Ok(properties) => properties
                .and_then(|p| p.local_name)
                .unwrap_or_else(|| "the device".to_string()),
            Err(why) => return why,
        };

        if let Err(why) = wire(&peripheral).await {
            return format!("connected to {target} but the link was refused: {why}");
        }
        // Reported only once the subscription is live. A read that succeeds
        // and a notify that never arrives looks exactly like a dead target,
        // so "connected" means bytes can already flow both ways.
        let _ = ready.send(Ok(()));
        carry(&peripheral, &inbound, outbound, &management).await;
        // The one signal that means "stop reading" to whoever is waiting on
        // us, sent once the loop above has decided the device went away.
        let _ = inbound.send(None);
        format!("{target} went away")
    });

    // Kept alive to the last line so the connection outlives the loop that
    // used it; `runtime` dropping here is what hangs up anything the device
    // still has open.
    drop(runtime);
    let _ = outcome;
}

/// Scan for `selection` and connect to it.
async fn find(selection: &Selection) -> Result<Peripheral, String> {
    let manager = Manager::new()
        .await
        .map_err(|why| format!("no Bluetooth manager could be opened: {why}"))?;
    let adapters = manager
        .adapters()
        .await
        .map_err(|why| format!("no Bluetooth adapter could be listed: {why}"))?;
    let adapter = adapters.into_iter().next().ok_or_else(|| {
        "this machine has no Bluetooth adapter — one is needed to reach the \
         board, so try --transport serial or --bridge"
            .to_string()
    })?;

    adapter
        .start_scan(ScanFilter::default())
        .await
        .map_err(|why| format!("the scan would not start: {why}"))?;
    tokio::time::sleep(SCAN_TIMEOUT).await;

    let mut candidates = Vec::new();
    for candidate in adapter
        .peripherals()
        .await
        .map_err(|why| format!("the scan's results could not be read: {why}"))?
    {
        // Reading properties can fail for a device that has just moved out of
        // range; that is not interesting and must not end the scan.
        let Ok(Some(properties)) = candidate.properties().await else {
            continue;
        };
        let speaks_nus = properties.services.contains(&NUS_SERVICE);
        let wanted = selection.accepts(
            properties.local_name.as_deref(),
            &candidate.id().to_string(),
            None,
        );
        if speaks_nus && wanted {
            candidates.push(candidate);
        }
    }
    let _ = adapter.stop_scan().await;

    let chosen = candidates
        .into_iter()
        .next()
        .ok_or_else(|| match selection {
            Selection::Any => "nothing advertising the Nordic UART Service was found within \
             {} s — is the board powered?"
                .replace("{}", &SCAN_TIMEOUT.as_secs().to_string()),
            Selection::Name(name) => format!("no Nordic UART device named “{name}” was found"),
            Selection::Address(address) => {
                format!("no Nordic UART device at {address} was found")
            }
        })?;

    chosen
        .connect()
        .await
        .map_err(|why| format!("the connection was refused: {why}"))?;
    Ok(chosen)
}

/// Discover the service and subscribe to the characteristic that carries the
/// target's output.
async fn wire(peripheral: &Peripheral) -> Result<(), String> {
    peripheral
        .discover_services()
        .await
        .map_err(|why| format!("the services could not be enumerated: {why}"))?;

    let characteristics = peripheral.characteristics();
    for wanted in [NUS_TX, NUS_RX] {
        // Checked for both, because a firmware that advertises NUS but does
        // not lay out the standard characteristics is a real thing, and it is
        // better to say which end is missing than to fail on the first write.
        if !characteristics.iter().any(|c| c.uuid == wanted) {
            return Err(format!(
                "the device does not offer the Nordic UART characteristic {wanted}"
            ));
        }
    }
    let out = characteristics
        .iter()
        .find(|c| c.uuid == NUS_TX)
        .ok_or_else(|| "the notify characteristic vanished".to_string())?;
    peripheral
        .subscribe(out)
        .await
        .map_err(|why| format!("notify could not be subscribed to: {why}"))?;

    // The management service is optional and *must stay* optional: a bridge
    // without it is a bridge that can still carry a terminal, and refusing to
    // connect to one because it cannot answer `@i?` would turn a firmware
    // that predates the service into "the board is broken".
    if characteristics.iter().any(|c| c.uuid == MGMT_RESPONSE) {
        let responses = characteristics
            .iter()
            .find(|c| c.uuid == MGMT_RESPONSE)
            .ok_or_else(|| "the management notify characteristic vanished".to_string())?;
        peripheral
            .subscribe(responses)
            .await
            .map_err(|why| format!("the management channel could not be subscribed to: {why}"))?;
    }
    Ok(())
}

/// The first of a run of identical management notifications, and nothing of
/// the rest.
///
/// BlueZ re-broadcasts one GATT notification once per outstanding notify
/// session on the characteristic, so a client sees the same value several
/// times in a row whenever sessions from earlier clients have not been
/// cleaned up — and it kept happening, the count climbing from three to four
/// as more clients came and went. The radio sends each one once (checked on
/// the air with `btmon`: one indication, one confirmation), so the repeats
/// are the daemon's alone; passing them on turns one scan result into four
/// rows and one ending into several orphans.
///
/// The comparison is against the *immediately* previous notification rather
/// than a set of seen ones: what arrives is runs of repeats, and a value that
/// legitimately comes back later — a state that is published twice — is not
/// this function's to swallow. Terminal bytes never come through here, so
/// nothing a user types is ever suppressed.
fn keep_once(last: &mut Option<Vec<u8>>, value: Vec<u8>) -> Option<Vec<u8>> {
    if last.as_deref() == Some(value.as_slice()) {
        return None;
    }
    *last = Some(value.clone());
    Some(value)
}

/// Move bytes in both directions until one of them stops.
async fn carry(
    peripheral: &Peripheral,
    inbound: &Sender<Option<Vec<u8>>>,
    mut outbound: tokio::sync::mpsc::Receiver<Frame>,
    management: &Sender<Vec<u8>>,
) {
    let characteristics = peripheral.characteristics();
    let sink = characteristics.iter().find(|c| c.uuid == NUS_RX).cloned();
    let command = characteristics
        .iter()
        .find(|c| c.uuid == MGMT_COMMAND)
        .cloned();
    let Ok(mut notifications) = peripheral.notifications().await else {
        return;
    };
    // The last management notification, so its repeats can be told apart.
    // See `keep_once`.
    let mut last_response: Option<Vec<u8>> = None;

    loop {
        tokio::select! {
            notification = notifications.next() => {
                match notification {
                    Some(notification) => {
                        if notification.value.is_empty() {
                            continue;
                        }
                        // Two notify characteristics now feed this loop, and
                        // which one a packet came in on is the only reliable
                        // way to know what it is: both carry plain bytes and
                        // neither carries a hint. Sending a management
                        // response down the target's stream would type the
                        // bridge's answer into somebody's shell prompt.
                        if notification.uuid == MGMT_RESPONSE {
                            if let Some(bytes) = keep_once(&mut last_response, notification.value) {
                                if management.send(bytes).is_err() {
                                    return;
                                }
                            }
                        } else if notification.uuid == NUS_TX
                            && inbound.send(Some(notification.value)).is_err()
                        {
                            return;
                        }
                    }
                    // The stream ending is the device going away.
                    None => return,
                }
            }
            frame = outbound.recv() => {
                match frame {
                    Some(Frame::Stream(bytes)) => {
                        let Some(sink) = sink.as_ref() else { continue };
                        for piece in segment(&bytes, MTU_WRITE_LIMIT) {
                            // Without response is the throughput choice: NUS is
                            // handed to the Link Layer with whatever credit it
                            // has, and waiting for an acknowledgement per packet
                            // would cap the link well under the radio's rate.
                            if peripheral
                                .write(sink, &piece, WriteType::WithoutResponse)
                                .await
                                .is_err()
                            {
                                return;
                            }
                        }
                    }
                    // With response, unlike the stream above. A management
                    // request asks something worth waiting for and its answer
                    // is worth losing nothing of; there are a handful of these
                    // per session and none on the path a keystroke takes.
                    Some(Frame::Management(frame)) => {
                        let Some(command) = command.as_ref() else { continue };
                        for piece in segment(&frame, MTU_WRITE_LIMIT) {
                            if peripheral
                                .write(command, &piece, WriteType::WithResponse)
                                .await
                                .is_err()
                            {
                                return;
                            }
                        }
                    }
                    // The sender dropping is the interface closing the link.
                    None => return,
                }
            }
        }
    }
}

/// Bytes from the device, in order.
struct BleReader {
    inbound: Receiver<Option<Vec<u8>>>,
    /// Responses from the bridge's management channel, until [`Self::
    /// side_channel`] takes them over. `Option` because that happens exactly
    /// once and must not be able to happen twice.
    management: Option<Receiver<Vec<u8>>>,
    /// What a notification that did not fit in the caller's buffer left over.
    leftover: VecDeque<u8>,
    done: bool,
}

impl LinkReader for BleReader {
    /// Management responses are not target output, so they leave by the side
    /// channel rather than by [`Self::read`].
    ///
    /// The forwarding thread exists because the radio speaks in Tokio time and
    /// this reader is read synchronously on a thread of its own: merging the
    /// two would mean polling for management bytes inside `read`, and a
    /// `read` that returns early looking for them would look exactly like a
    /// link that hung up.
    fn side_channel(&mut self, events: Sender<super::LinkEvent>) {
        let Some(management) = self.management.take() else {
            return;
        };
        thread::spawn(move || {
            while let Ok(bytes) = management.recv() {
                if events.send(super::LinkEvent::Management(bytes)).is_err() {
                    return;
                }
            }
        });
    }

    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.done {
            return Ok(0);
        }
        // Drained first: the previous notification may have been longer than
        // the caller's buffer, and those bytes were promised before any later
        // ones were received.
        while self.leftover.is_empty() {
            match self.inbound.recv() {
                Ok(Some(bytes)) => self.leftover.extend(bytes),
                // `None` is the radio saying the device went away; a
                // disconnected channel means the loop already gave up. Either
                // way this is a clean end of stream, not an error.
                Ok(None) | Err(_) => {
                    self.done = true;
                    return Ok(0);
                }
            }
        }
        let n = buf.len().min(self.leftover.len());
        for slot in buf.iter_mut().take(n) {
            *slot = self.leftover.pop_front().unwrap_or(0);
        }
        Ok(n)
    }
}

/// Bytes towards the device.
struct BleWriter {
    outbound: Sender<Frame>,
}

impl LinkWriter for BleWriter {
    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        self.outbound
            .send(Frame::Stream(buf.to_vec()))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "the BLE link is no longer up"))
    }

    /// Queue a management frame for the bridge.
    ///
    /// Framing belongs to `protocol`, not here: this knows only which
    /// characteristic the bytes go to, which is the one piece of the answer a
    /// bearer cannot get from anywhere else.
    fn write_management(&mut self, frame: &[u8]) -> io::Result<()> {
        self.outbound
            .send(Frame::Management(frame.to_vec()))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "the BLE link is no longer up"))
    }

    fn flush(&mut self) -> io::Result<()> {
        // Nothing is buffered here: each write goes straight to the radio, and
        // pretending otherwise would let a caller believe its bytes were held
        // when in fact they have already gone out.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The characteristics are named after their direction from the device's
    /// point of view, which is backwards from this host's, and getting it
    /// backwards is the single most likely bug in this module. Asserted here
    /// against the strings every NUS implementation publishes.
    #[test]
    fn the_uuids_are_the_nordic_ones() {
        assert_eq!(
            NUS_SERVICE.to_string(),
            "6e400001-b5a3-f393-e0a9-e50e24dcca9e"
        );
        assert_eq!(NUS_RX.to_string(), "6e400002-b5a3-f393-e0a9-e50e24dcca9e");
        assert_eq!(NUS_TX.to_string(), "6e400003-b5a3-f393-e0a9-e50e24dcca9e");
    }

    /// "Any" is how a single board on a bench is dialled, and it has to accept
    /// even a device that told us nothing about itself.
    #[test]
    fn any_accepts_the_first_nus_device_whatever_it_is_called() {
        let any = Selection::any();
        assert!(any.accepts(Some("Linkr BLE UART-3"), "EE:C7:42:34:48:CF", None));
        assert!(any.accepts(None, "some-platform-uuid", None));
    }

    /// Names are matched on a substring ignoring case, because the same board
    /// advertises `Linkr BLE UART-3` in one firmware and something else in
    /// another, and the user types the part they recognise.
    #[test]
    fn a_name_matches_a_fragment_in_any_case() {
        let by_name = Selection::Name("linkr".to_string());
        assert!(by_name.accepts(Some("Linkr BLE UART-3"), "EE:C7:42:34:48:CF", None));
        assert!(!by_name.accepts(Some("Some Other Device"), "EE:C7:42:34:48:CF", None));
        // No name advertised at all cannot match a name asked for.
        assert!(!by_name.accepts(None, "EE:C7:42:34:48:CF", None));
    }

    /// An address is either the BlueZ identifier or the advertised address,
    /// and either spelling of it must work.
    #[test]
    fn an_address_matches_either_way_the_platform_reports_it() {
        let by_address = Selection::Address("ee:c7:42:34:48:cf".to_string());
        assert!(by_address.accepts(Some("Linkr BLE UART-3"), "EE:C7:42:34:48:CF", None));
        assert!(
            by_address.accepts(
                Some("Linkr BLE UART-3"),
                "some-uuid",
                Some("EE:C7:42:34:48:CF")
            ),
            "and by the advertised address when the id is not a MAC"
        );
        assert!(
            !by_address.accepts(Some("Linkr BLE UART-3"), "AA:BB:CC:DD:EE:FF", None),
            "another board is not this board"
        );
    }

    /// A mistyped address that reduces to nothing must dial nothing. Without
    /// this a value like `":"` would match every candidate.
    #[test]
    fn an_empty_address_never_matches() {
        let empty = Selection::Address("::".to_string());
        assert!(!empty.accepts(Some("Linkr BLE UART-3"), "EE:C7:42:34:48:CF", None));
        assert!(!empty.accepts(Some("Anything"), "", None));
    }

    /// Segmentation is what keeps a long write legal, and the thing it must
    /// never do is change or drop a byte.
    #[test]
    fn segmentation_preserves_every_byte_in_order() {
        let payload: Vec<u8> = (0..800).map(|i| (i % 251) as u8).collect();
        let pieces = segment(&payload, MTU_WRITE_LIMIT);
        assert_eq!(pieces.len(), 4, "800 bytes at 244 goes in four writes");
        assert!(pieces.iter().all(|p| p.len() <= MTU_WRITE_LIMIT));
        let rejoined: Vec<u8> = pieces.into_iter().flatten().collect();
        assert_eq!(rejoined, payload, "and every byte survives, in order");
    }

    /// Nothing to send is no write at all — a zero-length ATT packet is
    /// traffic the device did not ask for.
    #[test]
    fn nothing_to_send_is_no_packet() {
        assert!(segment(&[], MTU_WRITE_LIMIT).is_empty());
    }

    /// The sidebar has to say which board is being looked for, or an operator
    /// with two on the bench cannot tell them apart.
    #[test]
    fn the_description_names_the_target() {
        assert_eq!(
            BleLink::discovering().describe(),
            "ble:find the nearest device"
        );
        assert_eq!(
            BleLink::named("Linkr BLE UART-3").describe(),
            "ble:Linkr BLE UART-3"
        );
        assert_eq!(
            BleLink::addressed("ee:c7:42:34:48:cf").describe(),
            "ble:EE:C7:42:34:48:CF",
            "and the address is said the way the tooling prints it"
        );
    }

    /// The link must name itself as Bluetooth whatever else changes.
    #[test]
    fn the_kind_is_bluetooth() {
        assert_eq!(BleLink::discovering().kind(), LinkKind::Ble);
    }

    /// A reader whose radio has gone must say end-of-stream rather than an
    /// error, because a device that walked away is not a fault to report.
    #[test]
    fn a_reader_with_no_radio_behind_it_reads_end_of_stream() {
        let (tx, rx) = mpsc::channel::<Option<Vec<u8>>>();
        let mut reader = BleReader {
            inbound: rx,
            management: None,
            leftover: VecDeque::new(),
            done: false,
        };
        let _ = tx.send(Some(vec![1, 2, 3]));
        let mut buf = [0u8; 8];
        assert_eq!(reader.read(&mut buf).unwrap(), 3);
        assert_eq!(&buf[..3], &[1, 2, 3]);

        // "The device went away".
        let _ = tx.send(None);
        assert_eq!(reader.read(&mut buf).unwrap(), 0, "then end of stream");
        assert_eq!(reader.read(&mut buf).unwrap(), 0, "and that stays true");
    }

    /// A notification longer than the caller's buffer is *not* truncated: the
    /// rest has to come back on the next read. Losing the tail of a long line
    /// would corrupt the very traffic this link exists to carry.
    #[test]
    fn bytes_that_do_not_fit_wait_for_the_next_read() {
        let (tx, rx) = mpsc::channel::<Option<Vec<u8>>>();
        let mut reader = BleReader {
            inbound: rx,
            management: None,
            leftover: VecDeque::new(),
            done: false,
        };
        let sent: Vec<u8> = (0..10).collect();
        let _ = tx.send(Some(sent.clone()));

        let mut got = Vec::new();
        let mut small = [0u8; 4];
        for expected in [4, 4, 2] {
            let n = reader.read(&mut small).unwrap();
            assert_eq!(n, expected, "one read takes no more than it is offered");
            got.extend_from_slice(&small[..n]);
        }
        assert_eq!(got, sent, "ten bytes through a four-byte buffer, whole");
    }

    /// A notification the daemon repeated is passed on once, and its repeats
    /// are dropped — but only while they are *repeats*: the same value coming
    /// back after a different one is a new event and is not this link's to
    /// swallow.
    #[test]
    fn a_repeated_notification_is_passed_on_once() {
        let mut last = None;
        let line = b"@scan result BenchNet wpa2 ch=6 -54dBm".to_vec();

        for copy in 0..4 {
            let kept = keep_once(&mut last, line.clone());
            if copy == 0 {
                assert_eq!(kept.as_ref(), Some(&line), "the first copy is the event");
            } else {
                assert!(kept.is_none(), "copy {copy} is the daemon talking twice");
            }
        }

        // A different notification in between makes the same value new again.
        assert!(keep_once(&mut last, b"@scan done".to_vec()).is_some());
        assert!(
            keep_once(&mut last, line.clone()).is_some(),
            "after a different notification the same value is a new one"
        );
    }
}
