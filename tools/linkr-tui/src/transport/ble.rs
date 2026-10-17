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

/// The name the bridge's firmware ships with, which its scan response carries
/// when the advertisement itself has no room for it.
///
/// A prefix rather than the whole name: a bench with several units suffixes
/// the name to tell them apart (`Linkr BLE UART 7F2C`), and the name is only
/// one of the three signals [`is_board`] accepts.
pub const BOARD_NAME: &str = "Linkr BLE UART";

/// Whether an advertisement describes a board this program can dial.
///
/// Three signals, any one of them enough, because what an advertisement
/// carries depends on the platform as much as on the firmware. The board
/// itself advertises the **management service** and puts its **name** in the
/// scan response — the UART service lives in the GATT table and is not on the
/// air at all. BlueZ folds cached GATT services into what it reports, so a
/// Linux that has met this board before lists the UART service too and a
/// UART-only test works there; Windows reports the advertisement and nothing
/// else, so that same test found nothing at all on Windows. All three signals
/// are accepted, and which one answered is not kept: what the device *is* is
/// checked once the link is up, where [`wire`] refuses anything without the
/// UART characteristics — a device that merely looks like a board fails with
/// the right error instead of reading as silence.
pub fn is_board(services: &[Uuid], name: Option<&str>) -> bool {
    services
        .iter()
        .any(|uuid| *uuid == NUS_SERVICE || *uuid == MGMT_SERVICE)
        || name.is_some_and(|name| {
            name.get(..BOARD_NAME.len())
                .is_some_and(|head| head.eq_ignore_ascii_case(BOARD_NAME))
        })
}

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
/// that "wrong board" is answered rather than waited out. The search ends the
/// moment a candidate answers, so this is a ceiling and not a delay: a board
/// in front of the user is dialled in the first second of it.
const SCAN_TIMEOUT: Duration = Duration::from_secs(10);

/// How often the adapter's results are looked at while listening.
///
/// BlueZ reports an advertisement as soon as it hears one, so a quarter of a
/// second is invisible to the user and kind to the radio.
const SCAN_POLL: Duration = Duration::from_millis(250);

/// How long `open` waits for the reactor to report one way or the other.
///
/// The reactor retries inside this window rather than the caller retrying the
/// link: the state of a failed attempt is known there, and the interface is
/// owed the real reason the moment there is one — not twenty seconds of
/// waiting for an answer that was never coming.
///
/// Sized to cover the worst dial the reactor will attempt — a scan that runs
/// out its window, one connect that runs out its own, and the handshake on top
/// — because a window shorter than the work turns a slow board into a missing
/// one. A caller that would rather wait less says `--connect-timeout`, and
/// [`Link::open`]'s late-success path hangs the link up again rather than
/// leaving a radio nobody is reading.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// How long one dial attempt may take before it counts as failed.
///
/// btleplug's own ceiling is half a minute; that is a bound on the D-Bus call,
/// not on a person's patience, and a screen that says "connecting" for it is
/// worse than an attempt that starts over from nothing. Fifteen seconds and
/// not less, because a Windows connect builds a fresh GATT session every time
/// — WinRT keeps no service cache between sessions — and a first dial to a
/// board this machine has not seen this boot takes several of them; cutting at
/// eight read as a refusal on a board that was merely slow, while the host
/// tool this program replaces allowed for it.
const DIAL_ATTEMPT: Duration = Duration::from_secs(15);

/// The wall-clock budget every retry loop gets: three backoffs' worth.
const RETRY_BUDGET: Duration = Duration::from_millis(2250);

/// How long to wait between two attempts of the same operation.
const RETRY_BACKOFF: Duration = Duration::from_millis(750);

/// How often to look for a link somebody else is dialling, while a busy
/// answer says one is being dialled right now.
const RETRY_POLL: Duration = Duration::from_millis(250);

