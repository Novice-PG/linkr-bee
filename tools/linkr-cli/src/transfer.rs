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

/// Hard ceiling on one ZMODEM run, so a wedged peer cannot pin the view open
/// for the rest of the session.
const RUN_TIMEOUT: Duration = Duration::from_secs(1800);

/// After the host's process exits, how long to keep watching for the target's
/// own `rc` line before deciding it is not coming.
const SETTLE: Duration = Duration::from_millis(1500);

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
/// * the guard refuses to clobber an existing destination *before* any byte
///   moves, instead of discovering it afterwards.
/// * `sz` puts the sender's file name in the ZFILE header, which is `local`'s
///   name; when the form asked for a different one, a single `mv` inside the
///   same command settles it. Not a digest — a name.
fn launch_recv(dest: &str, dir: &str, written: &str) -> Result<String, String> {
    let dest_q = quote_shell(dest).map_err(|err| err.to_string())?;
    let dir_q = quote_shell(dir).map_err(|err| err.to_string())?;
    let written_q = quote_shell(written).map_err(|err| err.to_string())?;
    Ok(format!(
        "if [ -e {dest_q} ]; then printf '{MARKER}exists %s\\n' {dest_q}; false; else printf '{MARKER}go\\n'; (cd {dir_q} && rz -e -O -y 2>/dev/null) && {{ [ {written_q} = {dest_q} ] || mv -f {written_q} {dest_q}; }}; fi"
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
fn wrap(seq: u32, body: &str) -> String {
    format!("{body}; printf '{MARKER}rc {seq} %s\\n' \"$?\"")
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
            total: None,
            status: String::new(),
            queue: VecDeque::new(),
            next_at: None,
            steps: VecDeque::new(),
            seq: 0,
            cap: Vec::new(),
            scan: Vec::new(),
            proc: None,
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
                self.expected_sha256 = if probe.digest() {
                    local_sha256(&self.local).unwrap_or_default()
                } else {
                    String::new()
                };
                let digest = self.expected_sha256.clone();
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

    /// Bytes for the link this tick: what we are typing first, then — and only
    /// while the child owns the line — the child's own stream. One chunk per
    /// interval, whatever either of them produced.
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
        } else if matches!(self.phase, Phase::Run { .. }) {
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
    fn take_scan_line(&mut self) -> Option<String> {
        let end = self.scan.iter().position(|b| *b == b'\n')?;
        let mut line = self.scan.drain(..=end).collect::<Vec<u8>>();
        line.pop();
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        String::from_utf8(line).ok()
    }

    fn on_marker(&mut self, line: &str) {
        let Some(rest) = line.strip_prefix(MARKER) else {
            return;
        };
        let mut parts = rest.splitn(3, ' ');
        match parts.next() {
            Some("go") => {
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
                    self.phase = Phase::Run {
                        at: Instant::now() + RUN_TIMEOUT,
                    };
                }
            }
            Some("exists") => {
                let path = parts.next().unwrap_or("").to_string();
                self.fail(format!(
                    "The target already has {path}; rename or remove it."
                ));
            }
            Some("missing") => {
                let path = parts.next().unwrap_or("").to_string();
                self.fail(format!("The target has no file at {path}."));
            }
            Some("rc") => {
                let (Some(seq), Some(code)) = (
                    parts.next().and_then(|s| s.parse::<u32>().ok()),
                    parts.next().and_then(|s| s.parse::<i32>().ok()),
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
                self.moved = moved;
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
                    // The target never reported. The host's clean exit is the
                    // best evidence there is, and saying so beats stalling —
                    // but it is reported as what it is.
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

    fn stop_host(&mut self) {
        if let Some(mut proc) = self.proc.take() {
            proc.kill();
        }
    }

    fn fail(&mut self, reason: String) {
        self.stop_host();
        self.queue.clear();
        self.steps.clear();
        self.download = None;
        self.phase = Phase::Done;
        self.outcome = Outcome::Failed(reason);
    }

    /// Drop everything, keeping the form. Also what a reconnect does.
    pub fn reset(&mut self) {
        self.stop_host();
        self.queue.clear();
        self.steps.clear();
        self.cap.clear();
        self.scan.clear();
        self.seq = 0;
        self.next_at = None;
        self.launch_rc = None;
        self.download = None;
        self.moved = 0;
        self.status.clear();
        self.outcome = Outcome::Idle;
        self.phase = Phase::Idle;
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
                probe.home = parts.next().unwrap_or("").trim().to_string();
            }
            _ => {}
        }
    }
    probe
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

    /// An existing destination stops an upload before a byte moves: the guard
    /// is the target's own `exists` line, not a guess made on this side.
    #[test]
    fn an_existing_target_file_stops_the_run_before_any_bytes_move() {
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
        assert!(transfer.proc.is_none(), "the child must be killed");
        assert!(!transfer.busy(), "the view must be free again");
    }

    /// Aborting kills the child and hands back two control characters that
    /// break the target back to a prompt — unpaced, because a transfer being
    /// stopped must not wait its turn in the queue.
    #[test]
    fn an_abort_kills_the_child_and_returns_an_unpaced_break() {
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
