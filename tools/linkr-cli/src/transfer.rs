//! File transfer over the console link: ZMODEM first, the `dd | base64`
//! pager of [`crate::target_files`] as the fallback for a target that has no
//! `lrzsz` installed.
//!
//! Two things decide the design.
//!
//! **The link is a byte stream with a small queue in front of it.** A burst
//! over about 1 KiB loses bytes through the bridge (`dist/paste_integrity.py`:
//! 800 B clean, 1600 B loses 136), so nothing here ever hands the session a
//! big write: every outbound byte goes out through [`Transfer::pump`] in
//! [`CHUNK_BYTES`] slices separated by [`CHUNK_INTERVAL`]. Slow on purpose —
//! a retransmit costs more than waiting did.
//!
//! **The console is a shell, not a pipe.** A transfer therefore begins by
//! *typing* one line and ends when the target's answer has come back, which is
//! why every command is wrapped with its own sequence number: the marker that
//! finishes a step carries the number of the step that asked for it, so an
//! answer that arrives late — or the shell's echo of the command that asked —
//! can never be mistaken for the answer to the command in flight.
//!
//! The markers follow the rule [`crate::target_files`] already established: a
//! marker is a whole line that **starts** with the prefix, and no command we
//! type starts with that prefix (they start with `printf`, `if`, `p=`), so the
//! echo of the command cannot match the marker that command waits for.
//!
//! ## Integrity
//!
//! ZMODEM needs no digest of its own and this module does not add one: every
//! frame carries CRC-32, a damaged frame is dropped and retransmitted, and
//! `rz` compares the bytes it wrote against the file length in the ZFILE
//! header before reporting success — `sz`/`rz` exiting 0 *is* the
//! verification. The pager has no such property (a corrupted base64 page
//! decodes into wrong bytes happily), which is exactly what
//! [`target_files::upload_complete_command`] exists for: it re-reads size and
//! digest on the target and refuses to move the part file into place unless
//! they match.

use crate::target_files::{
    encode_base64, format_bytes, marker_lines, parse_file_read, parse_upload_progress,
    parse_upload_result, quote_shell, read_file_command, upload_chunk_command, upload_plan,
    ReadStatus, UploadRequest, UploadStatus, DEFAULT_CHUNK_BYTES, MAX_READ_BYTES,
    MAX_UPLOAD_CHUNKS, PART_SUFFIX,
};
use std::collections::{BTreeMap, VecDeque};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TryRecvError, TrySendError};
use std::time::{Duration, Instant};

/// Prefix of every line this module looks for on its own account. The pager's
/// `LINKR_FILE:` / `LINKR_UPLOAD:` markers are parsed by `target_files`.
pub const MARKER: &str = "LINKR_ZM:";

/// Bytes handed to the session per tick while a transfer runs.
///
/// Deliberately a quarter of the ~1 KiB burst the bridge is known to survive
/// (`dist/paste_integrity.py`: 1600 B loses 136) and just over the ~232 B
/// reliable BLE payload, so one tick is one bridge write rather than a
/// queue-filling blast.
pub const CHUNK_BYTES: usize = 256;

/// Minimum spacing between two chunks: 256 B / 25 ms ≈ 10 KiB/s. Both numbers
/// are constants rather than settings because the safe value is a property of
/// the bridge's queue, not of the person running the TUI.
pub const CHUNK_INTERVAL: Duration = Duration::from_millis(25);

/// How long one typed command may take to answer before the step is called
/// dead. Probe and pager pages answer in milliseconds over either transport;
/// 45 s still covers a target that is busy.
const STEP_TIMEOUT: Duration = Duration::from_secs(45);

/// A run that has not moved a single byte for this long is wedged, not slow.
///
/// This is the check a ceiling could never be: it can say so *while* the
/// transfer is still on screen, instead of one verdict at the end for the
/// whole run. The byte counter only stops growing when the peer has genuinely
/// stopped — `sz` blocks once its pipe fills because the target has stopped
/// acknowledging, and `rz` blocks because nothing arrives — so "no byte has
/// crossed the link for ninety seconds" is the condition a stalled ZMODEM
/// transfer actually has, at any file size.
const STALL_TIMEOUT: Duration = Duration::from_secs(90);

/// Absolute ceiling on one ZMODEM run, far above [`STALL_TIMEOUT`], so even a
/// peer that trickles one byte short of a stall cannot pin the view open for
/// the rest of the session.
///
/// The original 1800 s was a ceiling on the *transfer* rather than on a
/// wedge. Measured end to end the link does about 2 KiB/s — 512 KiB took
/// 316 s on a Bee, with every ATT write waiting out its own round trip — so
/// that budget covered roughly 3.5 MiB and then cut the transfer off while
/// all of its bytes were still arriving. [`STALL_TIMEOUT`] is what bounds a
/// wedged run now; this only bounds an absurd one.
const RUN_TIMEOUT: Duration = Duration::from_secs(7200);

/// After the host's process exits, how long to keep watching for the target's
/// own `rc` line before falling back on the host's clean exit.
///
/// It is the launch step's own budget rather than a grace period, because the
/// gap is not idle time: while the target is still *inside* `rz` that shell
/// reads stdin in raw mode, so anything typed at it is eaten as ZMODEM data —
/// no echo, no execution. The gap was measured on the Bee at **12.8 s** and
/// again at **56.0 s**, both times because the sender's `OO` never reached the
/// receiver (see `pump`). 1.5 s called that `Sent`; the round-trip's receive
/// command was then typed straight into the busy console and the run died on
/// the step timeout. The report is the only thing that proves the console is
/// back, so it gets the whole budget to arrive — and only after it does the
/// host's clean exit stand in for a target that never spoke.
const SETTLE: Duration = STEP_TIMEOUT;

/// The ZMODEM cancel, exactly as lrzsz itself puts it on the wire: ten CAN
/// (0x18) followed by ten backspaces (0x08).
///
/// `^C` alone cannot stop a peer that is *inside* a transfer. The receiver
/// has its terminal in raw mode with `ISIG` off, so the break characters
/// arrive as ordinary data, get swallowed as protocol bytes, and the target
/// carries on holding its console until our own stall timeout gives up on it
/// — which is the "I stopped, but the board never left zmodem" this exists to
/// fix.
///
/// The backspaces come with the CANs because the same bytes are also the
/// courteous thing to send when the peer is *not* in a transfer: they scrub
/// whatever the abandoned run had typed into its command line before `^C`
/// arrives to clear the rest.
///
/// Measured against lrzsz 0.12.21, both halves wired through a throttled
/// relay with the cancel injected three seconds into a live transfer: this
/// sequence ends the session in **both** directions — into a receiver it
/// cancels and the sender follows it out; into a sender it cancels and the
/// receiver follows. `exit 128` on both sides, 2 of 2 trials each. The same
/// bytes do *not* land reliably on a receiver that is still idle waiting for
/// a sender, which is a different read path — so the sequence is only ever
/// sent when a session is genuinely on the wire (see
/// [`Transfer::peer_cancel`]).
const ZMODEM_CANCEL: &[u8] = &[
    0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, //
    0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08,
];

/// Cap on captured command output kept for parsing: the pager's largest page
/// plus its markers, with room to spare. Past this the target is talking to
/// itself, not answering us.
const CAP_LIMIT: usize = 512 * 1024;

/// Upper bound the pager accepts for an upload: `MAX_UPLOAD_CHUNKS` commands
/// of `DEFAULT_CHUNK_BYTES` payload. Larger files need ZMODEM.
const PAGER_LIMIT: u64 = MAX_UPLOAD_CHUNKS as u64 * DEFAULT_CHUNK_BYTES as u64;

/// Which way the file goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Host file → target path.
    Send,
    /// Target path → host file.
    Recv,
}

impl Direction {
    pub fn label(self) -> &'static str {
        match self {
            Direction::Send => "send",
            Direction::Recv => "receive",
        }
    }
}

/// Which of the two channels a run uses. Chosen once, from the probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    /// Host `sz`/`rz` as child processes, the target running the other half.
    Zmodem,
    /// `dd | base64` pages over typed commands (`target_files`).
    Pager,
}

/// What the probe found on the target.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Probe {
    /// `command -v` results, one entry per name in [`PROBE_TOOLS`].
    pub tools: BTreeMap<String, bool>,
    /// First line of `sz --version` / `rz --version`, when there was one.
    pub sz_version: String,
    pub rz_version: String,
    /// The target's `$HOME`, used to expand a leading `~` in a path.
    pub home: String,
    /// Everything the probe printed, for the view's detail line.
    pub raw: String,
}

/// The tools one probe asks about: the ZMODEM pair, then everything the pager
/// reaches for, then the two possible digests.
pub const PROBE_TOOLS: &[&str] = &[
    "sz",
    "rz",
    "dd",
    "base64",
    "wc",
    "tr",
    "sha256sum",
    "shasum",
];

impl Probe {
    pub fn has(&self, name: &str) -> bool {
        self.tools.get(name).copied().unwrap_or(false)
    }

    /// The target's half of the ZMODEM precondition.
    pub fn target_zmodem(&self) -> bool {
        self.has("sz") && self.has("rz")
    }

    /// The pager's precondition: everything `read_file_command` and
    /// `upload_chunk_command` reach for.
    pub fn pager(&self) -> bool {
        ["dd", "base64", "wc", "tr"]
            .iter()
            .all(|name| self.has(name))
    }

    /// A digest is optional. Without one the pager still checks the size, and
    /// the view says "not verified" rather than pretending.
    pub fn digest(&self) -> bool {
        self.has("sha256sum") || self.has("shasum")
    }
}

/// What the *host* has, checked once when the view opens. A missing host copy
/// is a reason to fall back to the pager, not a reason to fail: the pager has
/// no host-side dependency at all.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HostTools {
    pub sz: bool,
    pub rz: bool,
    pub sz_version: String,
    pub rz_version: String,
}

impl HostTools {
    /// Two `--version` spawns, about 5 ms, once per view open.
    pub fn detect() -> Self {
        let mut out = HostTools::default();
        out.sz = first_line("sz", &mut out.sz_version);
        out.rz = first_line("rz", &mut out.rz_version);
        out
    }

    pub fn zmodem(&self) -> bool {
        self.sz && self.rz
    }
}

fn first_line(tool: &str, into: &mut String) -> bool {
    let Ok(output) = Command::new(tool).arg("--version").output() else {
        return false;
    };
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    if let Some(line) = text.lines().find(|line| !line.trim().is_empty()) {
        *into = line.trim().to_string();
    }
    // lrzsz answers 0 for `--version`; a build that does not know the flag
    // prints usage and fails, but the binary is still there.
    !into.is_empty() || output.status.success()
}

/// The probe as one typed line: which tools exist, their versions, and
/// `$HOME`, so a path starting with `~` can be expanded without guessing.
///
/// It starts with `printf`, and that is load-bearing — see the module docs on
/// echoes.
pub fn probe_command() -> String {
    let list = PROBE_TOOLS.join(" ");
    format!(
        "printf '{MARKER}begin\\n'; for t in {list}; do if command -v \"$t\" >/dev/null 2>&1; then printf '{MARKER}have %s\\n' \"$t\"; else printf '{MARKER}no %s\\n' \"$t\"; fi; done; for t in sz rz; do if command -v \"$t\" >/dev/null 2>&1; then \"$t\" --version 2>&1 | while IFS= read -r l; do printf '{MARKER}ver %s %s\\n' \"$t\" \"$l\"; break; done; fi; done; printf '{MARKER}home %s\\n' \"$HOME\""
    )
}

