//! Where the bytes on the wire are recorded, when somebody asked for them.
//!
//! Two switches, one shape. `--log-file` appends what the device sent to a
//! file, which is how a session is kept: a console log is evidence, and the
//! pane scrolls what it showed out of existence. `--debug-io` traces both
//! directions to stderr, which is how a wire is *debugged* — the question it
//! answers is not "what did the board say" but "did what I typed get there".
//!
//! The two are deliberately different sinks for the same reason they are
//! different flags. A log file is opened once and appended to for the whole
//! session, so it survives the program; a trace is read while the program runs
//! and is worthless afterwards. Nothing is buffered in either: a trace that
//! loses its last line because the process exited is a trace missing exactly
//! the moment that mattered.

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::Path;

/// The recording, for a session that may not have asked for one.
#[derive(Debug, Default)]
pub struct ByteTrace {
    /// Where received bytes are appended, when `--log-file` named one.
    file: Option<File>,
    /// Whether both directions are traced to standard error.
    stderr: bool,
}

impl ByteTrace {
    /// A trace that records nothing, which is the default: a program that
    /// wrote a log file because nobody asked would be filling disks.
    pub fn none() -> ByteTrace {
        ByteTrace {
            file: None,
            stderr: false,
        }
    }

    /// Append what the device sends to `path`, creating it when it is not
    /// there.
    ///
    /// Append rather than truncate on purpose: a log that lost yesterday's
    /// session when today's started is a log nobody can compare against, and
    /// comparing is the whole point of keeping one.
    pub fn to_file(path: &Path) -> std::io::Result<ByteTrace> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(ByteTrace {
            file: Some(file),
            stderr: false,
        })
    }

    /// Also trace both directions to standard error.
    pub fn with_stderr(mut self) -> ByteTrace {
        self.stderr = true;
        self
    }

    /// Whether anything is being recorded at all, which the caller asks so a
    /// session with no trace does not pay for one — and, more to the point, so
    /// a test can assert that it did not.
    pub fn is_active(&self) -> bool {
        self.file.is_some() || self.stderr
    }

    /// Whether both directions are being traced to standard error, which the
    /// palette reads to know which way its switch points.
    pub fn tracing_stderr(&self) -> bool {
        self.stderr
    }

    /// Turn the standard-error trace on or off while the session runs.
    ///
    /// A switch rather than a constructor-only option because the question a
    /// trace answers — "did what I typed get there" — is usually asked *after*
    /// something went wrong, and a switch that needed a restart would make the
    /// user reproduce the problem to see it.
    pub fn set_stderr(&mut self, on: bool) {
        self.stderr = on;
    }

    /// Bytes that arrived from the device.
    pub fn rx(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        if let Some(file) = self.file.as_mut() {
            // A full disk or a closed pipe must not end the session: the log is
            // a record of the session, not a condition of it.
            let _ = file.write_all(bytes);
            let _ = file.flush();
        }
        if self.stderr {
            eprintln!("rx {} bytes: {}", bytes.len(), debugged(bytes));
        }
    }

    /// Bytes that left for the device down the stream, keystrokes and all.
    pub fn tx(&mut self, bytes: &[u8]) {
        if self.stderr && !bytes.is_empty() {
            eprintln!("tx {} bytes: {}", bytes.len(), debugged(bytes));
        }
    }

    /// A management command that left for the bridge.
    ///
    /// Taken already redacted, because this is the one trace that would
    /// otherwise put a WiFi password on stderr — and stderr is scrollback, a
    /// terminal recording, or a CI log, none of which is a place a password
    /// belongs. See [`crate::accessory::redact_command`].
    pub fn tx_command(&mut self, command: &str) {
        if self.stderr && !command.is_empty() {
            eprintln!("tx command: {command}");
        }
    }
}

/// How bytes are shown: printable ones as themselves and the rest as `\xNN`,
/// which is the shape the reference client prints and the shape that makes a
/// mangled echo obvious at a glance. `A` and `\x41` are the same byte, but
/// only one of them looks like a wire problem.
pub fn debugged(bytes: &[u8]) -> String {
    let mut out = String::new();
    for byte in bytes {
        if byte.is_ascii_graphic() || *byte == b' ' {
            out.push(*byte as char);
        } else {
            out.push_str(&format!("\\x{byte:02x}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Printable bytes read as themselves and the rest as escapes, which is
    /// what makes a mangled echo obvious: `A` and `\x41` are the same byte, but
    /// only one of them looks like a wire problem.
    #[test]
    fn bytes_are_shown_as_themselves_when_they_can_be() {
        assert_eq!(debugged(b"ok"), "ok");
        assert_eq!(debugged(b"a\nb"), "a\\x0ab");
        assert_eq!(debugged(b""), "");
    }

    /// A log file is appended to, not truncated: a log that lost yesterday's
    /// session when today's started is one nobody can compare against, and
    /// comparing is the point of keeping it.
    #[test]
    fn a_log_file_is_appended_to_and_not_truncated() {
        let dir = std::env::temp_dir().join(format!("linkr-trace-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("rx.log");
        let _ = std::fs::remove_file(&path);

        {
            let mut trace = ByteTrace::to_file(&path).expect("the file opens");
            assert!(trace.is_active(), "a file is being written");
            trace.rx(b"first");
        }
        {
            let mut trace = ByteTrace::to_file(&path).expect("the file opens again");
            trace.rx(b"-second");
        }
        let kept = std::fs::read(&path).expect("the file reads back");
        assert_eq!(String::from_utf8_lossy(&kept), "first-second");

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A trace that is not active records nothing, so a session with no
    /// `--log-file` and no `--debug-io` does not touch the disk at all.
    #[test]
    fn a_trace_nobody_asked_for_is_inactive() {
        let mut trace = ByteTrace::none();
        assert!(!trace.is_active());
        // Called so that an inactive trace is proven not to panic rather than
        // merely not to open a file.
        trace.rx(b"ignored");
        trace.tx(b"ignored");
        trace.tx_command("@w=ssid,secret");
        assert!(ByteTrace::none().with_stderr().is_active());
    }

    /// The standard-error trace can be switched on and off while the session
    /// runs: the question it answers is asked after something already went
    /// wrong, and a switch that needed a restart would make the user reproduce
    /// the problem to watch it.
    #[test]
    fn the_stderr_trace_switches_at_runtime() {
        let mut trace = ByteTrace::none();
        assert!(!trace.tracing_stderr(), "off until asked for");
        trace.set_stderr(true);
        assert!(trace.tracing_stderr());
        assert!(trace.is_active(), "a trace without a file is still a trace");
        trace.set_stderr(false);
        assert!(!trace.tracing_stderr());
        assert!(!trace.is_active(), "and back to recording nothing");
    }
}