/// How long to give BlueZ to finish a disconnect before dialling again.
///
/// Asking for the disconnect is instant and being free of it is not: BlueZ
/// tears the link down on its own time, and a dial that starts inside that
/// window finds a device that is reported connected and cannot be wired. Two
/// and a half seconds is several times the teardown measured on a desktop
/// adapter, and it is only ever spent when a dial actually meets one.
const HANGUP_SETTLE: Duration = Duration::from_millis(2500);

/// How long a single BlueZ call may take before the adapter is called wedged.
///
/// Generous on purpose: a real GATT call completes in milliseconds, and only
/// an adapter that has stopped answering reaches this. A call that never
/// returned would hold the one reactor thread this link has forever, and a
/// link that is quietly wedged looks exactly like a board that went silent.
const CALL_TIMEOUT: Duration = Duration::from_secs(15);

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
    /// End the LE link itself. Nothing is written: the radio is asked to
    /// disconnect, which is what makes the board advertise again — a board
    /// left connected to a client that is gone is a board the next client
    /// cannot find.
    Hangup,
}

/// A BLE connection that has not been opened yet.
///
/// Nothing here touches the radio. Describing a connection has to be possible
/// with no adapter present — the command line builds one of these before it
/// has decided whether one can be made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BleLink {
    selection: Selection,
    /// How long the whole dial may take. `None` is the radio's own patience,
    /// which is what an interface dial gets; a script that named
    /// `--connect-timeout` hands its own window down here.
    within: Option<Duration>,
}

impl BleLink {
    /// Look for `selection`. No radio is touched until [`Link::open`].
    pub fn new(selection: Selection) -> BleLink {
        BleLink {
            selection,
            within: None,
        }
    }

    /// Give the whole dial this much time at most.
    pub fn within(mut self, window: Option<Duration>) -> BleLink {
        self.within = window;
        self
    }

    /// Connect to whichever device turns up.
    pub fn discovering() -> BleLink {
        BleLink {
            selection: Selection::Any,
            within: None,
        }
    }

    /// Connect to a particular device, by name.
    pub fn named(name: impl Into<String>) -> BleLink {
        BleLink {
            selection: Selection::Name(name.into()),
            within: None,
        }
    }