/// The upload: the target starts `rz` inside the directory the form named.
///
/// * `printf go` comes first, so the host knows the exact moment `rz` is up —
///   and it lands *after* this line's own echo, which is therefore already
///   behind us when capture starts.
/// * `2>/dev/null` keeps `rz`'s progress chatter out of the link, where it
///   would be fed straight to `sz`'s stdin.
/// * the guard refuses to clobber an existing file *before* any byte moves,
///   instead of discovering it afterwards — and it has to check **both** paths.
///   `rz -y` writes under the *sender's* file name, so a rename puts two files
///   at risk: the one the form asked for, and the one the sender's name already
///   occupies in that directory. Checking only the first let `rz` overwrite the
///   second and then carry the result over the top of it, so an unrelated
///   `original.bin` was lost to an upload nobody named it in.
/// * `sz` puts the sender's file name in the ZFILE header, which is `local`'s
///   name; when the form asked for a different one, a single `mv` inside the
///   same command settles it. Not a digest — a name. `mv -n`, never `-f`: the
///   rename must not become the very clobber the guard just refused, and since
///   `mv -n` exits 0 whether or not it moved anything, the `[ ! -e ]` behind it
///   is what reports a refusal (the received bytes stay under the sender's
///   name, intact, and the step fails rather than losing them).
fn launch_recv(dest: &str, dir: &str, written: &str) -> Result<String, String> {
    let dest_q = quote_shell(dest).map_err(|err| err.to_string())?;
    let dir_q = quote_shell(dir).map_err(|err| err.to_string())?;
    let written_q = quote_shell(written).map_err(|err| err.to_string())?;
    Ok(format!(
        "if [ -e {dest_q} ]; then printf '{MARKER}exists %s\\n' {dest_q}; false; elif [ {written_q} != {dest_q} ] && [ -e {written_q} ]; then printf '{MARKER}exists %s\\n' {written_q}; false; else printf '{MARKER}go\\n'; (cd {dir_q} && rz -e -O -y 2>/dev/null) && {{ [ {written_q} = {dest_q} ] || {{ mv -n {written_q} {dest_q} && [ ! -e {written_q} ]; }}; }}; fi"
    ))
}

/// The download: the target starts `sz` on the file the form named. The host
/// does the renaming, on its own disk, after the bytes are in.
fn launch_send(source: &str) -> Result<String, String> {
    let source_q = quote_shell(source).map_err(|err| err.to_string())?;
    Ok(format!(
        "if [ ! -e {source_q} ]; then printf '{MARKER}missing %s\\n' {source_q}; false; else printf '{MARKER}go\\n'; (sz -e -O -q -- {source_q}); fi"
    ))
}

/// Wrap a body so it reports its own exit status under this step's sequence
/// number. The result is one line, and it never *begins* with [`MARKER`].
///
/// The report opens a line of its own, and it has to: what precedes it is not
/// blank. Once `rz`/`sz` has held the port, the target's last byte before the
/// report is the end of a ZMODEM frame (`\x8a`) and **not** a newline, so a
/// bare `printf '…rc…'` would land glued to `**\x18B…`. [`Transfer::on_marker`]
/// only reads a marker at position zero — that is what keeps the target's own
/// echo from answering — so the report would go unread, `launch_rc` would stay
/// `None`, and the run would end on the [`SETTLE`] fallback instead of on the
/// target's word. Measured: the Bee's `rc` arrived glued, 12.8 s late.
fn wrap(seq: u32, body: &str) -> String {
    format!("{body}; printf '\\n{MARKER}rc {seq} %s\\n' \"$?\"")
}

/// Drop the control noise a console prints around a line, leaving what the
/// line actually says: escape sequences anywhere, then the control characters
/// and blanks sitting in front of it.
///
/// [`Transfer::on_marker`] matches `MARKER` at the *start* of a line, which
/// is what keeps the target's own echo from answering — the echoed command
/// carries `MARKER` inside its `printf '…'`, but never at position zero
/// (see the `no_command_begins_with_the_marker` test). A shell prefixes the line too:
/// bash switches bracketed paste off (`\e[?2004l`) the instant it executes
/// one, so the **first** line of every command's output arrives glued to
/// that escape. `go` is the launch's first and only word before `rz` takes
/// the port, so without this the run never leaves `Cmd`, dies on the step
/// timeout, and `rz` waits forever for bytes that are never sent.
fn strip_console_noise(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.next() {
            // CSI: parameter and intermediate bytes, then one final byte.
            Some('[') => {
                for c in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c) {
                        break;
                    }
                }
            }
            // OSC: runs until BEL or a string terminator (`ESC \`).
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\u{7}' {
                        break;
                    }
                    if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            // ESC intermediates (a byte in 0x20..=0x2F) are followed by the
            // final byte; anything else was the two-byte form already.
            Some(c) if ('\u{20}'..='\u{2f}').contains(&c) => {
                chars.next();
            }
            _ => {}
        }
    }
    out.trim_start_matches(|c: char| c.is_control() || c.is_whitespace())
        .to_string()
}

/// Why a step was typed; decides what its captured output is parsed as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Probe,
    Launch,
    PagerPrepare,
    PagerChunk {
        index: i64,
        offset: i64,
        bytes: i64,
        fresh: bool,
    },
    PagerVerify,
    PagerComplete,
    PagerRead,
}

/// A command waiting to be typed.
struct Queued {
    step: Step,
    body: String,
}

/// What a finished step means for the chain around it.
enum Flow {
    /// The probe answered; the form is now armed.
    Ready,
    /// Take the next queued command.
    Continue,
    /// The whole run is over — `outcome` already says how it ended.
    Finished,
}

/// One half of a ZMODEM run: a child process whose stdout feeds the link and
/// whose stdin is fed from it, with a bounded queue in front of each side so a
/// slow link throttles the child instead of the frame loop.
pub struct HostProc {
    child: Child,
    out_rx: Receiver<Vec<u8>>,
    out_staged: Vec<u8>,
    out_at: usize,
    in_tx: SyncSender<Vec<u8>>,
    in_pending: VecDeque<Vec<u8>>,
    err_rx: Receiver<Vec<u8>>,
    /// Bytes handed to the link (a send) or taken from it (a receive).
    pub moved: u64,
    /// The child's own diagnostic stream, for the view's status line.
    pub status: String,
}

impl HostProc {
    /// `sz -- local_file` for an upload, `rz` in the destination's directory
    /// for a download. `-e` escapes control characters, which is what keeps a
    /// console's `onlcr`/`icrnl` translation from rewriting the stream;
    /// `-O` disables lrzsz's own timeouts, because a link we pace deliberately
    /// is a slow link, not a dead one.
    fn spawn(direction: Direction, path: &Path) -> Result<Self, String> {
        let mut command = match direction {
            Direction::Send => {
                let mut c = Command::new("sz");
                c.arg("-e").arg("-O").arg("-q").arg("--").arg(path);
                c
            }
            Direction::Recv => {
                let mut c = Command::new("rz");
                c.arg("-e").arg("-O").arg("-y").arg("-q");
                if let Some(dir) = path.parent() {
                    if !dir.as_os_str().is_empty() {
                        c.current_dir(dir);
                    }
                }
                c
            }
        };
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|err| format!("Could not start lrzsz on this host: {err}"))?;
        let stdout = child.stdout.take().expect("piped");
        let stderr = child.stderr.take().expect("piped");
        let stdin = child.stdin.take().expect("piped");

        // The channels are bounded, so a child that produces faster than the
        // link drains blocks on its own pipe — the backpressure reaches
        // lrzsz, where it belongs, and never this frame loop.
        let (out_tx, out_rx) = sync_channel::<Vec<u8>>(8);
        std::thread::spawn(move || pump_read(stdout, out_tx, 8192));
        let (err_tx, err_rx) = sync_channel::<Vec<u8>>(8);
        std::thread::spawn(move || pump_read(stderr, err_tx, 1024));
        let (in_tx, in_rx) = sync_channel::<Vec<u8>>(32);
        std::thread::spawn(move || feed_write(stdin, in_rx));

        Ok(HostProc {
            child,
            out_rx,
            out_staged: Vec::new(),
            out_at: 0,
            in_tx,
            in_pending: VecDeque::new(),
            err_rx,
            moved: 0,
            status: String::new(),
        })
    }

    /// The next slice for the link, at most [`CHUNK_BYTES`]. Timing belongs to
    /// [`Transfer::pump`], which paces the typed commands by the same clock.
    fn pump(&mut self) -> Option<Vec<u8>> {
        if self.out_at >= self.out_staged.len() {
            match self.out_rx.try_recv() {
                Ok(message) => {
                    self.out_staged = message;
                    self.out_at = 0;
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return None,
            }
            if self.out_staged.is_empty() {
                return None;
            }
        }
        let end = (self.out_at + CHUNK_BYTES).min(self.out_staged.len());
        let slice = self.out_staged[self.out_at..end].to_vec();
        self.out_at = end;
        if self.out_at >= self.out_staged.len() {
            self.out_staged.clear();
            self.out_at = 0;
        }
        self.moved += slice.len() as u64;
        Some(slice)
    }

    /// Queue link bytes for the child's stdin without ever blocking: the frame
    /// loop hands over what it has and comes back later.
    fn feed(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        // The field's contract is "bytes handed to the link (a send) or taken
        // from it (a receive)", but only the sending half ever counted. A
        // download therefore sat at `0 B` for its whole run — and, worse,
        // left [`Transfer`] nothing to tell a transfer that is still moving
        // from one whose peer has gone quiet.
        self.moved += bytes.len() as u64;
        self.in_pending.push_back(bytes.to_vec());
        self.flush();
    }

    fn flush(&mut self) {
        while let Some(next) = self.in_pending.pop_front() {
            match self.in_tx.try_send(next) {
                Ok(()) => {}
                Err(TrySendError::Full(value)) => {
                    self.in_pending.push_front(value);
                    break;
                }
                Err(TrySendError::Disconnected(_)) => {
                    self.in_pending.clear();
                    break;
                }
            }
        }
    }

    /// `Some(code)` once the child has been reaped; `None` while it runs.
    fn exit_code(&mut self) -> Option<i32> {
        match self.child.try_wait() {
            Ok(Some(status)) => Some(status.code().unwrap_or(-1)),
            Ok(None) => None,
            Err(_) => Some(-1),
        }
    }

    fn take_status(&mut self) {
        while let Ok(chunk) = self.err_rx.try_recv() {
            self.status.push_str(&String::from_utf8_lossy(&chunk));
            if self.status.len() > 512 {
                self.status.drain(..self.status.len() - 512);
            }
        }
    }

    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn pump_read<T: Read + Send + 'static>(source: T, tx: SyncSender<Vec<u8>>, size: usize) {
    let mut source = source;
    let mut buf = vec![0u8; size];
    loop {
        match source.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        }
    }
}

fn feed_write(mut sink: std::process::ChildStdin, rx: Receiver<Vec<u8>>) {
    while let Ok(chunk) = rx.recv() {
        if sink.write_all(&chunk).is_err() {
            break;
        }
        let _ = sink.flush();
    }
}

/// Where a run is. The `Cmd` variant carries the step in flight so a late
/// answer can be recognised as late.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    /// Nothing running; the view is a form.
    Idle,
    /// The probe answered; the Start action is live.
    Ready,
    /// A typed command is waiting for its `rc`.
    Cmd { step: Step, seq: u32, at: Instant },
    /// The host's process is running and the link is its.
    Run { at: Instant },
    /// The host finished; the target's own `rc` may still be on its way.
    Settle { until: Instant },
    /// Terminal state; [`Transfer::outcome`] says whether it was a success.
    Done,
}

/// What the view shows under the progress line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Idle,
    Busy(&'static str),
    Failed(String),
    Ok(String),
}

/// State of a pager download: the file grows page by page as they arrive.
struct Download {
    file: File,
    offset: u64,
    total: Option<u64>,
}

/// A transfer. Driven entirely from the frame loop: [`Transfer::poll`] for
/// time, [`Transfer::pump`] for bytes out, [`Transfer::on_rx`] for bytes in.
pub struct Transfer {
    pub direction: Direction,
    pub channel: Channel,
    /// Host file: what is sent, or where the receive lands.
    pub local: PathBuf,
    /// Target file, absolute after [`Transfer::expand`].
    pub target: String,
    pub probe: Option<Probe>,
    pub host: HostTools,
    pub phase: Phase,
    pub outcome: Outcome,
    /// Bytes moved, for the progress line.
    pub moved: u64,
    /// When a byte last crossed the link in either direction. `None` outside
    /// a run, where there is nothing to be wedged.
    ///
    /// It is what [`STALL_TIMEOUT`] is measured against, and what makes a
    /// stalled transfer reportable *while* it is still on screen rather than
    /// only at the end of a hard ceiling.
    pub last_progress: Option<Instant>,
    /// Total, when the direction makes it known.
    pub total: Option<u64>,
    /// The host child's diagnostic stream, for the status line.
    pub status: String,

