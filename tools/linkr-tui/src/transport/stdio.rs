//! The process bridge: a child process's standard input and output carry the
//! device's bytes.
//!
//! This is the first real link, and it is the one that makes everything else
//! checkable. A radio cannot be exercised in CI and a serial port cannot be
//! exercised on a laptop with nothing plugged into it, but a *program* can:
//! the same `Link` trait the BLE and serial links will implement is driven here
//! by `cat`, by a shell one-liner, by the Python host client, or by the built-in
//! loopback below. So the pump, the journal, the counters and the disconnect
//! path are all real from the first commit, and swapping in a radio later
//! changes which type is constructed and nothing else.
//!
//! Three shapes, all one link:
//!
//! - [`StdioBridge::new`] wraps any reader and writer this process already has.
//!   That is what the tests use, and what a future `--from-fd` would use.
//! - [`StdioBridge::spawn`] starts a program and bridges over its stdio. The
//!   command is a shell word list, so `--bridge "python3 host.py"` works the
//!   way a user expects without this module shipping a shell parser.
//! - [`StdioBridge::loopback`] is a device that does not exist: a banner and a
//!   shell that answers a handful of commands. It is what `--bridge -` runs, so
//!   a frame with real content can be looked at on a machine with no hardware
//!   attached — which is exactly the situation this link was built for.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::{Link, LinkKind, LinkReader, LinkWriter};

/// A link over a process's standard streams.
pub enum StdioBridge {
    /// Streams this process already holds. Boxed because the two halves are
    /// consumed separately and neither is named again.
    Pipes {
        reader: Box<dyn Read + Send>,
        writer: Box<dyn Write + Send>,
        describe: String,
    },
    /// A program to start and bridge over. Kept unspawned until [`Link::open`],
    /// so a failed dial is reported where a failed dial belongs.
    Command(String),
    /// The built-in device that is not a device.
    Loopback,
}

impl StdioBridge {
    /// Bridge over a reader and a writer this process already owns.
    pub fn new(
        reader: impl Read + Send + 'static,
        writer: impl Write + Send + 'static,
        describe: impl Into<String>,
    ) -> StdioBridge {
        StdioBridge::Pipes {
            reader: Box::new(reader),
            writer: Box::new(writer),
            describe: describe.into(),
        }
    }

    /// The description a spawned command is dialled under.
    pub fn command_description(command: &str) -> String {
        format!("stdio: {command}")
    }

    /// Start `command` and bridge over its standard streams.
    pub fn spawn(command: impl Into<String>) -> StdioBridge {
        StdioBridge::Command(command.into())
    }

    /// The built-in loopback device, as `--bridge -` selects it.
    pub fn loopback() -> StdioBridge {
        StdioBridge::Loopback
    }

    /// Split `command` into a program and its arguments, the way a shell would
    /// split it — spaces only, with a backslash escaping the next character.
    /// No quoting is invented: a command that needs quotes needs a shell, and
    /// the caller can write `sh -c '...'` for that.
    fn words(command: &str) -> io::Result<Vec<String>> {
        let mut words = Vec::new();
        let mut word = String::new();
        let mut escaped = false;
        for ch in command.chars() {
            if escaped {
                word.push(ch);
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch.is_whitespace() {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            } else {
                word.push(ch);
            }
        }
        if escaped {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the command ends with a trailing backslash",
            ));
        }
        if !word.is_empty() {
            words.push(word);
        }
        if words.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "no command to bridge over",
            ));
        }
        Ok(words)
    }
}

