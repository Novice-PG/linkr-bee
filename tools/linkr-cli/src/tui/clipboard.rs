//! Reading the system clipboard for `Ctrl+Shift+V`.
//!
//! A terminal program only gets the clipboard for free on the way *out*:
//! `OSC 52` writes it (`terminal_view::osc52_write`). On the way in the
//! portable route is the emulator's own paste key, which arrives as one
//! bracketed `Event::Paste` (`mod::paste`) — that is the path taken on every
//! desktop that binds `Ctrl+Shift+V` itself, and it needs nothing from here.
//!
//! When the key reaches this program instead, the text has to come from the
//! platform, and outside a browser that means one of the small helper tools
//! the desktops ship: `wl-paste` (Wayland), `xclip` / `xsel` (X11), `pbpaste`
//! (macOS), `Get-Clipboard` (Windows). None of them is assumed to exist: each
//! helper gets its own deadline, the first answer wins, and one that is
//! missing or wedged costs at most its own timeout and is killed rather than
//! left behind.
//!
//! The other direction is [`write`]: an inbound `OSC 52` asks the *emulator*
//! to set the clipboard, and we are the emulator here — `TermGrid` keeps the
//! base64 payload (`terminal_view::take_clipboard`) and `mod::on_core_event`
//! hands it over.
//!
//! `None` is what the caller reports — `mod::request_paste` turns it into the
//! same kind of news the web frontend gives when the browser denies
//! `navigator.clipboard.readText()` (`web/app.js` → `pasteUnavailable`).

use std::io::{Read, Write};
use std::process::{ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Worker threads a helper has left behind, from spawn to exit.
///
/// Every one of them has to go away with its helper: a reader parked on a
/// pipe that a stray grandchild keeps open would otherwise sit there for the
/// rest of the session — one thread and one read end per paste — with nothing
/// in the program able to see it, let alone reclaim it.
static LIVE_WORKERS: AtomicUsize = AtomicUsize::new(0);

/// Counts one worker across **every** return path (`Drop`, not a single
/// `fetch_sub` at the end of the body, so an early `return` cannot lose it).
struct Worker;

impl Worker {
    fn start() -> Self {
        LIVE_WORKERS.fetch_add(1, Ordering::SeqCst);
        Worker
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        LIVE_WORKERS.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
fn live_workers() -> usize {
    LIVE_WORKERS.load(Ordering::SeqCst)
}

/// One way to ask the desktop for its clipboard.
struct Helper {
    program: &'static str,
    args: &'static [&'static str],
    /// Long enough for a real clipboard read, short enough that a helper
    /// started without a display (an `xclip` on Wayland, say) cannot hold the
    /// frame loop hostage: the key handler runs on the same thread as the
    /// drawing.
    timeout: Duration,
    /// PowerShell appends a line ending of its own; every other helper
    /// answers with the selection byte for byte.
    trims_line_ending: bool,
}

/// Deadline for the helpers that answer in milliseconds.
const FAST: Duration = Duration::from_millis(350);

/// The helpers this platform may have, in the order they are tried.
fn helpers() -> Vec<Helper> {
    #[cfg(target_os = "windows")]
    {
        vec![Helper {
            program: "powershell.exe",
            args: &[
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Get-Clipboard -Raw",
            ],
            // Cold PowerShell is slow, but it is the only route there is.
            timeout: Duration::from_millis(1_500),
            trims_line_ending: true,
        }]
    }
    #[cfg(target_os = "macos")]
    {
        vec![Helper {
            program: "pbpaste",
            args: &[],
            timeout: FAST,
            trims_line_ending: false,
        }]
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        vec![
            Helper {
                program: "wl-paste",
                args: &["--no-newline"],
                timeout: FAST,
                trims_line_ending: false,
            },
            Helper {
                program: "xclip",
                args: &["-selection", "clipboard", "-o"],
                timeout: FAST,
                trims_line_ending: false,
            },
            Helper {
                program: "xsel",
                args: &["--clipboard", "--output"],
                timeout: FAST,
                trims_line_ending: false,
            },
        ]
    }
}

/// Read the clipboard **off the frame loop** and hand back where the answer
/// will land.
///
/// The chain below can spend `FAST` per helper (1.5 s for PowerShell) on a
/// wedged desktop, and the key handler runs on the same thread as the drawing
/// — so a paste must not be what stalls the interface. `mod::poll_clipboard`
/// collects the receiver on a later tick; until then the key has already
/// returned.
pub fn spawn_read() -> std::sync::mpsc::Receiver<Option<String>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(read());
    });
    rx
}