    /// Bytes still to type, paced by [`CHUNK_INTERVAL`].
    queue: VecDeque<u8>,
    next_at: Option<Instant>,
    /// Commands waiting for the one in flight to finish.
    steps: VecDeque<Queued>,
    /// Sequence number of the step in flight.
    seq: u32,
    /// Output of the command in flight, for `target_files`' parsers.
    cap: Vec<u8>,
    /// Rolling line buffer: marker detection, bounded by line consumption.
    scan: Vec<u8>,
    /// The ZMODEM child, while one is running.
    proc: Option<HostProc>,
    /// Bytes this engine wants on the wire right now, ahead of everything the
    /// pacing queue holds: the cancel that tells a peer already inside a
    /// transfer to let go of its console. `poll` hands it out, the transfer
    /// view sends it.
    ///
    /// [`fail`] is where it is set, because failing a run is the one thing
    /// every dead end shares — a stall, a launch that never answered, a link
    /// that went, the person pressing abort. Before this, the cancel only went
    /// out on the keyboard path, and a transfer that died on its own left the
    /// target sitting in `rz` for good, holding the console every later
    /// command would have to get past.
    pending_out: Vec<u8>,
    /// The `rc` the target reported for the launch command, once it does.
    launch_rc: Option<i32>,
    /// State of a pager download in progress.
    download: Option<Download>,
    /// Digest the pager's `complete` step must reproduce.
    expected_sha256: String,
    /// The line terminator this run's commands end with, taken from the
    /// terminal's Enter mode so a transfer types exactly what a person would.
    enter: Vec<u8>,
}

impl Default for Transfer {
    fn default() -> Self {
        Transfer {
            direction: Direction::Send,
            channel: Channel::Zmodem,
            local: PathBuf::new(),
            target: String::new(),
            probe: None,
            host: HostTools::default(),
            phase: Phase::Idle,
            outcome: Outcome::Idle,
            moved: 0,
            last_progress: None,
            total: None,
            status: String::new(),
            queue: VecDeque::new(),
            next_at: None,
            steps: VecDeque::new(),
            seq: 0,
            cap: Vec::new(),
            scan: Vec::new(),
            proc: None,
            pending_out: Vec::new(),
            launch_rc: None,
            download: None,
            expected_sha256: String::new(),
            enter: b"\r".to_vec(),
        }
    }
}

impl Transfer {
    /// The Enter mode's rendering of a newline, so typed commands end the way
    /// the terminal pane's own input does.
    pub fn set_enter(&mut self, enter: Vec<u8>) {
        self.enter = if enter.is_empty() {
            b"\r".to_vec()
        } else {
            enter
        };
    }

    /// Whether inbound bytes belong to this transfer rather than to the grid.
    /// The grid is deliberately starved while capture is on: a ZDATA frame is
    /// not text, and painting it would be worse than missing it.
    pub fn capturing(&self) -> bool {
        matches!(
            self.phase,
            Phase::Cmd { .. } | Phase::Run { .. } | Phase::Settle { .. }
        )
    }

    pub fn busy(&self) -> bool {
        !matches!(self.phase, Phase::Idle | Phase::Ready | Phase::Done)
    }

    /// Ask the target what it has. Nothing else may start before this
    /// answered — the gate: no `lrzsz`, no transfer.
    pub fn probe_now(&mut self) -> Result<(), String> {
        if self.busy() {
            return Err("A transfer is already running.".to_string());
        }
        self.reset();
        self.probe = None;
        self.steps.push_back(Queued {
            step: Step::Probe,
            body: probe_command(),
        });
        self.next_step();
        Ok(())
    }

    /// Start the run the form describes: validate, pick a channel, spawn the
    /// host half, type the target's half.
    pub fn start(&mut self) -> Result<(), String> {
        if !matches!(self.phase, Phase::Ready | Phase::Done) {
            return Err("Run the probe before starting a transfer.".to_string());
        }
        let Some(probe) = self.probe.clone() else {
            return Err("Run the probe first.".to_string());
        };
        self.validate(&probe)?;
        self.reset();
        // A cancel owed to a peer that is no longer there is not owed to the
        // one this run is about to talk to: it would land in the middle of the
        // launch command and read as protocol noise.
        self.pending_out.clear();
        self.total = match self.direction {
            Direction::Send => std::fs::metadata(&self.local).ok().map(|m| m.len()),
            Direction::Recv => None,
        };
        self.channel = if probe.target_zmodem() && self.host.zmodem() {
            Channel::Zmodem
        } else if probe.pager() {
            Channel::Pager
        } else {
            return Err(
                "The target has neither lrzsz (sz/rz) nor the pager tools (dd/base64/wc/tr)."
                    .to_string(),
            );
        };
        self.outcome = Outcome::Busy(match self.direction {
            Direction::Send => "Sending…",
            Direction::Recv => "Receiving…",
        });

        match self.channel {
            Channel::Zmodem => self.start_zmodem(&probe)?,
            Channel::Pager => self.start_pager(&probe)?,
        }
        Ok(())
    }

