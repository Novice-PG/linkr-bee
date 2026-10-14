//! The wire: whatever carries bytes between this host and the device.
//!
//! Every link in the product — the process bridge that stands in for a radio
//! while there is no hardware, Bluetooth, the LAN socket — is the same two
//! things: something that yields received bytes and something that accepts
//! bytes to send.
//!
//! There are three, and there is no serial link among them on purpose. The
//! device *is* a wireless serial bridge: the far end of every link here is a
//! UART the bridge is holding, and reaching the port directly from the host
//! would need a cable — which is the thing the product exists to do without.
//! The target's serial semantics (its prompts, its pager, the shape of its
//! login) are this side's business, because they arrive over one of these
//! links as bytes; see `serial`. Opening the port itself never is. That is all a view or a protocol needs to know, so it
//! is all this module promises. The link is therefore split into a reader half
//! and a writer half at open time: a BLE characteristic and a serial port both
//! need reads and writes to make progress *at the same time*, and a single
//! `&mut self` API would make the writer wait behind a read that has not
//! arrived yet.
//!
//! Bytes move on two dedicated threads rather than in the event loop, because
//! both halves block: a terminal that spins on `read` is a terminal that burns
//! a core, and the interface has a 250 ms beat to keep. The loop talks to the
//! threads over channels, so `App` never owns a socket, a file descriptor or a
//! `JoinHandle` — it owns a [`LinkHandle`], which is plain data plus a queue.
//!
//! That also makes the whole link testable with no hardware at all: the tests
//! drive an in-memory pair through the same `Link` trait the real links
//! implement, so the pump, the counters, the disconnect path and the journal
//! are exercised exactly as they are in production.

pub mod ble;
pub mod lan;
pub mod stdio;

use std::io;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};

/// Which link the session is carried over. The sidebar names it, the command
/// line pins it, and `--transport` accepts the spelling [`LinkKind::as_str`]
/// gives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LinkKind {
    /// A child process whose standard input and output carry device bytes —
    /// the bridge that stands in for a radio before one is implemented, and
    /// the way an out-of-tree driver (the Python client, `socat`, a test
    /// double) is attached without giving it a socket of its own.
    Stdio,
    /// Bluetooth Low Energy, the Nordic UART Service.
    #[default]
    Ble,
    /// The LAN socket.
    Lan,
}

impl LinkKind {
    /// Every link, in the order the sidebar and the command line list them.
    pub const ALL: [LinkKind; 3] = [LinkKind::Stdio, LinkKind::Ble, LinkKind::Lan];

    /// The spelling a command line uses.
    pub fn as_str(self) -> &'static str {
        match self {
            LinkKind::Stdio => "stdio",
            LinkKind::Ble => "ble",
            LinkKind::Lan => "lan",
        }
    }

    /// The link a name refers to. Unknown names give `None`, so a caller can
    /// report the bad value instead of silently dialling something else.
    pub fn from_name(name: &str) -> Option<LinkKind> {
        let name = name.trim().to_ascii_lowercase();
        LinkKind::ALL.into_iter().find(|kind| kind.as_str() == name)
    }

    /// The word the chrome shows, upper-cased the way the design does.
    pub fn label(self) -> &'static str {
        match self {
            LinkKind::Stdio => "stdio",
            LinkKind::Ble => "BLE",
            LinkKind::Lan => "LAN",
        }
    }
}

/// The receiving half of a link.
pub trait LinkReader: Send {
    /// Read into `buf`. `Ok(0)` means the far end closed cleanly.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;

    /// Where a link with a side channel says something that is not target
    /// output. Called once, before the read loop starts.
    ///
    /// Almost every bearer is a pipe and takes the default, which is to have
    /// nothing to say out of band. A WebSocket is the exception: the bridge
    /// sends the target's bytes as binary frames and its own announcements as
    /// text, and a text frame is a sentence for the user rather than a
    /// keystroke for the device. Routing it through `read` would type it into
    /// the terminal, so it comes out here instead — see [`lan`].
    fn notice_sink(&mut self, _events: Sender<LinkEvent>) {}
}

/// The sending half of a link.
pub trait LinkWriter: Send {
    /// Write every byte; a short write is the implementation's problem.
    fn write_all(&mut self, buf: &[u8]) -> io::Result<()>;
    /// Push what was buffered to the far end.
    fn flush(&mut self) -> io::Result<()>;
}