/// Set the clipboard from an inbound `OSC 52`, **also** off the frame loop: a
/// device that copies to the host must not freeze the terminal for the length
/// of a helper that is not answering. `false` (no helper took it) reaches the
/// caller through the receiver, one tick later.
pub fn spawn_write(payload: String) -> std::sync::mpsc::Receiver<bool> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(write(&payload));
    });
    rx
}

/// The clipboard as text, or `None` when no helper could answer in time.
///
/// Callers treat `Some("")` like `None`: an empty selection has nothing to
/// paste and reporting it as a failure reads better than a silent no-op.
fn read() -> Option<String> {
    helpers().into_iter().find_map(|helper| {
        let text = run(&helper)?;
        Some(if helper.trims_line_ending {
            trim_line_ending(text)
        } else {
            text
        })
    })
}

/// Run one helper, giving up at its deadline.
///
/// The answer is read on its own thread. A helper that writes more than the
/// 64 KiB pipe holds blocks on its own `write()` until somebody reads, so
/// waiting for the exit first would only ever reach the deadline — which is
/// how a paste of anything bigger than that came out as "clipboard
/// unavailable".
///
/// The bytes are accumulated **as they arrive**, not when the pipe finally
/// reports EOF: a helper that hands its standard output to a background
/// grandchild (a daemon, a selection owner, anything that inherits the file
/// descriptors) exits successfully without ever closing the pipe, and what it
/// had already printed is the clipboard answer. Reading only at EOF threw
/// that away — the paste came back as `Some("")`, which `read()` reports as
/// "clipboard unavailable" — and left the reader thread parked on a
/// descriptor nobody was ever going to close.
fn run(helper: &Helper) -> Option<String> {
    let mut child = Command::new(helper.program)
        .args(helper.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let eof = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));
    let worker = Worker::start();
    {
        let bytes = Arc::clone(&bytes);
        let eof = Arc::clone(&eof);
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            let _worker = worker;
            copy_out(stdout, &bytes, &eof, &stop);
        });
    }
    let deadline = Instant::now() + helper.timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(None) | Err(_) => {
                stop.store(true, Ordering::Release);
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    // The helper is gone; only a stray grandchild that inherited the pipe can
    // still be holding it open. The reader gets what is left of *this*
    // helper's own budget — never a fresh one, or a child that exits just
    // before its deadline would cost a second timeout — and then whatever has
    // arrived is taken either way.
    while !eof.load(Ordering::Acquire) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
    // On the EOF path the reader has already left; on the grandchild path
    // this is what makes it leave, closing the pipe behind it instead of
    // parking one thread and one file descriptor per paste for the session.
    stop.store(true, Ordering::Release);
    if !status.success() {
        return None;
    }
    let buffer = bytes.lock().expect("clipboard buffer").clone();
    String::from_utf8(buffer).ok()
}

/// Drain a helper's standard output into `bytes`, then say so through `eof`.
///
/// The wait for readability happens in [`reader_ready`], where `stop` is
/// honoured, so the transfer itself never has to be interrupted from the
/// outside.
fn copy_out(mut stream: ChildStdout, bytes: &Mutex<Vec<u8>>, eof: &AtomicBool, stop: &AtomicBool) {
    let mut chunk = [0u8; 16 * 1024];
    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        if !reader_ready(&mut stream, stop) {
            break;
        }
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => bytes
                .lock()
                .expect("clipboard buffer")
                .extend_from_slice(&chunk[..read]),
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    eof.store(true, Ordering::Release);
}

/// Wait until `stream` can be read without blocking, checking for
/// cancellation as we go. `false` means the caller was told to give up.
///
/// On the platforms without `poll(2)` there is no way to interrupt a blocked
/// read from another thread, so this is a plain "ready": the data still
/// arrives in pieces (which is what matters to the caller), but a helper
/// whose pipe is held open by a grandchild keeps its reader until that
/// grandchild lets go.
#[cfg(unix)]
fn reader_ready(stream: &mut ChildStdout, stop: &AtomicBool) -> bool {
    use std::os::fd::AsRawFd;
    wait_for(stream.as_raw_fd(), libc::POLLIN, stop)
}