impl Link for StdioBridge {
    fn open(self: Box<Self>) -> io::Result<(Box<dyn LinkReader>, Box<dyn LinkWriter>)> {
        match *self {
            StdioBridge::Pipes { reader, writer, .. } => Ok((
                Box::new(StreamReader { inner: reader }),
                Box::new(StreamWriter {
                    inner: writer,
                    child: None,
                }),
            )),
            StdioBridge::Command(command) => {
                let words = StdioBridge::words(&command)?;
                let mut child = Command::new(&words[0])
                    .args(&words[1..])
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::inherit())
                    .spawn()
                    .map_err(|err| {
                        io::Error::new(err.kind(), format!("could not start {command:?}: {err}"))
                    })?;
                let stdout = child.stdout.take().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::BrokenPipe, "the bridge has no stdout")
                })?;
                let stdin = child.stdin.take().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::BrokenPipe, "the bridge has no stdin")
                })?;
                Ok((
                    Box::new(StreamReader {
                        inner: Box::new(stdout),
                    }),
                    Box::new(StreamWriter {
                        inner: Box::new(stdin),
                        child: Some(Box::new(child)),
                    }),
                ))
            }
            StdioBridge::Loopback => {
                let device = Arc::new(LoopbackDevice::default());
                let (sender, receiver) = std::sync::mpsc::channel();
                Ok((
                    Box::new(DeviceReader {
                        device: Arc::clone(&device),
                        outbox: receiver,
                        pending: VecDeque::new(),
                        greeted: false,
                        sender,
                    }),
                    Box::new(DeviceWriter { device }),
                ))
            }
        }
    }

    fn kind(&self) -> LinkKind {
        LinkKind::Stdio
    }

    fn describe(&self) -> String {
        match self {
            StdioBridge::Pipes { describe, .. } => describe.clone(),
            StdioBridge::Command(command) => StdioBridge::command_description(command),
            StdioBridge::Loopback => "stdio: loopback".to_string(),
        }
    }
}

/// A reader over any `Read`.
struct StreamReader {
    inner: Box<dyn Read + Send>,
}

impl LinkReader for StreamReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}

/// A writer over any `Write`, holding the child so it can be reaped.
struct StreamWriter {
    inner: Box<dyn Write + Send>,
    child: Option<Box<Child>>,
}