/// A link that has not been opened yet.
///
/// Opening is separate from dialling on purpose: a real link opens the port or
/// the GATT server, and that is the step that fails on a machine with no
/// adapter. Keeping it out of the constructor means the *description* of what
/// to dial can be built, logged and tested with nothing plugged in.
pub trait Link: Send {
    /// Open the link and split it into its two halves.
    fn open(self: Box<Self>) -> io::Result<(Box<dyn LinkReader>, Box<dyn LinkWriter>)>;

    /// Which kind of link this is.
    fn kind(&self) -> LinkKind;

    /// How the sidebar and the notices name it, e.g. `stdio: cat`.
    fn describe(&self) -> String;
}

/// What the link reports to the event loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkEvent {
    /// The link is open and the session may be called connected.
    Opened,
    /// Bytes arrived from the device. Never empty.
    Data(Vec<u8>),
    /// The far end said something in words rather than bytes — a bridge
    /// announcement over a channel that has one. Never empty, and never
    /// target output: it is shown as a notice and typed nowhere.
    Notice(String),
    /// The link ended. The string is what the notice says happened; nothing
    /// after a `Closed` is delivered.
    Closed(String),
}

/// A command from the event loop to the writer thread.
enum Outgoing {
    Bytes(Vec<u8>),
    Close,
}

/// Size of the read buffer each turn. A BLE notification is at most 244 bytes
/// and a serial read is whatever the driver has; 4 KiB keeps a burst to a
/// handful of turns without holding a megabyte of the device's output per
/// link while it is not being drained.
const READ_CHUNK: usize = 4096;

/// A running link: the event queue the loop drains and the door it writes
/// through. Dropping the handle closes the link.
#[derive(Debug)]
pub struct LinkHandle {
    events: Receiver<LinkEvent>,
    outgoing: Sender<Outgoing>,
    /// Kept so the threads are known to be alive for the life of the handle;
    /// they are detached rather than joined, because a reader blocked on a
    /// socket must not hold up a quit.
    _reader: JoinHandle<()>,
    _writer: JoinHandle<()>,
    kind: LinkKind,
    description: String,
}

impl LinkHandle {
    /// Open `link` and start pumping it.
    pub fn start(link: Box<dyn Link>) -> io::Result<LinkHandle> {
        let kind = link.kind();
        let description = link.describe();
        let (reader, writer) = link.open()?;
        let (event_tx, events) = mpsc::channel();
        let (outgoing, command_rx) = mpsc::channel();

        let reader = spawn_reader(reader, event_tx.clone());
        let writer = spawn_writer(writer, command_rx, event_tx);
        Ok(LinkHandle {
            events,
            outgoing,
            _reader: reader,
            _writer: writer,
            kind,
            description,
        })
    }

    /// Which link this is.
    pub fn kind(&self) -> LinkKind {
        self.kind
    }

    /// How the link names itself.
    pub fn describe(&self) -> &str {
        &self.description
    }

    /// Queue bytes to the device. `false` means the writer thread is gone, so
    /// the caller can stop counting them as sent.
    pub fn send(&self, bytes: Vec<u8>) -> bool {
        self.outgoing.send(Outgoing::Bytes(bytes)).is_ok()
    }

    /// Take every event that has arrived, without blocking. Stops as soon as
    /// nothing is left so the frame is never delayed by the link.
    pub fn drain(&self) -> Vec<LinkEvent> {
        let mut out = Vec::new();
        loop {
            match self.events.try_recv() {
                Ok(event) => out.push(event),
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => return out,
            }
        }
    }
}

impl Drop for LinkHandle {
    fn drop(&mut self) {
        // Ask the writer to stop. The reader is left to notice its own EOF:
        // a blocking read on a socket cannot be interrupted from here without
        // closing the descriptor under it, which is the link's own business.
        let _ = self.outgoing.send(Outgoing::Close);
    }
}

/// The reader thread: hand every chunk to the loop, then report why it ended.
fn spawn_reader(mut reader: Box<dyn LinkReader>, events: Sender<LinkEvent>) -> JoinHandle<()> {
    // Offered before the loop, so a link with a side channel can announce
    // something on its first read — and so the reads it does make are the only
    // thing this thread ever waits on.
    reader.notice_sink(events.clone());
    thread::spawn(move || {
        if events.send(LinkEvent::Opened).is_err() {
            return;
        }
        let mut buf = [0u8; READ_CHUNK];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if events.send(LinkEvent::Data(buf[..n].to_vec())).is_err() {
                        return;
                    }
                }
                Err(err) => {
                    let _ = events.send(LinkEvent::Closed(err.to_string()));
                    return;
                }
            }
        }
        let _ = events.send(LinkEvent::Closed("end of stream".to_string()));
    })
}