#[cfg(unix)]
fn writer_ready(stream: &mut ChildStdin, stop: &AtomicBool) -> bool {
    use std::os::fd::AsRawFd;
    wait_for(stream.as_raw_fd(), libc::POLLOUT, stop)
}

#[cfg(unix)]
fn wait_for(fd: libc::c_int, events: libc::c_short, stop: &AtomicBool) -> bool {
    let mut fds = libc::pollfd {
        fd,
        events,
        revents: 0,
    };
    loop {
        // `poll` sleeps, so the 50 ms is what bounds how long a cancelled
        // worker takes to notice; a readiness event returns at once.
        let ready = unsafe { libc::poll(&mut fds, 1, 50) };
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        if ready < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return false;
        }
        // Ready, hung up or broken: `read`/`write` decides what that means.
        if ready > 0 {
            return true;
        }
    }
}

#[cfg(not(unix))]
fn reader_ready(_stream: &mut ChildStdout, _stop: &AtomicBool) -> bool {
    true
}

#[cfg(not(unix))]
fn writer_ready(_stream: &mut ChildStdin, _stop: &AtomicBool) -> bool {
    true
}

/// Take the blocking wait out of the writer's `write(2)`.
///
/// A pipe write that does not fit blocks until somebody reads, and no amount
/// of polling around it can help: the cancellation flag would never be looked
/// at again. With `O_NONBLOCK` a full pipe comes back as `WouldBlock`, the
/// thread returns to [`writer_ready`], and the helper's departure is noticed
/// there — killing the helper only closes *its* end, a child it left behind
/// may still be holding the read end open. The flag lives on the write end's
/// own file description, so the helper's inherited descriptor is untouched.
#[cfg(unix)]
fn nonblocking(stream: &mut ChildStdin) {
    use std::os::fd::AsRawFd;
    let fd = stream.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags >= 0 {
        unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    }
}

#[cfg(not(unix))]
fn nonblocking(_stream: &mut ChildStdin) {}

/// Drop the one line ending PowerShell adds of its own, so pasting an API key
/// into a single-line field does not also send an Enter to the target.
fn trim_line_ending(mut text: String) -> String {
    if text.ends_with('\n') {
        text.pop();
        if text.ends_with('\r') {
            text.pop();
        }
    }
    text
}

/// One way to hand text *to* the desktop clipboard: every helper of this kind
/// reads standard input and exits, so the shape is a program, its arguments
/// and the deadline it is given.
struct Writer {
    program: &'static str,
    args: &'static [&'static str],
    timeout: Duration,
}

/// The helpers this platform may have, in the order they are tried.
fn writers() -> Vec<Writer> {
    #[cfg(target_os = "windows")]
    {
        // `clip.exe` is part of Windows and reads stdin.
        vec![Writer {
            program: "clip",
            args: &[],
            timeout: FAST,
        }]
    }
    #[cfg(target_os = "macos")]
    {
        vec![Writer {
            program: "pbcopy",
            args: &[],
            timeout: FAST,
        }]
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        vec![
            Writer {
                program: "wl-copy",
                args: &[],
                timeout: FAST,
            },
            Writer {
                program: "xclip",
                args: &["-selection", "clipboard", "-in"],
                timeout: FAST,
            },
            Writer {
                program: "xsel",
                args: &["--clipboard", "--input"],
                timeout: FAST,
            },
        ]
    }
}

/// Set the system clipboard from an inbound `OSC 52`.
///
/// The sequence carries base64 and the clipboard holds text, so the payload is
/// decoded first. `false` means no helper took it — nothing of this kind is
/// installed, the desktop is unreachable, or the payload was not base64 — and
/// the caller turns that into one notice instead of one per sequence.
pub fn write(payload: &str) -> bool {
    let Some(text) = decode(payload) else {
        return false;
    };
    write_text(&text)
}