    /// Connect to a particular device, by address.
    pub fn addressed(address: impl Into<String>) -> BleLink {
        BleLink {
            selection: Selection::Address(address.into()),
            within: None,
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
        let window = self.within.unwrap_or(CONNECT_TIMEOUT);
        match ready_rx.recv_timeout(window) {
            Err(why) => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "the BLE link did not come up within {} s ({why})",
                    window.as_secs()
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

    runtime.block_on(async move {
        // Every failing path reports through `ready`: `open` is waiting on
        // exactly this channel, and a caller told "it took too long" when the
        // answer was "there is no board on the air" has been told nothing.
        let refuse = |why: String| {
            let _ = ready.send(Err(io::Error::other(why)));
        };
        let (peripheral, dialled_here) = match find(selection).await {
            Ok(found) => found,
            Err(why) => {
                refuse(why);
                return;
            }
        };
        let target = match peripheral
            .properties()
            .await
            .map_err(|why| format!("the device would not say what it is: {why}"))
        {
            Ok(properties) => properties
                .and_then(|p| p.local_name)
                .unwrap_or_else(|| "the device".to_string()),
            Err(why) => {
                refuse(why);
                return;
            }
        };

        // Joining a link that is already up is an optimization, and it has one
        // failure shape: BlueZ reports a device as connected for a second or
        // two after the client that dialled it asked to hang up, and a session
        // built on that remaining link cannot subscribe — the CCCD write lands
        // on a connection BlueZ is in the middle of taking down. So a *joined*
        // link that will not wire is cleared, waited out, and dialled for real
        // once. A link this dial made keeps its own refusal, which is about the
        // device rather than about what the last session left behind.
        let mut dialled_here = dialled_here;
        let mut wired = wire(&peripheral).await;
        if wired.is_err() && !dialled_here {
            let _ = peripheral.disconnect().await;
            wait_until_down(&peripheral).await;
            match dial(&peripheral).await {
                Ok(made) => {
                    dialled_here = made;
                    wired = wire(&peripheral).await;
                }
                Err(why) => {
                    refuse(why);
                    return;
                }
            }
        }
        if let Err(why) = wired {
            // A refused link is still an up link as far as BlueZ is concerned,
            // and hanging it up is only ours to do when the dial was: a link
            // another client brought up has that client's session running
            // over it.
            if dialled_here {
                let _ = peripheral.disconnect().await;
            }
            refuse(format!(
                "connected to {target} but the link was refused: {why}"
            ));
            return;
        }
        // Reported only once the subscription is live. A read that succeeds
        // and a notify that never arrives looks exactly like a dead target,
        // so "connected" means bytes can already flow both ways.
        if ready.send(Ok(())).is_err() {
            // The caller gave up (or went away) while this dial was still
            // working, so there is no reader for the bytes below and a link
            // left open is a radio the next dial has to fight for. Hanging it
            // up is ours to do only when the dial was.
            if dialled_here {
                let _ = peripheral.disconnect().await;
            }
            return;
        }
        carry(&peripheral, &inbound, outbound, &management).await;
        // The one signal that means "stop reading" to whoever is waiting on
        // us, sent once the loop above has decided the device went away.
        let _ = inbound.send(None);
    });

    // Kept alive to the last line so the connection outlives the loop that
    // used it; `runtime` dropping here is what hangs up anything the device
    // still has open.
    drop(runtime);
}

/// Dial the board, the way a bench with two clients needs it dialled.
///
/// * a link that is already up — from an earlier attempt, or because a second
///   client got there first — is joined instead of dialled again;
/// * `In Progress` means another client is dialling *right now*, and our own
///   `Connect` is exactly what makes BlueZ cancel theirs, so the loop stops
///   asking and watches for their link instead;
/// * a transient `br-connection-*` answer means our own attempt died with
///   nothing up, so the leftovers are cleared and it is tried again within
///   the budget;
/// * everything else is the answer, kept whole and worded by [`dial_error`].
///
/// `Ok(true)` when this call dialled the link, `Ok(false)` when it joined one
/// that was already up: the difference is who may hang it up again.
async fn dial(peripheral: &Peripheral) -> Result<bool, String> {
    let deadline = tokio::time::Instant::now() + RETRY_BUDGET;
    loop {
        if is_up(peripheral).await {
            return Ok(false);
        }
        let error = match peripheral.connect_with_timeout(DIAL_ATTEMPT).await {
            Ok(()) => return Ok(true),
            Err(error) => error,
        };
        let text = error.to_string();
        if is_transient(&text) && tokio::time::Instant::now() < deadline {
            // Best effort: nothing was up, but BlueZ may still be holding the
            // remains of the attempt, and a fresh `Connect` wants them gone.
            let _ = peripheral.disconnect().await;
            tokio::time::sleep(RETRY_BACKOFF).await;
            continue;
        }
        if is_busy(&text) {
            while tokio::time::Instant::now() < deadline {
                tokio::time::sleep(RETRY_POLL).await;
                if is_up(peripheral).await {
                    return Ok(false);
                }
            }
        }
        return Err(dial_error(&text));
    }
}

/// `true` when BlueZ already reports a link — whether this session dialled it
/// or another client did.
async fn is_up(peripheral: &Peripheral) -> bool {
    peripheral.is_connected().await.unwrap_or(false)
}

/// Wait for BlueZ to agree that the link is down, or for [`HANGUP_SETTLE`] to
/// run out.
///
/// Asking for a disconnect is not the same as the device being free again: the
/// property turns at the end of a teardown BlueZ runs on its own time, and a
/// dial that starts inside that window finds a link that is up and unusable.
async fn wait_until_down(peripheral: &Peripheral) {
    let deadline = tokio::time::Instant::now() + HANGUP_SETTLE;
    while is_up(peripheral).await && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(RETRY_POLL).await;
    }
}

/// A failed dial as a sentence: the two answers that have an action in them
/// name the action, and every other failure keeps its own words behind the
/// one thing this function knows and the failure does not — that the question
/// was "connect".
fn dial_error(text: &str) -> String {
    if is_busy(text) {
        format!(
            "the Bluetooth adapter is busy: {text} — another scan or connection is \
             still running; retry in a second"
        )
    } else if is_transient(text) {
        format!(
            "the BLE link dropped mid-dial: {text} — the board or another client let \
             go of the connection; retry in a second"
        )
    } else {
        format!("the connection was refused: {text}")
    }
}

/// `true` when BlueZ is saying somebody else owns the adapter: another scan or
/// connection is already running. The D-Bus message is `In Progress`, the
/// error name is `org.bluez.Error.InProgress`, and both have been seen in more
/// than one casing.
fn is_busy(text: &str) -> bool {
    let lowered = text.to_ascii_lowercase();
    lowered.contains("in progress") || lowered.contains("inprogress")
}

/// BlueZ's transient `Connect` answers: the attempt died before any link came
/// up — another client won the race, the controller gave up, or the board
/// stopped answering mid-handshake. Nothing succeeded, so starting over is
/// safe. The permanent refusals (`br-connection-rej-security`,
/// `br-connection-params-rejected`, …) are deliberately not here.
fn is_transient(text: &str) -> bool {
    const TRANSIENT: [&str; 5] = [
        "br-connection-canceled",
        "br-connection-timeout",
        "br-connection-failed",
        "br-connection-adv-timeout",
        "Not connected",
    ];
    TRANSIENT.iter().any(|needle| text.contains(needle))
}

/// Run `operation` again while BlueZ reports it busy and the budget lasts; the
/// last failure is handed up as text when it is not.
///
/// Only `In Progress` is retried, and only because it is the failure where the
/// operation demonstrably did **not** happen: BlueZ never started it, a second
/// client holds the adapter — a second `linkr`, a phone app, or a session of
/// ours that never tore down. Everything else keeps its own words.
///
/// Each attempt is bounded by [`CALL_TIMEOUT`]; a timeout is never retried,
/// because the operation may have half landed and is reported instead.
pub(crate) async fn retry_busy<T, F, Fut>(mut operation: F) -> Result<T, String>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, btleplug::Error>>,
{
    let deadline = tokio::time::Instant::now() + RETRY_BUDGET;
    loop {
        match tokio::time::timeout(CALL_TIMEOUT, operation()).await {
            Ok(Ok(value)) => return Ok(value),
            Ok(Err(error))
                if is_busy(&error.to_string()) && tokio::time::Instant::now() < deadline =>
            {
                tokio::time::sleep(RETRY_BACKOFF).await;
            }
            Ok(Err(error)) => return Err(error.to_string()),
            Err(_elapsed) => {
                return Err(format!(
                    "a Bluetooth call did not answer within {} s",
                    CALL_TIMEOUT.as_secs()
                ))
            }
        }
    }
}

/// Scan for `selection` and connect to it, reporting whether the dial was
/// this session's own — see [`dial`].
async fn find(selection: &Selection) -> Result<(Peripheral, bool), String> {
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

    // Whatever BlueZ already knows answers first: a board the system has seen
    // — by the startup scan, by an earlier session — needs no listening to be
    // dialled, and this is what makes a remembered address feel instant.
    let mut chosen = pick(&adapter, selection).await?;
    if chosen.is_none() {
        // Then the scan, which ends the moment the board answers rather than
        // at the end of a fixed window: waiting ten seconds for an answer
        // that arrived in the first one is ten seconds of the user watching a
        // screen that says "connecting".
        retry_busy(|| async { adapter.start_scan(ScanFilter::default()).await })
            .await
            .map_err(|why| format!("the scan would not start: {why}"))?;
        let deadline = tokio::time::Instant::now() + SCAN_TIMEOUT;
        loop {
            chosen = pick(&adapter, selection).await?;
            if chosen.is_some() || tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(SCAN_POLL).await;
        }
        let _ = adapter.stop_scan().await;
    }

    let chosen = chosen.ok_or_else(|| match selection {
        Selection::Any => "no board was found within {} s — is the board powered?"
            .replace("{}", &SCAN_TIMEOUT.as_secs().to_string()),
        Selection::Name(name) => format!("no board named “{name}” was found"),
        Selection::Address(address) => {
            format!("no board at {address} was found")
        }
    })?;

    let dialled_here = dial(&chosen).await?;
    Ok((chosen, dialled_here))
}

/// The first device that is a board this interface can talk to — see
/// [`is_board`] — and that `selection` accepts, or `None` while nobody like
/// that has answered yet.
///
/// The check is here and not in the scan's filter on purpose: a filtered scan
/// only hears devices whose advertisement names the filter's UUIDs, and a
/// board whose advertisement is (or reads as) partial is still a board this
/// bridge can dial. A device named by *address* is the exception, and is
/// dialled on BlueZ's word alone: a bonded board that is not advertising right
/// now has no UUIDs to check, and the characteristic check that matters happens
/// once the link is up (`wire`).
async fn pick(
    adapter: &btleplug::platform::Adapter,
    selection: &Selection,
) -> Result<Option<Peripheral>, String> {
    let candidates = adapter
        .peripherals()
        .await
        .map_err(|why| format!("the scan's results could not be read: {why}"))?;
    for candidate in candidates {
        // Reading properties can fail for a device that has just moved out of
        // range; that is not interesting and must not end the search.
        let Ok(Some(properties)) = candidate.properties().await else {
            continue;
        };
        let looks_like_a_board = is_board(&properties.services, properties.local_name.as_deref());
        let wanted = selection.accepts(
            properties.local_name.as_deref(),
            &candidate.id().to_string(),
            // BlueZ's identifier is a D-Bus path (`hci0/dev_EE_…`) while the
            // user names the board by its address, so the address property is
            // offered as well and either match counts.
            Some(&properties.address.to_string()),
        );
        let named_by_address = matches!(selection, Selection::Address(_));
        if wanted && (looks_like_a_board || named_by_address) {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

/// Discover the service and subscribe to the characteristic that carries the
/// target's output.
async fn wire(peripheral: &Peripheral) -> Result<(), String> {
    retry_busy(|| async { peripheral.discover_services().await })
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
    retry_busy(|| async { peripheral.subscribe(out).await })
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
        retry_busy(|| async { peripheral.subscribe(responses).await })
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
                            // Busy retries stand in for the acknowledgement:
                            // an `In Progress` chunk is one BlueZ never sent.
                            if retry_busy(|| async {
                                peripheral
                                    .write(sink, &piece, WriteType::WithoutResponse)
                                    .await
                            })
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
                            if retry_busy(|| async {
                                peripheral
                                    .write(command, &piece, WriteType::WithResponse)
                                    .await
                            })
                            .await
                            .is_err()
                            {
                                return;
                            }
                        }
                    }
                    // The sender dropping is the interface closing the link.
                    None => return,
                    // Asked to end it: the disconnect is the answer, so it is
                    // made before the loop leaves. Best effort — a link the
                    // device has already dropped is a link that is already
                    // hung up, which is the state we wanted.
                    Some(Frame::Hangup) => {
                        let _ = peripheral.disconnect().await;
                        return;
                    }
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

    /// End the LE link out loud: the radio disconnects, the board advertises
    /// again, and the next client — phone, tablet or another bench — can find
    /// it without waiting for a supervision timeout to guess we are gone.
    fn hangup(&mut self) -> io::Result<()> {
        self.outbound
            .send(Frame::Hangup)
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
        // The real failure on a Linux bench: BlueZ's id is a D-Bus path, and
        // only the address property says which board it is. A client that
        // offered only the id refused a board sitting in the cache under the
        // address the user had typed.
        assert!(
            by_address.accepts(
                Some("Linkr BLE UART-3"),
                "hci0/dev_EE_C7_42_34_48_CF",
                Some("EE:C7:42:34:48:CF")
            ),
            "a path-shaped id is matched by the address property"
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

    /// The firmware's own advertisement is the Windows case: it carries the
    /// management service and the name, and never the UART service — so a
    /// board test that wanted UART found nothing at all on a Windows host
    /// while BlueZ, holding a service cache from an earlier session, found
    /// the same board on Linux. Each of the three signals alone is a board.
    #[test]
    fn a_board_is_recognised_by_any_of_its_signals() {
        let none = None;
        assert!(
            is_board(&[NUS_SERVICE], none),
            "the UART service, cached by BlueZ"
        );
        assert!(
            is_board(&[MGMT_SERVICE], none),
            "the management service, as advertised"
        );
        assert!(
            is_board(&[], Some("Linkr BLE UART")),
            "exactly the shipped name"
        );
        assert!(
            is_board(&[], Some("linkr ble uart 7f2c")),
            "another unit's suffix does not make it a stranger"
        );
    }

    /// A name that merely starts the same way is not the board, and a short
    /// or non-ASCII name must be answered rather than panicked over — the
    /// prefix is cut from the candidate's bytes, not the constant's.
    #[test]
    fn a_name_that_only_sounds_like_the_board_is_not_one() {
        assert!(!is_board(&[], Some("Linkr BLE UAR")));
        assert!(!is_board(&[], Some("Linkr BLE Something")));
        assert!(!is_board(&[], None));
        assert!(!is_board(&[], Some("连接加热器")));
        assert!(!is_board(&[], Some("")));
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

    /// A second client holding the adapter is an answer BlueZ gives in more
    /// than one spelling, and every one of them means "try again in a moment",
    /// not "no".
    #[test]
    fn busy_failures_are_recognised_in_every_spelling() {
        assert!(is_busy("In Progress"), "the D-Bus message");
        assert!(is_busy("org.bluez.Error.InProgress"), "the error name");
        assert!(is_busy("IN PROGRESS"), "casing");
        assert!(!is_busy("device not found matching: AA:BB:CC:DD:EE:FF"));
        assert!(!is_busy("a Bluetooth call did not answer within 15 s"));
        assert!(!is_busy(""));
    }

    /// The transient `Connect` answers are the ones worth another dial; a
    /// permanent refusal is not, and retrying it would only delay the real
    /// message by the length of the budget.
    #[test]
    fn only_transient_refusals_are_worth_another_dial() {
        for transient in [
            "br-connection-canceled",
            "br-connection-timeout",
            "br-connection-failed",
            "br-connection-adv-timeout",
            "Not connected",
        ] {
            assert!(is_transient(transient), "{transient} is transient");
        }
        assert!(
            !is_transient("br-connection-rej-security"),
            "a refusal is the answer"
        );
        assert!(
            !is_transient("In Progress"),
            "busy is waited out, not redialled"
        );
        assert!(!is_transient("device not found"));
    }

    /// A dial that failed anyway says what to do about it, and anything else
    /// keeps BlueZ's own words behind the one thing the dial knows.
    #[test]
    fn a_failed_dial_names_the_next_step() {
        let busy = dial_error("org.bluez.Error.InProgress");
        assert!(busy.contains("adapter is busy"), "{busy}");
        assert!(busy.contains("retry in a second"), "{busy}");

        let dropped = dial_error("br-connection-canceled");
        assert!(dropped.contains("dropped mid-dial"), "{dropped}");
        assert!(dropped.contains("br-connection-canceled"), "{dropped}");

        let refused = dial_error("br-connection-rej-security");
        assert_eq!(
            refused, "the connection was refused: br-connection-rej-security",
            "a permanent refusal is not dressed up"
        );
    }
}