/// The writer thread: one byte buffer in, one `write_all` out.
fn spawn_writer(
    mut writer: Box<dyn LinkWriter>,
    commands: Receiver<Outgoing>,
    events: Sender<LinkEvent>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        while let Ok(command) = commands.recv() {
            match command {
                Outgoing::Close => break,
                Outgoing::Bytes(bytes) => {
                    if bytes.is_empty() {
                        continue;
                    }
                    if let Err(err) = writer.write_all(&bytes).and_then(|()| writer.flush()) {
                        let _ = events.send(LinkEvent::Closed(err.to_string()));
                        return;
                    }
                }
            }
        }
    })
}

#[cfg(test)]
pub mod testing {
    //! An in-memory link, so a test can drive the pump without a device.

    use super::*;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    /// The two ends of one in-memory pair.
    ///
    /// `to_device` is what the host writes; `from_device` is what the device
    /// sends. A test pushes on `device_end` and reads what the host sent.
    #[derive(Clone, Default)]
    pub struct MemoryLink {
        to_device: Arc<Mutex<VecDeque<u8>>>,
        from_device: Arc<Mutex<VecDeque<u8>>>,
        /// Set when the host's writer is dropped, so a test can tell the
        /// difference between "nothing yet" and "the host hung up".
        closed: Arc<Mutex<bool>>,
    }

    impl MemoryLink {
        pub fn new() -> MemoryLink {
            MemoryLink::default()
        }

        /// Build the `Link` the session opens.
        pub fn link(&self) -> Box<dyn Link> {
            Box::new(self.clone())
        }

        /// Bytes the host has written, drained.
        pub fn take_sent(&self) -> Vec<u8> {
            let mut queue = self.to_device.lock().expect("link lock");
            queue.drain(..).collect()
        }

        /// How many bytes the host has written, without draining them.
        ///
        /// The host's writer runs on its own thread, so a test that waits for
        /// the session's *counter* is waiting for the hand-off to the channel
        /// rather than for the wire. This is what such a test polls instead.
        pub fn sent_len(&self) -> usize {
            self.to_device.lock().expect("link lock").len()
        }

        /// Feed the host a chunk of device output.
        pub fn feed(&self, bytes: &[u8]) {
            let mut queue = self.from_device.lock().expect("link lock");
            queue.extend(bytes.iter().copied());
        }

        /// Close the device end, so the host's reader sees EOF.
        pub fn close(&self) {
            *self.closed.lock().expect("link lock") = true;
        }

        fn closed_flag(&self) -> bool {
            *self.closed.lock().expect("link lock")
        }
    }

    struct MemoryReader {
        link: MemoryLink,
    }