/// Base64 of an `OSC 52` payload as text. Returns `None` for a payload that
/// does not decode: a mangled sequence must not reach the clipboard as the
/// garbage it arrived as.
fn decode(payload: &str) -> Option<String> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(payload.trim())
        .ok()?;
    // An empty payload is xterm's "leave the clipboard alone", not "clear it":
    // there is nothing to write.
    if bytes.is_empty() {
        return None;
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// First helper that accepts the text wins, exactly like [`read`].
fn write_text(text: &str) -> bool {
    writers().into_iter().any(|writer| run_write(&writer, text))
}

/// Give `text` to one helper, giving up at its deadline.
///
/// The bytes are written from a separate thread: a helper that stops reading
/// would otherwise block here the moment the 64 KiB pipe fills, and this runs
/// on the same thread as the drawing. The thread is stopped as soon as the
/// helper is — a payload the helper never got to is not worth a thread that
/// outlives the program, and a grandchild holding the read end would
/// otherwise keep it blocked for good.
fn run_write(writer: &Writer, text: &str) -> bool {
    let Ok(mut child) = Command::new(writer.program)
        .args(writer.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let stop = Arc::new(AtomicBool::new(false));
    if let Some(mut stdin) = child.stdin.take() {
        nonblocking(&mut stdin);
        let payload = text.as_bytes().to_vec();
        let stop = Arc::clone(&stop);
        let worker = Worker::start();
        std::thread::spawn(move || {
            let _worker = worker;
            let mut written = 0;
            while written < payload.len() {
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                if !writer_ready(&mut stdin, &stop) {
                    return;
                }
                match stdin.write(&payload[written..]) {
                    // A closed pipe (`Ok(0)` or an error) is the helper
                    // telling us it is done: that surfaces as a broken pipe,
                    // which is the point of doing it off-thread. `WouldBlock`
                    // is a full pipe on a non-blocking descriptor — back to
                    // `writer_ready` to wait for it to drain.
                    Ok(0) => return,
                    Ok(count) => written += count,
                    Err(err)
                        if err.kind() == std::io::ErrorKind::Interrupted
                            || err.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(_) => return,
                }
            }
        });
    }
    let deadline = Instant::now() + writer.timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                stop.store(true, Ordering::Release);
                return status.success();
            }
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(None) | Err(_) => {
                stop.store(true, Ordering::Release);
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn helper(program: &'static str, args: &'static [&'static str], timeout_ms: u64) -> Helper {
        Helper {
            program,
            args,
            timeout: Duration::from_millis(timeout_ms),
            trims_line_ending: false,
        }
    }

    /// Whatever the platform, the list is what `read()` walks — a platform
    /// with no entry would silently turn every paste into a toast.
    #[test]
    fn every_platform_advertises_at_least_one_helper() {
        let helpers = helpers();
        assert!(!helpers.is_empty());
        let mut programs: Vec<&str> = helpers.iter().map(|helper| helper.program).collect();
        programs.sort_unstable();
        let before = programs.len();
        programs.dedup();
        assert_eq!(before, programs.len(), "a helper must not be listed twice");
    }

    /// A desktop without the tool installed must not pay for the attempt: the
    /// spawn fails at once and `read()` moves on to the next helper.
    #[test]
    fn a_helper_that_is_not_installed_costs_nothing() {
        let started = Instant::now();
        assert_eq!(
            run(&helper("linkr-no-such-clipboard-helper", &[], 300)),
            None
        );
        assert!(
            started.elapsed() < Duration::from_millis(300),
            "ENOENT has to return immediately, not at the deadline"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_clipboard_text_comes_back_verbatim() {
        assert_eq!(
            run(&helper("/bin/sh", &["-c", "printf 'key-1234'"], 2_000)),
            Some("key-1234".to_string())
        );
    }

    /// A helper that writes more than the 64 KiB pipe holds blocks on its own
    /// `write()` until somebody reads. Waiting for its exit first therefore
    /// saw only the deadline, and every paste longer than a pipe came back as
    /// "clipboard unavailable".
    #[cfg(unix)]
    #[test]
    fn an_answer_bigger_than_the_pipe_still_comes_through() {
        let args: &'static [&'static str] =
            Box::leak(vec!["-c", "yes | head -c 200000"].into_boxed_slice());

        let text = run(&helper("/bin/sh", args, 3_000)).expect("200 KB must arrive");

        assert_eq!(text.len(), 200_000, "byte for byte, not truncated");
    }

    #[cfg(unix)]
    #[test]
    fn a_helper_that_reports_failure_yields_nothing() {
        // xclip exits non-zero when there is no display to ask — the toast
        // path, not a paste of whatever happened to be on stdout.
        assert_eq!(run(&helper("/bin/sh", &["-c", "exit 3"], 2_000)), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_wedged_helper_is_killed_at_its_deadline() {
        let started = Instant::now();
        assert_eq!(run(&helper("/bin/sh", &["-c", "sleep 5"], 120)), None);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the frame loop may not wait for a helper that hung"
        );
    }

    /// A helper whose output is inherited by a background grandchild exits 0
    /// **without ever closing the pipe** — the author of the original code
    /// knew about that ("a stray grandchild holding the pipe open"), but the
    /// answer was then read only at EOF. Two things went wrong: the bytes the
    /// helper had already printed were dropped, so the paste reported
    /// "clipboard unavailable" instead of pasting, and the reader thread stayed
    /// parked on a descriptor nobody would ever close, one thread and one file
    /// descriptor per paste.
    ///
    /// The budget is part of the same bug: after the child exited the reader
    /// was given a *fresh* `helper.timeout` on top of the one already spent,
    /// so a helper that exits just before its deadline cost a second one.
    #[cfg(unix)]
    #[test]
    fn a_grandchild_holding_the_pipe_delivers_the_answer_and_parks_nothing() {
        let dir = std::env::temp_dir().join(format!("linkr-clip-read-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let pidfile = dir.join("pid");
        // Prints the answer, then leaves `sleep` holding the write end so the
        // pipe never reports EOF. The pid is written down so the test can
        // take the grandchild back with it.
        let script: &'static str = Box::leak(
            format!(
                "printf 'key-1234'; (sleep 30 & echo $! > '{}');",
                pidfile.display()
            )
            .into_boxed_str(),
        );
        let args: &'static [&'static str] = Box::leak(vec!["-c", script].into_boxed_slice());

        let before = live_workers();
        let started = Instant::now();
        let text = run(&helper("/bin/sh", args, 500));

        assert_eq!(
            text.as_deref(),
            Some("key-1234"),
            "what the helper printed is the clipboard, pipe or no pipe"
        );
        let elapsed = started.elapsed();
        // The reader waits out what is left of this helper's budget, never a
        // second one: 500 ms is the deadline, 750 ms would be the budget paid
        // twice.
        assert!(
            elapsed < Duration::from_millis(750),
            "the helper's budget was renewed after it exited ({elapsed:?})"
        );

        // The reader has to let go of the pipe the grandchild is holding.
        let mut let_go = false;
        for _ in 0..200 {
            if live_workers() <= before {
                let_go = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        // Take the grandchild home either way: a test must not leave a
        // process behind on the machine that runs it.
        if let Ok(pid) = std::fs::read_to_string(&pidfile) {
            if let Ok(pid) = pid.trim().parse::<u32>() {
                let _ = std::process::Command::new("kill")
                    .args(["-9", &pid.to_string()])
                    .status();
            }
        }
        let _ = std::fs::remove_dir_all(&dir);

        assert!(
            let_go,
            "the reader thread is still parked on the pipe ({} workers)",
            live_workers()
        );
    }

    /// The same shape on the way out: a helper that lets a grandchild hold
    /// standard input open never drains the payload, so the writer thread
    /// would block on a full pipe for the rest of the session. The call
    /// itself must still report at once — and take the thread with it.
    #[cfg(unix)]
    #[test]
    fn a_writer_does_not_outlive_a_helper_that_never_finishes_reading() {
        let dir = std::env::temp_dir().join(format!("linkr-clip-write-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let pidfile = dir.join("pid");
        let script: &'static str =
            Box::leak(format!("sleep 30 & echo $! > '{}'", pidfile.display()).into_boxed_str());
        let args: &'static [&'static str] = Box::leak(vec!["-c", script].into_boxed_slice());

        let before = live_workers();
        let accepted = run_write(
            &Writer {
                program: "/bin/sh",
                args,
                timeout: Duration::from_millis(500),
            },
            // Twice the pipe: without a reader it blocks after the first 64 KiB.
            &"x".repeat(128 * 1024),
        );
        assert!(accepted, "the helper exited successfully");

        let mut let_go = false;
        for _ in 0..200 {
            if live_workers() <= before {
                let_go = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if let Ok(pid) = std::fs::read_to_string(&pidfile) {
            if let Ok(pid) = pid.trim().parse::<u32>() {
                let _ = std::process::Command::new("kill")
                    .args(["-9", &pid.to_string()])
                    .status();
            }
        }
        let _ = std::fs::remove_dir_all(&dir);

        assert!(
            let_go,
            "the writer thread is still blocked on the pipe ({} workers)",
            live_workers()
        );
    }

    #[test]
    fn powershell_line_endings_are_not_pasted_as_an_enter() {
        assert_eq!(trim_line_ending("token\r\n".to_string()), "token");
        assert_eq!(trim_line_ending("token\n".to_string()), "token");
        assert_eq!(
            trim_line_ending("line one\nline two".to_string()),
            "line one\nline two",
            "only the trailing ending goes, the paragraph keeps its breaks"
        );
        assert_eq!(trim_line_ending(String::new()), String::new());
    }

    /// An `OSC 52` carries base64; the clipboard holds text, and a payload
    /// that is not base64 at all must be refused rather than written out as
    /// the bytes it arrived as.
    #[test]
    fn an_osc52_payload_decodes_to_the_text_the_clipboard_holds() {
        assert_eq!(decode("aGVsbG8=").as_deref(), Some("hello"));
        assert_eq!(
            decode("5Lit5paH").as_deref(),
            Some("中文"),
            "the payload is UTF-8 text"
        );
        assert_eq!(
            decode("aGVsbG8=\n").as_deref(),
            Some("hello"),
            "a line ending from the transport rides along"
        );
        assert!(decode("not base64!").is_none());
        assert!(decode("").is_none());
    }

    /// The text reaches the helper verbatim — anything else and the device's
    /// clipboard lands in the host mangled.
    #[cfg(unix)]
    #[test]
    fn the_text_reaches_the_helper() {
        let path = std::env::temp_dir().join(format!("linkr-osc52-{}", std::process::id()));
        let script: &'static str =
            Box::leak(format!("cat > '{}'", path.display()).into_boxed_str());
        let args: &'static [&'static str] = Box::leak(vec!["-c", script].into_boxed_slice());

        let ok = run_write(
            &Writer {
                program: "/bin/sh",
                args,
                timeout: Duration::from_secs(2),
            },
            "hello from the device",
        );

        let written = std::fs::read_to_string(&path).unwrap_or_default();
        let _ = std::fs::remove_file(&path);
        assert!(ok, "the helper reported failure");
        assert_eq!(written, "hello from the device");
    }

    /// A helper that never reads would fill the 64 KiB pipe and then block
    /// the writer for good — which, if it ran on this thread, would stop the
    /// drawing along with it. The payload here is twice the pipe: the call
    /// still returns at its own deadline.
    #[cfg(unix)]
    #[test]
    fn a_helper_that_stops_reading_does_not_hold_the_frame_loop() {
        let args: &'static [&'static str] = Box::leak(vec!["-c", "sleep 5"].into_boxed_slice());
        let started = Instant::now();
        let accepted = run_write(
            &Writer {
                program: "/bin/sh",
                args,
                timeout: Duration::from_millis(120),
            },
            &"x".repeat(128 * 1024),
        );

        assert!(!accepted, "nobody read it, so nobody took it");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the frame loop waited {:?}",
            started.elapsed()
        );
    }

    /// Nothing installed is the common case on a server: the attempt costs
    /// one failed spawn, not a timeout, and it is reported as a failure.
    #[test]
    fn a_missing_writer_is_reported_not_waited_for() {
        let args: &'static [&'static str] = Box::leak(Vec::new().into_boxed_slice());
        let started = Instant::now();
        assert!(!run_write(
            &Writer {
                program: "linkr-no-such-clipboard-writer",
                args,
                timeout: Duration::from_millis(300),
            },
            "text",
        ));
        assert!(
            started.elapsed() < Duration::from_millis(300),
            "ENOENT has to return immediately, not at the deadline"
        );
    }
}
