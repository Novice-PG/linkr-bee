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
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

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
fn run(helper: &Helper) -> Option<String> {
    let mut child = Command::new(helper.program)
        .args(helper.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let mut reader = stdout;
        let _ = reader.read_to_end(&mut buffer);
        let _ = tx.send(buffer);
    });
    let deadline = Instant::now() + helper.timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // Up to EOF the reader is done with the child; it gets the
                // rest of its own budget rather than a blocking `join()`,
                // because a stray grandchild holding the pipe open must not
                // park the frame loop either.
                let buffer = rx.recv_timeout(helper.timeout).unwrap_or_default();
                if !status.success() {
                    return None;
                }
                return String::from_utf8(buffer).ok();
            }
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

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
/// on the same thread as the drawing.
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
    if let Some(mut stdin) = child.stdin.take() {
        let payload = text.to_string();
        std::thread::spawn(move || {
            // The read end is gone once the child is killed; that surfaces as
            // a broken pipe, which is the point of doing it off-thread.
            let _ = stdin.write_all(payload.as_bytes());
        });
    }
    let deadline = Instant::now() + writer.timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(None) | Err(_) => {
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