impl LinkWriter for StreamWriter {
    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        self.inner.write_all(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl Drop for StreamWriter {
    fn drop(&mut self) {
        // Closing the bridge's input is the polite hang-up; a program that
        // ignores it is killed, because a leaked child holding the radio is
        // worse than a killed one.
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

// --- the device that is not a device -----------------------------------------

/// A tiny shell standing in for the board, so `--bridge -` shows a populated
/// terminal on a machine with no hardware.
#[derive(Default)]
struct LoopbackDevice {
    /// Commands typed but not yet terminated by a newline.
    line: Mutex<String>,
    /// What the "device" has to say next.
    outbox: Mutex<VecDeque<u8>>,
    /// The channel to wake a blocked reader with.
    waker: Mutex<Option<std::sync::mpsc::Sender<()>>>,
}

impl LoopbackDevice {
    fn push(&self, text: &str) {
        if let Ok(mut outbox) = self.outbox.lock() {
            outbox.extend(text.as_bytes().iter().copied());
        }
        if let Ok(waker) = self.waker.lock() {
            if let Some(sender) = waker.as_ref() {
                let _ = sender.send(());
            }
        }
    }

    fn banner(&self) {
        self.push(
            "Linkr Bee loopback device (no radio attached)\r\n\
             Type a command; try: help, version, uname -a, df -h\r\n\
             linkr@bee:~$ ",
        );
    }

    /// The reply to one complete line, as the board would print it.
    fn reply(&self, line: &str) {
        let command = line.trim();
        let answer = match command {
            "" => String::new(),
            "help" => "help version uname df reboot\r\n".to_string(),
            "version" => "linkr-bee 1.0.0 (loopback)\r\n".to_string(),
            "uname -a" => "Linux bee 6.6.0-linkr #1 SMP aarch64 GNU/Linux\r\n".to_string(),
            "df -h" => "Filesystem      Size  Used Avail Use% Mounted on\r\n\
                 /dev/root        28G  7.4G   19G  29% /\r\n\
                 tmpfs           196M     0  196M   0% /run\r\n"
                .to_string(),
            other => format!("sh: {other}: not found\r\n"),
        };
        self.push(&answer);
        self.push("linkr@bee:~$ ");
    }
}

struct DeviceWriter {
    device: Arc<LoopbackDevice>,
}

impl LinkWriter for DeviceWriter {
    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        for byte in buf {
            match *byte {
                b'\r' | b'\n' => {
                    let line = self
                        .device
                        .line
                        .lock()
                        .map(|mut line| std::mem::take(&mut *line))
                        .unwrap_or_default();
                    // The far end of a serial line is a terminal, so the echo
                    // is CRLF like the device's own output.
                    self.device.push("\r\n");
                    if !line.trim().is_empty() {
                        self.device.reply(&line);
                    } else {
                        self.device.push("linkr@bee:~$ ");
                    }
                }
                0x08 | 0x7f => {
                    if let Ok(mut line) = self.device.line.lock() {
                        line.pop();
                    }
                    self.device.push("\x08 \x08");
                }
                byte if byte.is_ascii_graphic() || byte == b' ' => {
                    if let Ok(mut line) = self.device.line.lock() {
                        line.push(byte as char);
                    }
                    self.device.push(&(byte as char).to_string());
                }
                // Anything else (a control key, a non-ASCII byte) is echoed
                // raw so the pane still shows what was sent.
                0x09 => self.device.push("\t"),
                other => self.device.push(&String::from_utf8_lossy(&[other])),
            }
        }
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct DeviceReader {
    device: Arc<LoopbackDevice>,
    outbox: std::sync::mpsc::Receiver<()>,
    pending: VecDeque<u8>,
    greeted: bool,
    sender: std::sync::mpsc::Sender<()>,
}

impl LinkReader for DeviceReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if !self.greeted {
            self.greeted = true;
            *self.device.waker.lock().expect("waker lock") = Some(self.sender.clone());
            self.device.banner();
        }
        loop {
            if self.pending.is_empty() {
                let drained: VecDeque<u8> = {
                    let mut outbox = self.device.outbox.lock().expect("outbox lock");
                    std::mem::take(&mut *outbox)
                };
                self.pending = drained;
            }
            if !self.pending.is_empty() {
                let n = self.pending.len().min(buf.len());
                for slot in buf.iter_mut().take(n) {
                    *slot = self.pending.pop_front().expect("checked non-empty");
                }
                return Ok(n);
            }
            // Nothing to say: wait for the writer to wake us, the way a real
            // read waits on the wire. A timeout keeps the shutdown path from
            // hanging if the wake-up is missed.
            match self.outbox.recv_timeout(Duration::from_millis(50)) {
                Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Ok(0),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{LinkEvent, LinkHandle};
    use std::thread;
    use std::time::{Duration, Instant};

    fn wait_for(mut predicate: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if predicate() {
                return true;
            }
            thread::sleep(Duration::from_millis(2));
        }
        predicate()
    }

    /// Drain a handle into everything received, up to `want` bytes.
    fn collect_until(handle: &LinkHandle, want: usize) -> (Vec<u8>, Vec<LinkEvent>) {
        let mut bytes = Vec::new();
        let mut all = Vec::new();
        wait_for(|| {
            for event in handle.drain() {
                if let LinkEvent::Data(ref chunk) = event {
                    bytes.extend(chunk.iter().copied());
                }
                all.push(event);
            }
            bytes.len() >= want
        });
        (bytes, all)
    }

    /// Drain a handle until what has arrived contains `needle`.
    ///
    /// Waiting for a *needle* rather than for a byte count is what keeps the
    /// loopback tests off the clock. The device's banner and the reply to the
    /// first command can land in the same drain, so a test that waits for N
    /// bytes can be satisfied by the banner and then assert on a reply it never
    /// received — which is a race, and it failed exactly that way under load.
    fn collect_text(handle: &LinkHandle, needle: &str) -> Vec<u8> {
        let mut bytes = Vec::new();
        wait_for(|| {
            for event in handle.drain() {
                if let LinkEvent::Data(ref chunk) = event {
                    bytes.extend(chunk.iter().copied());
                }
            }
            String::from_utf8_lossy(&bytes).contains(needle)
        });
        bytes
    }

    /// A command is split the way a user writes it, and a command that cannot
    /// be split is refused rather than started wrongly.
    #[test]
    fn a_command_splits_into_words() {
        assert_eq!(
            StdioBridge::words("python3 host.py --reliable").expect("splits"),
            vec!["python3", "host.py", "--reliable"]
        );
        assert_eq!(
            StdioBridge::words("  cat   ").expect("splits"),
            vec!["cat"],
            "surrounding and repeated spaces carry no words"
        );
        assert_eq!(
            StdioBridge::words(r"sh -c echo\ hi").expect("splits"),
            vec!["sh", "-c", "echo hi"],
            "a backslash joins words"
        );
        for bad in ["", "   ", r"cat \"] {
            assert!(StdioBridge::words(bad).is_err(), "{bad:?} was accepted");
        }
    }

    /// The link names itself, and the name is what the sidebar shows.
    #[test]
    fn the_link_names_itself() {
        let bridge = StdioBridge::loopback();
        assert_eq!(bridge.kind(), LinkKind::Stdio);
        assert_eq!(bridge.describe(), "stdio: loopback");
        let spawned = StdioBridge::spawn("cat");
        assert_eq!(spawned.describe(), "stdio: cat");
    }

    /// A reader and a writer this process already owns are bridged verbatim:
    /// what is written comes out and what is fed comes back.
    #[test]
    fn a_pair_of_streams_is_bridged() {
        // Two pipes: one the host writes and the "device" reads, one the other
        // way round.
        let (mut device_input, host_to_device) = std::io::pipe().expect("pipe");
        let (host_from_device, mut device_output) = std::io::pipe().expect("pipe");
        let handle = LinkHandle::start(Box::new(StdioBridge::new(
            host_from_device,
            host_to_device,
            "test pipes",
        )))
        .expect("opens");

        handle.send(b"ping\n".to_vec());
        let mut got = [0u8; 5];
        assert!(wait_for(|| device_input.read_exact(&mut got).is_ok()));
        assert_eq!(&got, b"ping\n");

        device_output.write_all(b"pong\n").expect("writes");
        let (bytes, _) = collect_until(&handle, 5);
        assert_eq!(bytes, b"pong\n".to_vec());
    }

    /// A child process is really started and really spoken to: the stdio
    /// bridge is not a mock. `cat` is the smallest program that proves both
    /// directions, and it is present everywhere this test can run.
    #[test]
    fn a_child_process_is_bridged_both_ways() {
        let handle = LinkHandle::start(Box::new(StdioBridge::spawn("cat"))).expect("spawns cat");
        handle.send(b"hello wire\n".to_vec());
        let (bytes, events) = collect_until(&handle, 11);
        assert_eq!(bytes, b"hello wire\n".to_vec());
        assert!(
            events.iter().any(|e| matches!(e, LinkEvent::Opened)),
            "the link never opened: {events:?}"
        );
        assert!(
            !events.iter().any(|e| matches!(e, LinkEvent::Closed(_))),
            "the bridge closed while it was still in use: {events:?}"
        );
    }

    /// A program that does not exist fails at open, with the command in the
    /// message, rather than opening a link that silently swallows everything.
    #[test]
    fn a_missing_program_fails_at_open() {
        let err = LinkHandle::start(Box::new(StdioBridge::spawn("linkr-no-such-program")))
            .expect_err("nothing to start");
        assert!(
            err.to_string().contains("linkr-no-such-program"),
            "the error does not say what was dialled: {err}"
        );
    }

    /// The loopback answers the commands it documents, and the session sees a
    /// device that talks back — which is the whole point of shipping it.
    #[test]
    fn the_loopback_answers_a_command() {
        let handle = LinkHandle::start(Box::new(StdioBridge::loopback())).expect("opens");
        // The prompt is the last thing in the banner, so waiting for it means
        // the banner is over before the command goes out.
        let banner = collect_text(&handle, "linkr@bee:~$ ");
        assert!(
            String::from_utf8_lossy(&banner).contains("loopback device"),
            "no banner: {:?}",
            String::from_utf8_lossy(&banner)
        );

        handle.send(b"uname -a\n".to_vec());
        let reply = collect_text(&handle, "aarch64");
        let text = String::from_utf8_lossy(&reply);
        assert!(text.contains("uname -a"), "no echo: {text:?}");
        assert!(text.contains("aarch64"), "no answer: {text:?}");
    }

    /// An unknown command is reported the way a shell reports it, so the pane
    /// shows a plausible device rather than an empty one.
    #[test]
    fn the_loopback_reports_an_unknown_command() {
        let handle = LinkHandle::start(Box::new(StdioBridge::loopback())).expect("opens");
        let _ = collect_text(&handle, "linkr@bee:~$ ");
        handle.send(b"nonsense\n".to_vec());
        let reply = collect_text(&handle, "not found");
        let text = String::from_utf8_lossy(&reply);
        assert!(text.contains("not found"), "{text:?}");
    }
}