    /// Refuse anything that would lose data or type into the wrong place.
    fn validate(&self, probe: &Probe) -> Result<(), String> {
        let target = self.expand(&self.target, &probe.home);
        if target.is_empty() {
            return Err("The target path is empty.".to_string());
        }
        if !target.starts_with('/') {
            return Err("The target path must be absolute (it starts with /).".to_string());
        }
        if target.chars().any(|c| c.is_control()) {
            return Err("The target path must not contain control characters.".to_string());
        }
        match self.direction {
            Direction::Send => {
                if !self.local.is_file() {
                    return Err(format!("No such local file: {}", self.local.display()));
                }
                if self.total_of(&self.local) == Some(0) {
                    return Err(format!(
                        "{} is empty; there is nothing to send.",
                        self.local.display()
                    ));
                }
            }
            Direction::Recv => {
                if self.local.exists() {
                    return Err(format!(
                        "The local file already exists: {}",
                        self.local.display()
                    ));
                }
                let parent = self.local.parent().filter(|p| !p.as_os_str().is_empty());
                let parent = parent.unwrap_or_else(|| Path::new("."));
                if !parent.is_dir() {
                    return Err(format!("No such local directory: {}", parent.display()));
                }
                // The host's `rz` writes the *sender's* file name next to the
                // destination and this module renames it afterwards; that
                // rename must not have anything to clobber.
                if let Some(name) = Path::new(&target).file_name() {
                    let sibling = parent.join(name);
                    if sibling != self.local && sibling.exists() {
                        return Err(format!(
                            "Receiving writes {} on the way; it already exists.",
                            sibling.display()
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    fn total_of(&self, path: &Path) -> Option<u64> {
        std::fs::metadata(path).ok().map(|m| m.len())
    }

    /// `~/x` expands with the *target's* home from the probe, never with this
    /// machine's — they are different computers.
    fn expand(&self, path: &str, home: &str) -> String {
        if path == "~" {
            return home.to_string();
        }
        if let Some(rest) = path.strip_prefix("~/") {
            if !home.is_empty() {
                return format!("{home}/{rest}");
            }
        }
        path.to_string()
    }

    fn start_zmodem(&mut self, probe: &Probe) -> Result<(), String> {
        let target = self.expand(&self.target, &probe.home);
        let body = match self.direction {
            Direction::Send => {
                let dir = Path::new(&target)
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .ok_or_else(|| format!("Cannot tell the target directory out of {target}."))?
                    .display()
                    .to_string();
                let name = self
                    .local
                    .file_name()
                    .and_then(|n| n.to_str())
                    .ok_or_else(|| "The local file has no usable name.".to_string())?;
                let written = Path::new(&dir).join(name);
                launch_recv(&target, &dir, &written.display().to_string())?
            }
            Direction::Recv => launch_send(&target)?,
        };

        // The host half comes up *before* the target's line is typed: `rz`
        // sends ZRQINIT the instant it starts, and a byte that arrived before
        // there was anywhere to put it would cost a whole handshake round.
        let proc = HostProc::spawn(self.direction, &self.local)?;
        self.proc = Some(proc);
        self.launch_rc = None;
        self.steps.push_back(Queued {
            step: Step::Launch,
            body,
        });
        self.next_step();
        Ok(())
    }

    fn start_pager(&mut self, probe: &Probe) -> Result<(), String> {
        let target = self.expand(&self.target, &probe.home);
        match self.direction {
            Direction::Send => {
                let size = std::fs::metadata(&self.local)
                    .map_err(|err| format!("Cannot read {}: {err}", self.local.display()))?
                    .len();
                if size > PAGER_LIMIT {
                    return Err(format!(
                        "That file is {size} bytes; the pager tops out at {PAGER_LIMIT} bytes. Use lrzsz (ZMODEM) on the target."
                    ));
                }
                // Only a digest that was really computed may be sent: an
                // unreadable file must fail here, not travel as an empty
                // hash that quietly turns the target's verification off.
                // (`expected_sha256` itself comes from the plan below.)
                let digest = if probe.digest() {
                    local_sha256(&self.local).ok_or_else(|| {
                        format!(
                            "Cannot read {}: its digest could not be computed.",
                            self.local.display()
                        )
                    })?
                } else {
                    String::new()
                };
                let mut request = UploadRequest::new(&target, size as i64);
                request.sha256 = &digest;
                let plan = upload_plan(request).map_err(|err| err.to_string())?;
                if let Some(prepare) = plan.prepare_command {
                    self.steps.push_back(Queued {
                        step: Step::PagerPrepare,
                        body: prepare,
                    });
                }
                // The body of a chunk is filled in when its turn comes, so the
                // file is read 720 bytes at a time instead of all at once —
                // a 14 MB upload must not cost 14 MB of RAM.
                for chunk in &plan.chunks {
                    self.steps.push_back(Queued {
                        step: Step::PagerChunk {
                            index: chunk.index as i64,
                            offset: chunk.offset as i64,
                            bytes: chunk.bytes as i64,
                            fresh: chunk.offset == 0,
                        },
                        body: String::new(),
                    });
                }
                self.steps.push_back(Queued {
                    step: Step::PagerVerify,
                    body: plan.verify_command,
                });
                self.steps.push_back(Queued {
                    step: Step::PagerComplete,
                    body: plan.complete_command,
                });
                self.expected_sha256 = plan.expected_sha256;
                self.next_step();
            }
            Direction::Recv => {
                let file = File::create(&self.local)
                    .map_err(|err| format!("Cannot create {}: {err}", self.local.display()))?;
                self.download = Some(Download {
                    file,
                    offset: 0,
                    total: None,
                });
                let body =
                    read_file_command(&target, 0, MAX_READ_BYTES).map_err(|err| err.to_string())?;
                self.steps.push_back(Queued {
                    step: Step::PagerRead,
                    body,
                });
                self.next_step();
            }
        }
        Ok(())
    }

    /// Type the next queued command, if nothing is in flight.
    fn next_step(&mut self) {
        if matches!(self.phase, Phase::Cmd { .. } | Phase::Run { .. }) {
            return;
        }
        let Some(queued) = self.steps.pop_front() else {
            return;
        };
        let body = match queued.step {
            Step::PagerChunk {
                index,
                offset,
                bytes,
                fresh,
            } => match self.chunk_body(index, offset, bytes, fresh) {
                Ok(body) => body,
                Err(err) => {
                    self.fail(err);
                    return;
                }
            },
            _ => queued.body,
        };
        self.seq += 1;
        let mut bytes = wrap(self.seq, &body).into_bytes();
        bytes.extend_from_slice(&self.enter);
        self.queue.extend(bytes);
        self.cap.clear();
        self.scan.clear();
        self.phase = Phase::Cmd {
            step: queued.step,
            seq: self.seq,
            at: Instant::now(),
        };
    }

    /// Build one pager chunk command: read the slice now, base64 it, hand it
    /// to `target_files`' builder unchanged.
    fn chunk_body(
        &self,
        index: i64,
        offset: i64,
        bytes: i64,
        fresh: bool,
    ) -> Result<String, String> {
        let temp = format!("{}{PART_SUFFIX}", self.target);
        let mut file = File::open(&self.local)
            .map_err(|err| format!("open {}: {err}", self.local.display()))?;
        file.seek(SeekFrom::Start(offset as u64))
            .map_err(|err| format!("seek: {err}"))?;
        let mut buf = vec![0u8; bytes as usize];
        file.read_exact(&mut buf)
            .map_err(|err| format!("read: {err}"))?;
        upload_chunk_command(&temp, index, offset, bytes, &encode_base64(&buf), fresh)
            .map_err(|err| err.to_string())
    }

    /// Bytes [`fail`] wants on the wire *now*, ahead of the pacing queue —
    /// the cancel that frees a target left inside a transfer. Taken once and
    /// cleared, so a frame loop that runs every tick cannot send it twice.
    ///
    /// The caller puts the two break characters behind it: the cancel is what
    /// reaches a peer already inside a transfer, where the console is raw and
    /// `^C` is data; the break behind it clears the line once that peer is
    /// back at a shell.
    pub fn take_pending_out(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.pending_out)
    }

    /// Bytes for the link this tick: what we are typing first, then — while the
    /// child still has a claim on the line, which outlives the phase change —
    /// the child's own stream. One chunk per interval, whatever either of them
    /// produced.
    ///
    /// [`Phase::Settle`] counts, and the difference is the protocol itself.
    /// `sz` writes `OO` and exits in the same breath; the frame loop reads
    /// *time* before *bytes*, so by the time `pump` runs, `poll` has already
    /// reaped the exit and the phase is no longer `Run`. Gating the child on
    /// `Run` alone therefore left `OO` sitting in the pipe for good — the
    /// receiver then waits for a frame that never arrives. Measured on the
    /// Bee: `rz` held the console **56.0 s** for an `OO` the host had already
    /// dropped, the run ended on the [`SETTLE`] fallback, and the next
    /// transfer typed straight into a shell `rz` still owned. Locally the same
    /// code passes, because `sz`'s exit is not reaped within the ~0.4 ms that
    /// separates `08` from `OO`.
    pub fn pump(&mut self, now: Instant) -> Option<Vec<u8>> {
        if let Some(next) = self.next_at {
            if now < next {
                return None;
            }
        }
        let slice = if !self.queue.is_empty() {
            let take = CHUNK_BYTES.min(self.queue.len());
            let mut out = Vec::with_capacity(take);
            for _ in 0..take {
                out.push(self.queue.pop_front().expect("bounded by take"));
            }
            Some(out)
        } else if matches!(self.phase, Phase::Run { .. } | Phase::Settle { .. }) {
            self.proc.as_mut()?.pump()
        } else {
            None
        }?;
        self.next_at = Some(now + CHUNK_INTERVAL);
        Some(slice)
    }

    /// Feed one inbound frame. Bytes go to the child while it runs; the marker
    /// scanner runs in every capturing phase, because the line that ends a run
    /// is the target's `rc`.
    pub fn on_rx(&mut self, bytes: &[u8]) {
        if !self.capturing() {
            return;
        }
        if matches!(self.phase, Phase::Run { .. }) {
            if let Some(proc) = self.proc.as_mut() {
                proc.feed(bytes);
                proc.flush();
            }
        } else {
            self.cap.extend_from_slice(bytes);
            if self.cap.len() > CAP_LIMIT {
                let keep = CAP_LIMIT / 2;
                self.cap.drain(..self.cap.len() - keep);
            }
        }
        self.scan.extend_from_slice(bytes);
        while let Some(line) = self.take_scan_line() {
            self.on_marker(&line);
        }
    }

    /// Pop the next complete line out of the rolling buffer, dropping the `\r`
    /// a console adds. An incomplete line is kept, never guessed at.
    ///
    /// A line is decoded lossily, not rejected. Once `rz`/`sz` has had the
    /// port the line in front of the report is a ZMODEM frame, and it ends
    /// `\x8a` — not UTF-8. Rejecting it would return `None`, end [`on_rx`]'s
    /// scan loop, and strand every line still buffered behind it; on the Bee
    /// the `rc` that follows a frame is the last `\n` the shell prints before
    /// a newline-less prompt, so it would never be read at all. The marker it
    /// carries is ASCII and survives.
    fn take_scan_line(&mut self) -> Option<String> {
        let end = self.scan.iter().position(|b| *b == b'\n')?;
        let mut line = self.scan.drain(..=end).collect::<Vec<u8>>();
        line.pop();
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        Some(String::from_utf8_lossy(&line).into_owned())
    }

    /// Read one line the target printed as an answer, if it is one.
    fn on_marker(&mut self, line: &str) {
        let cleaned = strip_console_noise(line);
        let Some(rest) = cleaned.strip_prefix(MARKER) else {
            return;
        };
        // The verb and the whole tail after it — never the tail cut into
        // words: `exists` and `missing` carry a path the target quoted when
        // it printed the marker, and a path may hold spaces. `splitn(3, ' ')`
        // would stop at the first one and report a name the target never
        // checked.
        let (verb, tail) = rest.split_once(' ').unwrap_or((rest, ""));
        match verb {
            "go" => {
                if matches!(
                    self.phase,
                    Phase::Cmd {
                        step: Step::Launch,
                        ..
                    }
                ) {
                    // The command has been typed in full — nothing of it is
                    // left to send, and only the child owns the link now.
                    self.queue.clear();
                    // The stall budget starts here, not at the first byte: a
                    // run that never sees one must still be able to report
                    // itself wedged rather than wait out the ceiling.
                    self.last_progress = Some(Instant::now());
                    self.phase = Phase::Run {
                        at: Instant::now() + RUN_TIMEOUT,
                    };
                }
            }
            "exists" => {
                self.fail(format!(
                    "The target already has {tail}; rename or remove it."
                ));
            }
            "missing" => {
                self.fail(format!("The target has no file at {tail}."));
            }
            "rc" => {
                let mut args = tail.split(' ');
                let (Some(seq), Some(code)) = (
                    args.next().and_then(|s| s.parse::<u32>().ok()),
                    args.next().and_then(|s| s.parse::<i32>().ok()),
                ) else {
                    return;
                };
                if seq != self.seq {
                    return;
                }
                self.on_rc(code);
            }
            _ => {}
        }
    }

    fn on_rc(&mut self, code: i32) {
        let Phase::Cmd { step, .. } = self.phase else {
            // The launch command's own `rc` arrives after the run is over.
            if matches!(self.phase, Phase::Run { .. } | Phase::Settle { .. }) {
                self.launch_rc = Some(code);
                self.maybe_finish();
            }
            return;
        };
        // `read_file_command` reports its refusals through markers and exits
        // 0 on purpose, so a non-zero status here is always real news — and
        // the captured output is what makes it diagnosable.
        if code != 0 {
            let said = self.cap_text();
            let said = said.trim();
            self.fail(if said.is_empty() {
                format!("The target command exited {code}.")
            } else {
                format!("The target command exited {code}. {said}")
            });
            return;
        }
        let _ = step;
        match self.finish_step(step) {
            Flow::Ready => {}
            Flow::Continue => {
                self.phase = Phase::Idle;
                self.next_step();
            }
            Flow::Finished => {}
        }
    }

    /// Parse the captured output of a finished step.
    fn finish_step(&mut self, step: Step) -> Flow {
        let text = self.cap_text();
        match step {
            Step::Probe => {
                let probe = parse_probe(&text);
                self.probe = Some(probe);
                self.steps.clear();
                self.phase = Phase::Ready;
                self.outcome = Outcome::Idle;
                Flow::Ready
            }
            Step::Launch => {
                // Unreachable in practice: `go` moves the phase to `Run`
                // before the command can report its status. Reaching here
                // means the target never started lrzsz.
                self.fail("The target never started its half of the transfer.".to_string());
                Flow::Finished
            }
            Step::PagerPrepare => Flow::Continue,
            Step::PagerChunk { .. } => {
                let progress = parse_upload_progress(&text);
                if progress.status != UploadStatus::Ok {
                    self.fail(progress.reason);
                    return Flow::Finished;
                }
                self.moved = progress.next_offset.unwrap_or(self.moved);
                Flow::Continue
            }
            Step::PagerVerify => {
                let expected = self.total.map(|t| t as i64);
                match parse_upload_result(&text, expected, &self.expected_sha256) {
                    Ok(result) if result.status == UploadStatus::Ok => {
                        self.moved = result.bytes;
                        Flow::Continue
                    }
                    Ok(result) => {
                        self.fail(result.reason);
                        Flow::Finished
                    }
                    Err(err) => {
                        self.fail(err.to_string());
                        Flow::Finished
                    }
                }
            }
            Step::PagerComplete => {
                let expected = self.total.map(|t| t as i64);
                match parse_upload_result(&text, expected, &self.expected_sha256) {
                    Ok(result) if result.status == UploadStatus::Ok && result.moved => {
                        self.moved = result.bytes;
                        self.finish_ok();
                        Flow::Finished
                    }
                    Ok(result) => {
                        self.fail(result.reason);
                        Flow::Finished
                    }
                    Err(err) => {
                        self.fail(err.to_string());
                        Flow::Finished
                    }
                }
            }
            Step::PagerRead => {
                let page = parse_file_read(&text);
                if page.status != ReadStatus::Ok {
                    self.fail(match page.status {
                        ReadStatus::Missing => "The target file does not exist.".to_string(),
                        ReadStatus::Directory => "The target path is a directory.".to_string(),
                        ReadStatus::Denied => "The target file is not readable.".to_string(),
                        ReadStatus::Incomplete => {
                            if page.reason.is_empty() {
                                "The target's answer was incomplete.".to_string()
                            } else {
                                page.reason
                            }
                        }
                        ReadStatus::Ok => String::new(),
                    });
                    return Flow::Finished;
                }
                let data = page.data.unwrap_or_default();
                let n = data.len() as u64;
                // The write is done inside its own borrow so a failure can
                // still call `fail`, which needs the whole struct.
                let problem = match self.download.as_mut() {
                    Some(download) => {
                        let problem = download
                            .file
                            .write_all(&data)
                            .err()
                            .map(|err| err.to_string());
                        download.offset += n;
                        if download.total.is_none() {
                            download.total = page.total_bytes;
                        }
                        problem
                    }
                    None => Some("The download lost its state.".to_string()),
                };
                if let Some(problem) = problem {
                    self.fail(format!(
                        "Could not write {}: {problem}",
                        self.local.display()
                    ));
                    return Flow::Finished;
                }
                let (offset, total) = match self.download.as_ref() {
                    Some(download) => (download.offset, download.total),
                    None => {
                        self.fail("The download lost its state.".to_string());
                        return Flow::Finished;
                    }
                };
                self.moved = offset;
                self.total = total;
                let done = total.map(|t| offset >= t).unwrap_or(n == 0) || n == 0;
                if done {
                    self.download = None;
                    self.finish_ok();
                    return Flow::Finished;
                }
                match read_file_command(&self.target, offset as i64, MAX_READ_BYTES) {
                    Ok(body) => self.steps.push_back(Queued {
                        step: Step::PagerRead,
                        body,
                    }),
                    Err(err) => {
                        self.fail(err.to_string());
                        return Flow::Finished;
                    }
                }
                Flow::Continue
            }
        }
    }

    fn cap_text(&self) -> String {
        String::from_utf8_lossy(&self.cap).into_owned()
    }

    /// Frame-loop tick: timeouts, the settle window, and the moment the
    /// target's own `rc` decides whether the run succeeded.
    pub fn poll(&mut self, now: Instant) {
        match self.phase.clone() {
            Phase::Cmd { at, .. } => {
                if now.duration_since(at) > STEP_TIMEOUT {
                    self.fail("The target did not answer.".to_string());
                }
            }
            Phase::Run { at } => {
                // Read the child's state in one place so the borrow ends
                // before anything needs `&mut self` again.
                let state = match self.proc.as_mut() {
                    Some(proc) => {
                        proc.take_status();
                        Some((proc.exit_code(), proc.status.clone(), proc.moved))
                    }
                    None => None,
                };
                let Some((code, status, moved)) = state else {
                    self.fail("The host process is gone.".to_string());
                    return;
                };
                self.status = status;
                // A byte that crossed the link is the only proof the peer is
                // still there, so note the moment before the counter is
                // overwritten — the comparison *is* the check.
                let progressed = moved != self.moved;
                self.moved = moved;
                if progressed {
                    self.last_progress = Some(now);
                }
                match code {
                    Some(0) => {
                        self.phase = Phase::Settle {
                            until: now + SETTLE,
                        };
                        self.maybe_finish();
                    }
                    Some(other) => self.fail(format!(
                        "lrzsz on this host exited {other}. {}",
                        self.status.trim()
                    )),
                    // No byte for long enough and the peer is wedged, not
                    // slow. A ceiling could only have said this once, at the
                    // end, and about the whole run at once: `sz` stops
                    // producing when the target stops acknowledging, and `rz`
                    // stops when nothing arrives, so a stalled transfer has
                    // nothing crossing the link to point at — at any size.
                    None if self
                        .last_progress
                        .is_some_and(|since| now.duration_since(since) >= STALL_TIMEOUT) =>
                    {
                        self.fail(format!(
                            "The transfer stalled: nothing crossed the link for {}s.",
                            STALL_TIMEOUT.as_secs()
                        ));
                    }
                    None if now >= at => {
                        self.stop_host();
                        self.fail("The transfer timed out.".to_string());
                    }
                    None => {}
                }
            }
            Phase::Settle { until } => {
                if let Some(proc) = self.proc.as_mut() {
                    proc.take_status();
                    self.status = proc.status.clone();
                    self.moved = proc.moved;
                }
                self.maybe_finish();
                if matches!(self.phase, Phase::Settle { .. }) && now >= until {
                    // The target never reported, not even within the budget
                    // the launch step gets. The host's clean exit is the best
                    // evidence there is, and saying so beats stalling — but it
                    // is reported as what it is: the target never spoke.
                    self.finish_ok();
                }
            }
            Phase::Idle | Phase::Ready | Phase::Done => {
                if !self.steps.is_empty() {
                    self.next_step();
                }
            }
        }
    }

    /// The run is over when the host exited *and* the target said so — or the
    /// target said so first and the host has since.
    fn maybe_finish(&mut self) {
        if !matches!(self.phase, Phase::Run { .. } | Phase::Settle { .. }) {
            return;
        }
        let Some(rc) = self.launch_rc else {
            return;
        };
        if rc != 0 {
            self.fail(format!("The target's lrzsz exited {rc}."));
            return;
        }
        let host_gone = match self.proc.as_mut() {
            Some(proc) => proc.exit_code().is_some(),
            None => true,
        };
        if !host_gone {
            if matches!(self.phase, Phase::Run { .. }) {
                self.phase = Phase::Settle {
                    until: Instant::now() + SETTLE,
                };
            }
            return;
        }
        self.finish_ok();
    }

    /// Succeed: put the received file where the form asked for it, then say
    /// what actually landed, counted from the file itself rather than from
    /// the framing bytes around it.
    fn finish_ok(&mut self) {
        // Only ZMODEM lands the file under the *sender's* name and needs the
        // rename; the pager writes exactly where the form said.
        if self.direction == Direction::Recv && self.channel == Channel::Zmodem {
            let sibling = match Path::new(&self.target).file_name() {
                Some(name) => Some(self.local.parent().unwrap_or(Path::new(".")).join(name)),
                None => None,
            };
            match sibling {
                Some(sibling) if sibling != self.local => {
                    if !sibling.exists() {
                        self.fail(format!(
                            "The transfer finished but {} was never written.",
                            sibling.display()
                        ));
                        return;
                    }
                    if let Err(err) = std::fs::rename(&sibling, &self.local) {
                        self.fail(format!(
                            "Received {}, which is not the name asked for, and could not rename it to {}: {err}",
                            sibling.display(),
                            self.local.display()
                        ));
                        return;
                    }
                }
                _ => {
                    if !self.local.exists() {
                        self.fail(format!(
                            "The transfer finished but {} was never written.",
                            self.local.display()
                        ));
                        return;
                    }
                }
            }
        }
        let summary = self.summary();
        self.stop_host();
        self.phase = Phase::Done;
        self.outcome = Outcome::Ok(summary);
    }

    /// What landed, measured on the file rather than on the wire: a ZMODEM
    /// frame carries CRCs and headers, so bytes on the link are always more
    /// than bytes in the file.
    fn summary(&self) -> String {
        match self.direction {
            Direction::Send => {
                let size = self.total_of(&self.local).unwrap_or(self.moved);
                format!(
                    "Sent {} to {}.",
                    format_bytes(size as i64).unwrap_or_else(|_| format!("{size} B")),
                    self.target
                )
            }
            Direction::Recv => {
                let size = self.total_of(&self.local).unwrap_or(self.moved);
                format!(
                    "Received {} into {}.",
                    format_bytes(size as i64).unwrap_or_else(|_| format!("{size} B")),
                    self.local.display()
                )
            }
        }
    }

    /// Where a receive has the target's own file name on its way to the one
    /// the form asked for.
    ///
    /// `rz` writes under the *sender's* name, beside the destination, and this
    /// module renames it once the bytes are in ([`Transfer::finish_ok`]). The
    /// form refuses to start a receive while that path is already taken — it
    /// must not clobber a file it did not write — which makes a file found
    /// there *after* a run has begun the run's own, every time. See
    /// [`Transfer::sweep_staged`].
    fn staged_sibling(&self) -> Option<PathBuf> {
        if self.direction != Direction::Recv {
            return None;
        }
        let name = Path::new(&self.target).file_name()?;
        let sibling = self.local.parent().unwrap_or(Path::new(".")).join(name);
        (sibling != self.local).then_some(sibling)
    }

    /// Take back what an unfinished receive left on the disk, so the next one
    /// finds that path free. One aborted download used to block every download
    /// after it: a link drop left a 0-byte `linkr-zm-probe.bin` beside the
    /// destination, and the two runs that followed never started their receive
    /// at all — the form refused them before a byte moved and the target's
    /// console saw nothing.
    fn sweep_staged(&mut self) {
        if let Some(staged) = self.staged_sibling() {
            if staged.exists() {
                let _ = std::fs::remove_file(&staged);
            }
        }
    }

    fn stop_host(&mut self) {
        if let Some(mut proc) = self.proc.take() {
            proc.kill();
        }
    }

    /// The single way a run dies. Every dead end lands here — a stall, a
    /// launch that never answered, a link that went, the person pressing
    /// abort — so this is where the target is told to let go, whatever it
    /// was doing. The phase is read first: everything below wipes it.
    fn fail(&mut self, reason: String) {
        let cancel = self.peer_cancel();
        self.stop_host();
        self.sweep_staged();
        self.queue.clear();
        self.steps.clear();
        self.download = None;
        self.phase = Phase::Done;
        self.outcome = Outcome::Failed(reason);
        self.pending_out = cancel;
    }

    /// Drop everything, keeping the form. Also what a reconnect does.
    pub fn reset(&mut self) {
        self.stop_host();
        self.sweep_staged();
        self.queue.clear();
        self.steps.clear();
        self.cap.clear();
        self.scan.clear();
        self.seq = 0;
        self.next_at = None;
        self.launch_rc = None;
        self.download = None;
        self.moved = 0;
        self.last_progress = None;
        self.status.clear();
        self.outcome = Outcome::Idle;
        self.phase = Phase::Idle;
    }

    /// What the *target* has to hear when a run is abandoned, read while the
    /// run is still on the record: [`Transfer::abort`] replaces the phase this
    /// has to look at.
    ///
    /// [`Transfer::abort`] returns the two break characters, which are right
    /// for a peer still sitting at a shell — `^C` interrupts whatever is
    /// being typed — and wrong for one already inside a transfer, where the
    /// terminal is raw and the break is data (see [`ZMODEM_CANCEL`]). Both go
    /// out together, cancel first: the cancel reaches a receiver mid-transfer,
    /// and the `^C` behind it reaches the shell afterwards and clears the line
    /// the backspaces did not.
    ///
    /// Empty when there is no ZMODEM session to cancel — nothing running, or
    /// a `dd|base64` pager run, where the break alone is the whole answer.
    pub fn peer_cancel(&self) -> Vec<u8> {
        let in_session = matches!(
            self.phase,
            Phase::Cmd { .. } | Phase::Run { .. } | Phase::Settle { .. }
        ) && self.channel == Channel::Zmodem;
        if in_session {
            ZMODEM_CANCEL.to_vec()
        } else {
            Vec::new()
        }
    }

    /// Stop whatever is running: kill the host child and return the two
    /// control characters that break the target's `rz`/`sz` back to a prompt.
    /// Returned separately from [`Transfer::pump`] so they are not paced — a
    /// transfer being stopped must not wait its turn in the queue.
    pub fn abort(&mut self) -> Vec<u8> {
        self.fail("Aborted.".to_string());
        self.outcome = Outcome::Failed("Aborted.".to_string());
        b"\x03\x03".to_vec()
    }

    /// One line for the view.
    pub fn detail(&self) -> String {
        match &self.outcome {
            Outcome::Failed(reason) => reason.clone(),
            Outcome::Ok(text) => text.clone(),
            Outcome::Busy(what) => (*what).to_string(),
            Outcome::Idle => match &self.probe {
                None => "The probe has not run yet.".to_string(),
                Some(probe) => {
                    let mut parts = Vec::new();
                    parts.push(if probe.target_zmodem() {
                        format!("target lrzsz {}", probe.sz_version)
                    } else {
                        "no lrzsz on the target".to_string()
                    });
                    if !self.host.zmodem() {
                        parts.push("no lrzsz on this host".to_string());
                    }
                    if probe.pager() {
                        parts.push(if self.host.zmodem() {
                            "dd|base64 fallback ready".to_string()
                        } else {
                            "dd|base64 pager".to_string()
                        });
                    }
                    if !probe.digest() {
                        parts.push("no digest tool".to_string());
                    }
                    parts.join(" · ")
                }
            },
        }
    }
}

/// Read a probe's captured output back into a [`Probe`].
pub fn parse_probe(text: &str) -> Probe {
    let mut probe = Probe {
        raw: text.to_string(),
        ..Probe::default()
    };
    for line in marker_lines(text) {
        let Some(rest) = line.strip_prefix(MARKER) else {
            continue;
        };
        let mut parts = rest.splitn(3, ' ');
        match parts.next() {
            Some("have") => {
                if let Some(name) = parts.next() {
                    probe.tools.insert(name.to_string(), true);
                }
            }
            Some("no") => {
                if let Some(name) = parts.next() {
                    probe.tools.insert(name.to_string(), false);
                }
            }
            Some("ver") => {
                let name = parts.next().unwrap_or("");
                let version = parts.next().unwrap_or("").trim().to_string();
                match name {
                    "sz" => probe.sz_version = version,
                    "rz" => probe.rz_version = version,
                    _ => {}
                }
            }
            Some("home") => {
                // The whole tail is the path: a home directory may hold
                // spaces, which the word split would cut at the first one.
                probe.home = rest
                    .split_once(' ')
                    .map(|(_, tail)| tail)
                    .unwrap_or("")
                    .trim()
                    .to_string();
            }
            _ => {}
        }
    }
    probe
}

/// `~/x` on *this* machine expands with this machine's home.
///
/// The local side of a transfer runs here, so a path typed into "on this
/// host" has to be resolved here: `self.local.is_file()` against a literal
/// `~/Documents/...` is always false, and the precheck then answers "No such
/// local file" for a file that is right there. The target side is a different
/// computer and goes through [`Transfer::expand`] with the home the probe
/// reported instead — the two never share a home.
///
/// Only a bare `~` and a leading `~/` expand; `~user` is left alone, exactly
/// like the target's. With no home known the path stays as written so the
/// "must be absolute" check turns it into an error rather than a wrong file.
pub fn expand_host_home(path: &str) -> PathBuf {
    let Some(home) = dirs::home_dir() else {
        return PathBuf::from(path);
    };
    let home = home.to_string_lossy().trim_end_matches('/').to_string();
    if home.is_empty() {
        return PathBuf::from(path);
    }
    if path == "~" {
        return PathBuf::from(home);
    }
    match path.strip_prefix("~/") {
        Some(rest) => PathBuf::from(format!("{home}/{rest}")),
        None => PathBuf::from(path),
    }
}

/// sha256 of a local file, for the pager's `complete` step — which refuses to
/// move the part file unless size *and* digest match on the target. The
/// ZMODEM path never calls it: CRC-32 per frame plus `rz`'s length check
/// already cover that link.
pub fn local_sha256(path: &Path) -> Option<String> {
    use sha2::{Digest, Sha256};
    let mut file = File::open(path).ok()?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push(char::from_digit((byte >> 4) as u32, 16).expect("hex"));
        out.push(char::from_digit((byte & 0x0f) as u32, 16).expect("hex"));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A probe that claims every tool, so a test can choose a channel without
    /// depending on what this machine happens to have installed.
    fn probe_all(home: &str) -> Probe {
        Probe {
            tools: PROBE_TOOLS.iter().map(|t| (t.to_string(), true)).collect(),
            sz_version: "sz (lrzsz) 0.12.21rc".to_string(),
            rz_version: "rz (lrzsz) 0.12.21rc".to_string(),
            home: home.to_string(),
            raw: String::new(),
        }
    }

    fn ready(transfer: &mut Transfer, host_zmodem: bool) {
        transfer.host = HostTools {
            sz: host_zmodem,
            rz: host_zmodem,
            sz_version: String::new(),
            rz_version: String::new(),
        };
        transfer.phase = Phase::Ready;
    }

    /// Whether this machine can run a *real* zmodem child.
    ///
    /// The probe is faked everywhere else — a test picks a channel without
    /// asking the machine anything. Only the child is real in the handful of
    /// tests that need one, and a child that cannot be spawned says nothing
    /// about what those tests check. CI installs no `lrzsz`, and a Windows
    /// runner has no such package to install at all, so they stand down there
    /// rather than fail for the machine's sake.
    fn lrzsz_installed() -> bool {
        std::process::Command::new("sz")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok()
    }

    /// The probe is the gate: it must ask about every tool either channel
    /// needs, and it must never begin a line with the marker it waits for.
    #[test]
    fn the_probe_asks_about_both_channels_and_never_answers_itself() {
        let command = probe_command();
        for tool in PROBE_TOOLS {
            assert!(command.contains(tool), "probe does not ask about {tool}");
        }
        assert!(command.contains("$HOME"), "probe does not report the home");
        assert!(
            !command.starts_with(MARKER),
            "the probe's own echo would match its markers"
        );
        assert_eq!(
            command.lines().count(),
            1,
            "the probe must be one typed line"
        );
    }

    /// Every command the module types is one line that cannot be mistaken for
    /// the marker it is waiting on, and each carries its own sequence number.
    #[test]
    fn no_command_begins_with_the_marker() {
        let bodies = [
            probe_command(),
            launch_recv("/tmp/a.bin", "/tmp", "/tmp/a.bin").expect("launch"),
            launch_recv("/tmp/renamed.bin", "/tmp", "/tmp/original.bin").expect("launch"),
            launch_send("/tmp/a.bin").expect("launch"),
            launch_send("/missing/file").expect("launch"),
        ];
        for body in bodies {
            assert!(!body.starts_with(MARKER), "{body}");
            let wrapped = wrap(7, &body);
            assert!(!wrapped.starts_with(MARKER), "{wrapped}");
            assert_eq!(wrapped.lines().count(), 1, "{wrapped}");
            assert!(
                wrapped.contains(&format!("{MARKER}rc 7 ")),
                "the wrapped command must carry its sequence: {wrapped}"
            );
        }
    }

    /// The guard has to name **both** files: `rz -y` writes under the sender's
    /// file name, so a rename puts the destination and the sender's name in the
    /// same directory at risk. Guarding only the destination let `rz` overwrite
    /// an unrelated file and then carry the result away over the top of it.
    #[test]
    fn the_upload_guard_names_both_paths() {
        let command = launch_recv("/tmp/new.bin", "/tmp", "/tmp/original.bin").expect("launch");
        let before_go = command
            .split(&format!("{MARKER}go"))
            .next()
            .expect("the command opens with its guard");
        assert!(
            before_go.contains("[ -e '/tmp/new.bin' ]"),
            "the destination must be refused before anything moves: {command}"
        );
        assert!(
            before_go.contains("[ -e '/tmp/original.bin' ]"),
            "the sender's name must be refused too, before anything moves: {command}"
        );
        assert!(
            !command.contains("mv -f"),
            "the rename must not become the clobber the guard refused: {command}"
        );
        assert!(
            command.contains("mv -n"),
            "the rename must be no-clobber: {command}"
        );
    }

    /// The same thing, run: a stand-in for `rz` that writes the sender's file
    /// name exactly as `lrzsz` does, so the upload must refuse to start and a
    /// bystander file nobody named in the upload survives. No `lrzsz` and no
    /// link needed — this is the review's reproduction, as a test.
    ///
    /// Unix only: it needs `sh` and `mv -n`.
    #[cfg(unix)]
    #[test]
    fn an_upload_refuses_rather_than_destroy_a_bystander() {
        let dir = std::env::temp_dir().join(format!("linkr-recv-guard-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let bystander = dir.join("original.bin");
        let dest = dir.join("new.bin");
        std::fs::write(&bystander, b"precious").expect("seed the bystander");

        let mock = dir.join("mock-bin");
        std::fs::create_dir_all(&mock).expect("mock bin");
        let rz = mock.join("rz");
        std::fs::write(&rz, "#!/bin/sh\nprintf received > original.bin\n").expect("mock rz");
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&rz, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }

        let command = launch_recv(
            dest.to_str().expect("dest is utf8"),
            dir.to_str().expect("dir is utf8"),
            bystander.to_str().expect("written is utf8"),
        )
        .expect("launch");
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg(&command)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    mock.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .status()
            .expect("sh runs");

        assert!(!status.success(), "the upload must be refused: {command}");
        assert!(
            !dest.exists(),
            "nothing may be written when the upload is refused: {command}"
        );
        assert_eq!(
            std::fs::read(&bystander).expect("the bystander must still be there"),
            b"precious",
            "an upload nobody named this file in must not destroy it"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An answer to a command that is not the one in flight never closes the
    /// one that is — the sequence number is what makes the difference.
    #[test]
    fn a_stale_answer_is_ignored() {
        let mut transfer = Transfer::default();
        transfer.probe_now().expect("probe queues");
        assert!(matches!(
            transfer.phase,
            Phase::Cmd {
                step: Step::Probe,
                seq: 1,
                ..
            }
        ));

        // The shell echoing the command it just received, then an answer
        // belonging to some earlier command.
        transfer.on_rx(b"printf 'LINKR_ZM:begin\\n'; for t in sz; do true; done\r\n");
        transfer.on_rx(b"LINKR_ZM:rc 0 0\n");
        assert!(
            matches!(transfer.phase, Phase::Cmd { .. }),
            "a stale rc closed the probe"
        );

        for tool in ["sz", "rz", "dd", "base64", "wc", "tr"] {
            transfer.on_rx(format!("LINKR_ZM:have {tool}\n").as_bytes());
        }
        transfer.on_rx(b"LINKR_ZM:no sha256sum\n");
        transfer.on_rx(b"LINKR_ZM:ver sz sz (lrzsz) 0.12.21rc\n");
        transfer.on_rx(b"LINKR_ZM:home /home/p\n");
        transfer.on_rx(b"LINKR_ZM:rc 1 0\n");
        assert!(
            matches!(transfer.phase, Phase::Ready),
            "{:?}",
            transfer.phase
        );
        let probe = transfer.probe.clone().expect("probe parsed");
        assert!(probe.target_zmodem(), "{probe:?}");
        assert!(probe.pager(), "{probe:?}");
        assert!(!probe.digest(), "sha256sum was reported missing");
        assert_eq!(probe.sz_version, "sz (lrzsz) 0.12.21rc");
        assert_eq!(probe.home, "/home/p");
    }

    /// A marker split across two frames still fires: the scan buffer keeps the
    /// incomplete line rather than guessing at it.
    #[test]
    fn a_marker_split_across_frames_still_fires() {
        let mut transfer = Transfer::default();
        transfer.probe_now().expect("probe queues");
        transfer.on_rx(b"LINKR_ZM:have");
        assert!(matches!(transfer.phase, Phase::Cmd { .. }));
        transfer.on_rx(b" sz\nLINKR_ZM:rc 1 0\n");
        assert!(
            matches!(transfer.phase, Phase::Ready),
            "{:?}",
            transfer.phase
        );
    }

    /// A failed target command is reported with the output it produced, not
    /// with a bare exit code — "127" alone tells nobody what was missing.
    #[test]
    fn a_failed_target_command_reports_what_it_said() {
        let mut transfer = Transfer::default();
        transfer.probe_now().expect("probe queues");
        transfer.on_rx(b"sh: sz: not found\nLINKR_ZM:rc 1 127\n");
        match &transfer.outcome {
            Outcome::Failed(reason) => {
                assert!(reason.contains("127"), "{reason}");
                assert!(reason.contains("not found"), "{reason}");
            }
            other => panic!("expected a failure, got {other:?}"),
        }
        assert!(
            transfer.steps.is_empty(),
            "a failed step must stop the chain"
        );
        assert!(!transfer.busy(), "the view must be free again");
    }

    /// Capture is what keeps a ZDATA frame out of the grid: the view must be
    /// able to ask, at any moment, whether inbound bytes belong to it.
    #[test]
    fn capture_covers_exactly_the_phases_that_own_the_link() {
        let mut transfer = Transfer::default();
        assert!(!transfer.capturing(), "Idle must leave the grid alone");
        transfer.probe_now().expect("probe queues");
        assert!(transfer.capturing(), "a typed command owns the link");
        transfer.phase = Phase::Run {
            at: Instant::now() + RUN_TIMEOUT,
        };
        assert!(transfer.capturing(), "the child owns the link");
        transfer.phase = Phase::Settle {
            until: Instant::now(),
        };
        assert!(transfer.capturing(), "the target's rc may still be coming");
        transfer.phase = Phase::Done;
        assert!(!transfer.capturing(), "the grid gets its console back");
    }

    /// Nothing leaves faster than one chunk per interval, whatever the child
    /// produced: the queue and the child share one clock.
    #[test]
    fn nothing_is_handed_over_faster_than_one_chunk() {
        let mut transfer = Transfer::default();
        transfer.queue.extend(vec![0xAB; CHUNK_BYTES * 4]);
        let now = Instant::now();
        let first = transfer.pump(now).expect("the first slice is due");
        assert_eq!(
            first.len(),
            CHUNK_BYTES,
            "a slice must not exceed the chunk"
        );
        assert!(
            transfer.pump(now).is_none(),
            "the second slice waits its interval"
        );
        let second = transfer
            .pump(now + CHUNK_INTERVAL)
            .expect("the next slice is due");
        assert_eq!(second.len(), CHUNK_BYTES);
        assert!(
            transfer.pump(now + CHUNK_INTERVAL).is_none(),
            "one chunk per interval"
        );
        assert_eq!(
            transfer.queue.len(),
            CHUNK_BYTES * 2,
            "the rest is still waiting, in order"
        );
    }

    /// The form refuses to start before the probe answered, and refuses a
    /// relative target path — the shell would resolve it against a working
    /// directory nobody chose.
    #[test]
    fn the_form_gates_on_the_probe_and_on_absolute_paths() {
        if !lrzsz_installed() {
            return;
        }
        let mut transfer = Transfer {
            local: PathBuf::from("/etc/hostname"),
            target: "relative/path".to_string(),
            ..Transfer::default()
        };
        assert!(transfer.start().is_err(), "must not start without a probe");

        transfer.probe = Some(probe_all("/home/p"));
        ready(&mut transfer, true);
        let err = transfer
            .start()
            .expect_err("a relative target must be refused");
        assert!(err.contains("absolute"), "{err}");

        // A control character would break the single-line protocol the whole
        // module is built on, so it is refused rather than quoted away.
        transfer.target = "/tmp/ok\npath".to_string();
        let err = transfer
            .start()
            .expect_err("a newline in a path must be refused");
        assert!(err.contains("control characters"), "{err}");

        transfer.target = "/tmp/linkr-zm-form.bin".to_string();
        transfer.start().expect("an absolute send is accepted");
        assert_eq!(transfer.channel, Channel::Zmodem, "both halves are present");
        assert!(transfer.busy(), "the launch command is in flight");
        transfer.abort();
    }

    /// When the target has no lrzsz the run falls back to the pager instead
    /// of failing, and says which channel it picked.
    #[test]
    fn a_target_without_lrzsz_falls_back_to_the_pager() {
        let dir = std::env::temp_dir().join(format!("linkr-fallback-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let local = dir.join("payload.bin");
        std::fs::write(&local, b"hello pager").expect("write");

        let mut probe = probe_all("/home/p");
        probe.tools.insert("sz".to_string(), false);
        probe.tools.insert("rz".to_string(), false);

        let mut transfer = Transfer {
            direction: Direction::Send,
            local: local.clone(),
            target: "/remote/payload.bin".to_string(),
            probe: Some(probe),
            ..Transfer::default()
        };
        ready(&mut transfer, true);
        transfer.start().expect("the pager path starts");
        assert_eq!(transfer.channel, Channel::Pager, "no target lrzsz: page it");
        assert!(
            matches!(
                transfer.phase,
                Phase::Cmd {
                    step: Step::PagerPrepare,
                    ..
                }
            ),
            "{:?}",
            transfer.phase
        );

        // Every queued step is a real command with a real sequence number.
        assert!(
            transfer.steps.len() >= 3,
            "prepare, chunks, verify, complete"
        );
        transfer.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A `~` path expands with the home the probe reported, never with this
    /// machine's home — the two are different computers.
    #[test]
    fn a_tilde_path_expands_with_the_targets_home() {
        let transfer = Transfer::default();
        assert_eq!(
            transfer.expand("~/out.bin", "/home/board"),
            "/home/board/out.bin"
        );
        assert_eq!(transfer.expand("~", "/home/board"), "/home/board");
        assert_eq!(transfer.expand("/abs/x", "/home/board"), "/abs/x");
        // With no home known the path stays as written, so the "must be
        // absolute" check turns it into an error instead of a wrong file.
        assert_eq!(transfer.expand("~/x", ""), "~/x");
    }

    /// The *host's* `~` expands with this machine's home, the same way the
    /// target's expands with the probe's. Without it a `~/Documents/x` typed
    /// into the form fails `is_file()` and the precheck answers "No such
    /// local file" for a file that is right there.
    #[test]
    fn a_tilde_on_this_side_expands_with_this_machines_home() {
        assert_eq!(
            expand_host_home("/abs/x"),
            PathBuf::from("/abs/x"),
            "an absolute path is left alone"
        );
        assert_eq!(
            expand_host_home("relative/x"),
            PathBuf::from("relative/x"),
            "so is a relative one — only `~` is special"
        );
        if let Some(home) = dirs::home_dir() {
            let home = home.to_string_lossy().trim_end_matches('/').to_string();
            assert_eq!(
                expand_host_home("~/Documents/x"),
                PathBuf::from(format!("{home}/Documents/x"))
            );
            assert_eq!(expand_host_home("~"), PathBuf::from(home));
            assert_eq!(
                expand_host_home("~user/x"),
                PathBuf::from("~user/x"),
                "`~user` is not ours to resolve"
            );
        }
    }

    /// The probe parser reads exactly what the probe prints, including a tool
    /// that exists on one side only — that difference is the whole point.
    #[test]
    fn the_probe_parser_tells_the_two_channels_apart() {
        let text = concat!(
            "LINKR_ZM:begin\n",
            "LINKR_ZM:have sz\n",
            "LINKR_ZM:have dd\n",
            "LINKR_ZM:have base64\n",
            "LINKR_ZM:have wc\n",
            "LINKR_ZM:have tr\n",
            "LINKR_ZM:no rz\n",
            "LINKR_ZM:no sha256sum\n",
            "LINKR_ZM:no shasum\n",
            "LINKR_ZM:ver sz sz (lrzsz) 0.12.21rc\n",
            "LINKR_ZM:home /root\n",
        );
        let probe = parse_probe(text);
        assert!(!probe.target_zmodem(), "rz is missing");
        assert!(probe.pager(), "every pager tool is there");
        assert!(!probe.digest(), "neither digest tool exists");
        assert_eq!(probe.sz_version, "sz (lrzsz) 0.12.21rc");
        assert_eq!(probe.rz_version, "");
        assert_eq!(probe.home, "/root");
        // A home directory with a space in it is a path, not two words.
        assert_eq!(
            parse_probe("LINKR_ZM:home /home/a b\n").home,
            "/home/a b",
            "the home is the whole tail"
        );
    }

    /// A download writes straight through, so the file it produces *is* the
    /// pages it read: no staging, no rename, no digest round trip.
    #[test]
    fn a_pager_download_is_the_pages_it_read() {
        let dir = std::env::temp_dir().join(format!("linkr-recv-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let dest = dir.join("out.bin");

        let mut transfer = Transfer {
            direction: Direction::Recv,
            local: dest.clone(),
            target: "/remote/data.bin".to_string(),
            probe: Some(probe_all("/home/p")),
            ..Transfer::default()
        };
        ready(&mut transfer, false);
        transfer.start().expect("a pager download starts");
        assert_eq!(transfer.channel, Channel::Pager);
        assert!(matches!(
            transfer.phase,
            Phase::Cmd {
                step: Step::PagerRead,
                ..
            }
        ));

        let payload = encode_base64(b"ping");
        transfer.on_rx(
            format!("LINKR_FILE:begin total=4 from=0\n{payload}\nLINKR_FILE:end bytes=4\n")
                .as_bytes(),
        );
        transfer.on_rx(b"LINKR_ZM:rc 1 0\n");
        assert!(
            matches!(transfer.phase, Phase::Done),
            "{:?}",
            transfer.phase
        );
        let got = std::fs::read(&dest).expect("the file was written");
        assert_eq!(got, b"ping", "the page is the file");
        match &transfer.outcome {
            Outcome::Ok(text) => assert!(text.contains("Received"), "{text}"),
            other => panic!("expected success, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// [`STALL_TIMEOUT`] bounds a wedged run; [`RUN_TIMEOUT`] only bounds an
    /// absurd one.
    ///
    /// One ceiling was doing both jobs, and doing the wrong one: 1800 s
    /// measured from the start of the run cut off every transfer past about
    /// 3.5 MiB on a link that does about 2 KiB/s, while all of its bytes were
    /// still arriving. "No byte has crossed the link for ninety seconds" is
    /// the condition a stalled transfer actually has, at any file size, and
    /// unlike a ceiling it can be found *while* the transfer is on screen.
    #[test]
    fn a_wedged_run_is_found_while_it_still_runs_and_a_moving_one_is_not() {
        if !lrzsz_installed() {
            return;
        }
        let mut transfer = Transfer {
            local: PathBuf::from("/etc/hostname"),
            target: "/tmp/linkr-zm-stall.bin".to_string(),
            probe: Some(probe_all("/tmp")),
            ..Transfer::default()
        };
        ready(&mut transfer, true);
        transfer.start().expect("a zmodem send starts");
        transfer.on_rx(b"LINKR_ZM:go\n");
        assert!(
            matches!(transfer.phase, Phase::Run { .. }),
            "{:?}",
            transfer.phase
        );

        // The link is still moving bytes, so there is nothing to report —
        // however long the run has been going.
        transfer.proc.as_mut().expect("the host half").moved = 4096;
        transfer.moved = 0;
        transfer.poll(Instant::now());
        assert!(
            matches!(transfer.phase, Phase::Run { .. }),
            "a run whose bytes are still arriving is not wedged"
        );

        // The peer goes quiet for the whole stall budget. The finding is made
        // now — while the transfer is still on screen — instead of at the end
        // of a ceiling that could only ever have been a guess at the size.
        transfer.poll(Instant::now() + STALL_TIMEOUT + Duration::from_secs(1));
        assert_eq!(
            transfer.outcome,
            Outcome::Failed(format!(
                "The transfer stalled: nothing crossed the link for {}s.",
                STALL_TIMEOUT.as_secs()
            )),
            "a run that stopped moving bytes must say so on its own terms"
        );

        let _ = transfer.abort();
    }

    /// An existing destination stops an upload before a byte moves: the guard
    /// is the target's own `exists` line, not a guess made on this side.
    #[test]
    fn an_existing_target_file_stops_the_run_before_any_bytes_move() {
        if !lrzsz_installed() {
            return;
        }
        let mut transfer = Transfer {
            local: PathBuf::from("/etc/hostname"),
            target: "/tmp/linkr-zm-exists.bin".to_string(),
            probe: Some(probe_all("/tmp")),
            ..Transfer::default()
        };
        ready(&mut transfer, true);
        transfer.start().expect("a zmodem send starts");
        transfer.on_rx(b"LINKR_ZM:go\n");
        assert!(
            matches!(transfer.phase, Phase::Run { .. }),
            "{:?}",
            transfer.phase
        );
        transfer.on_rx(b"LINKR_ZM:exists /tmp/linkr-zm-exists.bin\n");
        match &transfer.outcome {
            Outcome::Failed(reason) => assert!(reason.contains("already has"), "{reason}"),
            other => panic!("expected a refusal, got {other:?}"),
        }
        // The path the target refused with arrives whole, spaces included:
        // it quoted the path when it printed the marker, so the message has
        // to name the file the user actually has.
        let mut spaced = Transfer {
            local: PathBuf::from("/etc/hostname"),
            target: "/tmp/a b c.bin".to_string(),
            probe: Some(probe_all("/tmp")),
            ..Transfer::default()
        };
        ready(&mut spaced, true);
        spaced.start().expect("a zmodem send starts");
        spaced.on_rx(b"LINKR_ZM:exists /tmp/a b c.bin\n");
        match &spaced.outcome {
            Outcome::Failed(reason) => assert!(
                reason.contains("/tmp/a b c.bin"),
                "the path was cut at its first space: {reason}"
            ),
            other => panic!("expected a refusal, got {other:?}"),
        }
        assert!(transfer.proc.is_none(), "the child must be killed");
        assert!(!transfer.busy(), "the view must be free again");
    }

    /// Aborting kills the child and hands back two control characters that
    /// break the target back to a prompt — unpaced, because a transfer being
    /// stopped must not wait its turn in the queue.
    #[test]
    fn an_abort_kills_the_child_and_returns_an_unpaced_break() {
        if !lrzsz_installed() {
            return;
        }
        let mut transfer = Transfer {
            local: PathBuf::from("/etc/hostname"),
            target: "/tmp/linkr-zm-abort.bin".to_string(),
            probe: Some(probe_all("/tmp")),
            ..Transfer::default()
        };
        ready(&mut transfer, true);
        transfer.start().expect("a zmodem send starts");
        transfer.on_rx(b"LINKR_ZM:go\n");
        assert!(transfer.proc.is_some(), "the host half is running");
        assert_eq!(transfer.abort(), b"\x03\x03");
        assert!(
            transfer.proc.is_none(),
            "the child must not outlive the run"
        );
        assert!(!transfer.busy(), "the view must be free again");
    }

    /// Every way a run dies lands in `fail`, so that is where the target is
    /// told to let go — including the paths nobody pressed a key for: a
    /// stalled link, a launch that never answered, a timeout. Before this the
    /// cancel went out only on the keyboard path, and a transfer that died on
    /// its own left the target sitting in `rz` for good, holding the console
    /// every later command would have to get past.
    #[test]
    fn every_dead_end_leaves_the_cancel_behind_for_the_frame_loop() {
        let mut running = Transfer {
            channel: Channel::Zmodem,
            phase: Phase::Run {
                at: Instant::now() + RUN_TIMEOUT,
            },
            ..Transfer::default()
        };

        running.fail("The transfer stalled.".to_string());
        assert_eq!(
            running.take_pending_out(),
            ZMODEM_CANCEL,
            "a stalled run owes the target a cancel it never sent"
        );
        assert!(
            running.take_pending_out().is_empty(),
            "taken once — the frame loop runs every tick and would resend it"
        );

        // The common death is the link going first, in which case there was
        // nowhere to send it. `reset` is what a reconnect runs, and it keeps
        // the debt: the peer that was left holding its console is still
        // holding it, and the next connection is the first chance to say
        // otherwise.
        running.phase = Phase::Run {
            at: Instant::now() + RUN_TIMEOUT,
        };
        running.fail("The link went away with the run on it.".to_string());
        running.reset();
        assert_eq!(
            running.take_pending_out(),
            ZMODEM_CANCEL,
            "a peer orphaned by a dropped link is owed the cancel on the way back"
        );

        let mut typing = Transfer {
            channel: Channel::Zmodem,
            phase: Phase::Cmd {
                step: Step::Launch,
                seq: 0,
                at: Instant::now(),
            },
            ..Transfer::default()
        };
        typing.fail("The link went.".to_string());
        assert_eq!(typing.take_pending_out(), ZMODEM_CANCEL);

        let mut idle = Transfer::default();
        idle.fail("The target never answered.".to_string());
        assert!(
            idle.take_pending_out().is_empty(),
            "a run that never reached the target's console owes it nothing"
        );
    }

    /// A peer inside a transfer has its console in raw mode and cannot hear
    /// `^C`, so a live ZMODEM run also sends the protocol's own cancel. A run
    /// that is not a ZMODEM session — nothing running, or a `dd|base64` pager
    /// — hears only the break, and the cancel's shape is lrzsz's own.
    #[test]
    fn the_cancel_goes_out_only_when_there_is_a_session_to_cancel() {
        let mut transfer = Transfer {
            channel: Channel::Zmodem,
            ..Transfer::default()
        };

        transfer.phase = Phase::Cmd {
            step: Step::Launch,
            seq: 0,
            at: Instant::now(),
        };
        assert_eq!(
            transfer.peer_cancel(),
            ZMODEM_CANCEL,
            "a launch command being typed is already a session: the target may have started rz"
        );

        transfer.phase = Phase::Run {
            at: Instant::now() + RUN_TIMEOUT,
        };
        assert_eq!(transfer.peer_cancel(), ZMODEM_CANCEL);

        transfer.phase = Phase::Settle {
            until: Instant::now() + SETTLE,
        };
        assert_eq!(transfer.peer_cancel(), ZMODEM_CANCEL);

        transfer.channel = Channel::Pager;
        assert!(
            transfer.peer_cancel().is_empty(),
            "a dd|base64 pager has no ZMODEM to cancel"
        );

        transfer.channel = Channel::Zmodem;
        transfer.phase = Phase::Idle;
        assert!(
            transfer.peer_cancel().is_empty(),
            "nothing is running, so there is nobody to tell"
        );

        // Ten CAN then ten backspaces — the byte string lrzsz sends itself.
        assert_eq!(&ZMODEM_CANCEL[..10], &[0x18; 10], "the cancel proper");
        assert_eq!(&ZMODEM_CANCEL[10..], &[0x08; 10], "its backspaces");
    }

    /// A console switches bracketed paste off (`\e[?2004l`) the instant it
    /// executes a line, so the **first** line of the output arrives glued to
    /// that escape — and `go` is the launch's first and only line before `rz`
    /// takes the port. Matching a bare line start never sees it: the run sits
    /// in `Cmd` until the step timeout while `rz` waits for bytes nobody
    /// sends. The echo, meanwhile, must still not answer.
    #[test]
    fn a_shell_escape_before_the_first_line_does_not_hide_go() {
        if !lrzsz_installed() {
            return;
        }
        let mut transfer = Transfer {
            local: PathBuf::from("/etc/hostname"),
            target: "/tmp/linkr-zm-paste.bin".to_string(),
            probe: Some(probe_all("/tmp")),
            ..Transfer::default()
        };
        ready(&mut transfer, true);
        transfer.start().expect("a zmodem send starts");

        // The echoed launch line, escape included — carries MARKER, but only
        // inside its printf, so it is not an answer.
        transfer.on_rx(
            b"\x1b[?2004hkickpi@k2b:~$ if [ -e '/tmp/x' ]; then printf 'LINKR_ZM:go\\n'; fi\r\n",
        );
        assert!(
            matches!(
                transfer.phase,
                Phase::Cmd {
                    step: Step::Launch,
                    ..
                }
            ),
            "the echo must not answer: {:?}",
            transfer.phase
        );

        // The target's real first line, with the shell's escape in front.
        transfer.on_rx(b"\x1b[?2004l\rLINKR_ZM:go\r\n**\x18B0100000063f694\r");
        assert!(
            matches!(transfer.phase, Phase::Run { .. }),
            "go was hidden: {:?}",
            transfer.phase
        );
        assert_eq!(transfer.abort(), b"\x03\x03");
        assert!(transfer.proc.is_none(), "the child must be killed");
        assert!(!transfer.busy(), "the view must be free again");
    }

    /// `lrzsz` hands back the flow control it was given, so an XON lands on
    /// the very line that carries the next answer.
    #[test]
    fn a_control_character_glued_to_a_marker_does_not_hide_it() {
        if !lrzsz_installed() {
            return;
        }
        let mut transfer = Transfer {
            local: PathBuf::from("/etc/hostname"),
            target: "/tmp/linkr-zm-xon.bin".to_string(),
            probe: Some(probe_all("/tmp")),
            ..Transfer::default()
        };
        ready(&mut transfer, true);
        transfer.start().expect("a zmodem send starts");
        transfer.on_rx(b"\x11\x13LINKR_ZM:go\r\n");
        assert!(
            matches!(transfer.phase, Phase::Run { .. }),
            "go was hidden: {:?}",
            transfer.phase
        );
        assert_eq!(transfer.abort(), b"\x03\x03");
        assert!(!transfer.busy(), "the view must be free again");
    }

    /// Once `rz` has held the port, the target's last byte before its report
    /// is the end of a ZMODEM frame — `\x8a`, and **not** a newline. Read raw,
    /// `printf '…rc…'` would land glued to `**\x18B…`, and [`Transfer::on_marker`],
    /// which only reads a marker at position zero, would never see it:
    /// `launch_rc` stays `None` and the run ends on the [`SETTLE`] fallback
    /// rather than on the target's word. That is what let a second transfer
    /// type its launch command into a console the first one's `rz` still
    /// owned — the bytes were swallowed whole, with no echo, and the run died
    /// on the step timeout.
    #[test]
    fn the_report_after_a_zmodem_frame_is_still_read() {
        // What the target is told to print: the report opens its own line.
        let wrapped = wrap(1, "sz -e -O -q -- /tmp/linkr-zm-probe.bin");
        assert!(
            wrapped.contains(&format!("printf '\\n{MARKER}rc 1 ")),
            "the report must open a line of its own: {wrapped}"
        );

        let mut transfer = Transfer {
            local: PathBuf::from("/etc/hostname"),
            target: "/tmp/linkr-zm-rc.bin".to_string(),
            ..Transfer::default()
        };
        transfer.phase = Phase::Run {
            at: Instant::now() + RUN_TIMEOUT,
        };
        // The step `start()` would have handed the launch command.
        transfer.seq = 1;
        // What the Bee actually printed, byte for byte: the frame, then the
        // report on the line that `wrap` made it start.
        transfer.on_rx(b"**\x18B0900000000a87c\r\x8a\nLINKR_ZM:rc 1 0\n");
        assert_eq!(
            transfer.launch_rc,
            Some(0),
            "the target's own word went unread"
        );
        assert!(!transfer.busy(), "the report ends the run");
        match &transfer.outcome {
            Outcome::Ok(text) => assert!(text.contains("Sent"), "{text}"),
            other => panic!("expected success, got {other:?}"),
        }
    }

    /// The gap between "the host is done" and "the target says so" is not a
    /// rounding error. On the Bee it measured **12.8 s** and then **56.0 s**,
    /// and for all of it that shell is inside `rz` and swallows every byte
    /// typed at it. A budget below that calls the gap `Sent` and hands the
    /// console to the next transfer too early.
    #[test]
    fn the_settle_budget_covers_the_lag_the_target_actually_had() {
        assert!(
            SETTLE > Duration::from_secs(12),
            "the Bee reported after 12.8 s; {SETTLE:?} would call that gap Sent"
        );
    }

    /// `sz` writes `OO` and exits in the same breath, and the frame loop reads
    /// time before bytes: `poll` reaps the exit and moves the run on, *then*
    /// `pump` decides whether the child may still speak. Gating the child on
    /// [`Phase::Run`] alone threw that `OO` away, so the receiver waited for a
    /// frame that never came — on the Bee, `rz` held the console for **56.0 s**
    /// over it. Locally the same code passes, because `sz`'s exit is not
    /// reaped in the ~0.4 ms between `08` and `OO`.
    #[test]
    fn the_childs_last_output_is_still_pumped_once_the_run_settles() {
        if !lrzsz_installed() {
            return;
        }
        let mut transfer = Transfer {
            local: std::env::temp_dir().join("linkr-pump-settle.bin"),
            target: "/tmp/linkr-zm-pump.bin".to_string(),
            probe: Some(probe_all("/tmp")),
            direction: Direction::Recv,
            ..Transfer::default()
        };
        ready(&mut transfer, true);
        transfer.start().expect("the host half starts");

        // The frame loop's order, frozen: `poll` has reaped the child and
        // moved the run on, nothing is queued for typing, and the child's
        // output has not been drained yet.
        transfer.queue.clear();
        transfer.phase = Phase::Settle {
            until: Instant::now() + STEP_TIMEOUT,
        };

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut got = None;
        while Instant::now() < deadline {
            if let Some(bytes) = transfer.pump(Instant::now()) {
                got = Some(bytes);
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let got = got
            .expect("the child's pending output must still reach the link after the run settles");
        assert!(!got.is_empty(), "the child spoke before it was paused");

        let _ = transfer.abort();
    }

    /// An unfinished receive takes its half-written file with it.
    ///
    /// `rz` lands the *sender's* file name beside the destination and this
    /// module renames it once the bytes are in; the form refuses to start a
    /// receive while that path is taken, so anything found there **after** a
    /// run has begun is that run's own. Leaving it behind turned one aborted
    /// download into a standing refusal: a dropped link left a 0-byte
    /// `linkr-zm-probe.bin`, and the next two round trips never started their
    /// receive at all — the target's console was never told to send anything.
    #[test]
    fn an_aborted_receive_sweeps_the_file_it_was_writing() {
        let dir = std::env::temp_dir().join("linkr-sweep-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a directory to receive into");
        let staged = dir.join("incoming.bin");
        std::fs::write(&staged, b"half a download").expect("the staged file");

        let mut transfer = Transfer {
            local: dir.join("out.bin"),
            target: "/tmp/incoming.bin".to_string(),
            direction: Direction::Recv,
            ..Transfer::default()
        };
        transfer.fail("The link dropped.".to_string());

        assert!(
            !staged.exists(),
            "an aborted receive must sweep {}",
            staged.display()
        );

        // When the sender's name *is* the name asked for there is nothing to
        // sweep, and nothing here may ever delete the destination.
        std::fs::write(&staged, b"mine").expect("the destination");
        let mut transfer = Transfer {
            local: staged.clone(),
            target: "/tmp/incoming.bin".to_string(),
            direction: Direction::Recv,
            ..Transfer::default()
        };
        transfer.fail("The link dropped.".to_string());
        assert!(staged.exists(), "the destination is never swept");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The whole feature, end to end, with a real `sh` standing in for the
    /// target's console and real `lrzsz` on both halves.
    ///
    /// This is the test that earns the others: the launch command is typed
    /// into a shell over pipes, the shell runs `rz` on its own stdio exactly
    /// as a board would, and the bytes come back through [`Transfer::on_rx`].
    /// Nothing here needs a device — but it does need `lrzsz` on this host,
    /// so it skips quietly when it is not installed.
    #[test]
    fn zmodem_moves_a_file_across_a_console_shell() {
        if !HostTools::detect().zmodem() {
            eprintln!("skipping: lrzsz is not installed on this host");
            return;
        }
        // Two directories on purpose: on a board these are two *machines*.
        // Pointing both halves at one directory would have the target's `rz`
        // truncate the very file `sz` is reading.
        let root = std::env::temp_dir().join(format!("linkr-zmodem-{}", std::process::id()));
        let from = root.join("host");
        let to = root.join("target");
        std::fs::create_dir_all(&from).expect("mkdir host");
        std::fs::create_dir_all(&to).expect("mkdir target");
        // The names differ on purpose: the run has to settle that with the
        // `mv` inside the launch command, not with a digest afterwards.
        let local = from.join("payload.bin");
        let dest = to.join("renamed.bin");
        let payload: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&local, &payload).expect("write");

        let mut transfer = Transfer {
            direction: Direction::Send,
            local: local.clone(),
            target: dest.display().to_string(),
            probe: Some(probe_all(&to.display().to_string())),
            ..Transfer::default()
        };
        ready(&mut transfer, true);
        // A pipe is not a tty: nothing translates CR into a newline the way a
        // console's line discipline does, so the commands end with a bare LF
        // here. That is exactly what `set_enter` exists to express.
        transfer.set_enter(b"\n".to_vec());
        transfer.start().expect("the run starts");

        // The console: a shell reading the commands we type, with `rz` as its
        // child — the same shape as a board sitting at a prompt.
        let mut shell = Command::new("sh")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn the stand-in console");
        let shell_out = shell.stdout.take().expect("piped");
        let (tx, rx) = sync_channel::<Vec<u8>>(64);
        std::thread::spawn(move || pump_read(shell_out, tx, 4096));

        let deadline = Instant::now() + Duration::from_secs(60);
        let mut stdin = shell.stdin.take().expect("piped");
        while Instant::now() < deadline {
            while let Some(chunk) = transfer.pump(Instant::now()) {
                stdin
                    .write_all(&chunk)
                    .expect("the console takes the bytes");
            }
            while let Ok(chunk) = rx.try_recv() {
                transfer.on_rx(&chunk);
            }
            transfer.poll(Instant::now());
            if !transfer.busy() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }

        drop(stdin);
        let _ = shell.wait();
        assert!(
            !transfer.busy(),
            "the run did not finish: {:?}",
            transfer.phase
        );
        match &transfer.outcome {
            Outcome::Ok(text) => assert!(text.contains("Sent"), "{text}"),
            other => panic!("the transfer did not succeed: {other:?}"),
        }
        let got = std::fs::read(&dest).expect("the destination was written under the new name");
        assert_eq!(got, payload, "the bytes are the bytes");
        assert_eq!(
            std::fs::read(&local).expect("the source is untouched"),
            payload,
            "sending must not consume the source"
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