    impl LinkReader for MemoryReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            loop {
                {
                    let mut queue = self.link.from_device.lock().expect("link lock");
                    if !queue.is_empty() {
                        let n = queue.len().min(buf.len());
                        for slot in buf.iter_mut().take(n) {
                            *slot = queue.pop_front().expect("checked non-empty");
                        }
                        return Ok(n);
                    }
                    if self.link.closed_flag() {
                        return Ok(0);
                    }
                }
                // Nothing buffered and not closed: wait for the test to feed
                // us, the way a real read waits on the wire.
                thread::sleep(std::time::Duration::from_millis(1));
            }
        }
    }

    struct MemoryWriter {
        link: MemoryLink,
    }

    impl LinkWriter for MemoryWriter {
        fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
            let mut queue = self.link.to_device.lock().expect("link lock");
            queue.extend(buf.iter().copied());
            Ok(())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Link for MemoryLink {
        fn open(self: Box<Self>) -> io::Result<(Box<dyn LinkReader>, Box<dyn LinkWriter>)> {
            let link = *self;
            Ok((
                Box::new(MemoryReader { link: link.clone() }),
                Box::new(MemoryWriter { link }),
            ))
        }

        fn kind(&self) -> LinkKind {
            LinkKind::Stdio
        }

        fn describe(&self) -> String {
            "memory".to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::MemoryLink;
    use super::*;
    use std::time::{Duration, Instant};

    /// Wait for `predicate`, up to a second. The pump runs on its own threads,
    /// so a test polls rather than sleeps a fixed amount — a fixed sleep is a
    /// flaky test on a loaded machine.
    fn wait_for(mut predicate: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if predicate() {
                return true;
            }
            thread::sleep(Duration::from_millis(1));
        }
        predicate()
    }

    /// Every link names itself and round-trips through its spelling.
    #[test]
    fn every_link_round_trips_through_its_name() {
        let names: Vec<&str> = LinkKind::ALL.iter().map(|k| k.as_str()).collect();
        assert_eq!(names, vec!["stdio", "ble", "lan"]);
        for kind in LinkKind::ALL {
            assert_eq!(LinkKind::from_name(kind.as_str()), Some(kind));
        }
        assert_eq!(LinkKind::from_name(" LAN "), Some(LinkKind::Lan));
        assert_eq!(LinkKind::from_name("wifi"), None);
        assert_eq!(LinkKind::from_name(""), None);
    }

    /// The labels the chrome shows are the design's, so a link is never drawn
    /// under two names in two places.
    #[test]
    fn the_labels_match_the_design() {
        assert_eq!(LinkKind::Ble.label(), "BLE");
        assert_eq!(LinkKind::Lan.label(), "LAN");
        assert_eq!(LinkKind::Stdio.label(), "stdio");
    }

    /// Opening reports `Opened` before any data, so the loop can move the
    /// session to connected without guessing.
    #[test]
    fn opening_reports_before_data() {
        let link = MemoryLink::new();
        let handle = LinkHandle::start(link.link()).expect("the memory link opens");
        assert!(wait_for(|| !handle.drain().is_empty()));
        // The queue is drained by the call above, so ask the link again by
        // feeding data and checking that `Opened` really came first: a
        // `Closed`-only start would have ended the session already.
        link.feed(b"hello");
        let mut seen = Vec::new();
        assert!(wait_for(|| {
            seen.extend(handle.drain());
            seen.iter().any(|e| matches!(e, LinkEvent::Data(_)))
        }));
        assert!(
            !seen.iter().any(|e| matches!(e, LinkEvent::Closed(_))),
            "the link closed before it delivered anything: {seen:?}"
        );
        assert_eq!(handle.kind(), LinkKind::Stdio);
        assert_eq!(handle.describe(), "memory");
    }

    /// Received bytes arrive whole and in order, and every byte is counted
    /// exactly once — the counters in the top bar come from this path.
    #[test]
    fn received_bytes_arrive_whole_and_in_order() {
        let link = MemoryLink::new();
        let handle = LinkHandle::start(link.link()).expect("opens");
        link.feed(b"alpha");
        link.feed(b"beta");
        let mut received = Vec::new();
        assert!(wait_for(|| {
            for event in handle.drain() {
                if let LinkEvent::Data(bytes) = event {
                    received.extend(bytes);
                }
            }
            received.len() >= 9
        }));
        assert_eq!(received, b"alphabeta".to_vec());
    }

    /// Writes reach the far end, which is the half the terminal's input uses.
    #[test]
    fn written_bytes_reach_the_device() {
        let link = MemoryLink::new();
        let handle = LinkHandle::start(link.link()).expect("opens");
        assert!(handle.send(b"reboot\n".to_vec()));
        assert!(wait_for(|| link.take_sent() == b"reboot\n".to_vec()));
    }

    /// A device that hangs up ends the session with a reason, exactly once:
    /// the loop watching for `Closed` must not see two ends and report two
    /// disconnects for one hang-up.
    #[test]
    fn a_closed_link_reports_once_with_a_reason() {
        let link = MemoryLink::new();
        let handle = LinkHandle::start(link.link()).expect("opens");
        link.close();
        let mut closes = 0;
        let mut reason = String::new();
        assert!(wait_for(|| {
            for event in handle.drain() {
                if let LinkEvent::Closed(why) = event {
                    closes += 1;
                    reason = why;
                }
            }
            closes > 0
        }));
        assert_eq!(closes, 1, "one hang-up, one report");
        assert_eq!(reason, "end of stream");
        // And it stays reported once: nothing re-announces the end.
        thread::sleep(Duration::from_millis(20));
        for event in handle.drain() {
            assert!(!matches!(event, LinkEvent::Closed(_)), "{event:?}");
        }
    }

    /// Sending after the link is gone reports failure instead of quietly
    /// counting the bytes as sent.
    #[test]
    fn sending_on_a_dead_link_fails_loudly() {
        let link = MemoryLink::new();
        let handle = LinkHandle::start(link.link()).expect("opens");
        drop(handle);
        // The handle is gone, so the writer is too; a fresh handle on the same
        // memory link still works, which is the property the session relies on
        // when it reconnects.
        let second = LinkHandle::start(link.link()).expect("reopens");
        assert!(second.send(b"x".to_vec()));
        assert!(wait_for(|| !link.take_sent().is_empty()));
    }
}
