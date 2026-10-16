//! The clipboard, and the terminal's own copy sequence.
//!
//! Two doors onto the same thing. OSC 52 is the *terminal's* clipboard: the
//! text rides one escape sequence through whatever terminal the interface is
//! drawn in, which is the only door that also works over SSH and inside tmux.
//! The system tools are the desktop's: `wl-copy`, `xclip` and their kin, so a
//! paste into a browser works the way a user expects. A copy goes through both
//! and complains about neither — whichever door exists, the text is there.

/// The most characters of text an OSC 52 copy will carry.
///
/// A full scrollback is far more than any emulator accepts in one sequence,
/// and an over-long OSC 52 is not truncated — it is discarded whole. Trimming
/// before the base64 turns "nothing arrives" into "the last of it arrives",
/// which is what a capped copy means: keep the end, which is where the answer
/// the user just read always is.
pub const MAX_OSC52_CHARS: usize = 64 * 1024;

/// Put `text` on the terminal's clipboard with OSC 52.
///
/// Written straight to stdout rather than through the frame: a clipboard copy
/// is not a cell, and routing it through the buffer would either double it or
/// put it inside a frame's diff. One long escape sequence, flushed, and the
/// screen is untouched — the sequence carries no glyphs.
///
/// No ST terminator on purpose. The BEL form is the one every emulator that
/// supports the sequence accepts, and the ST form ends in a byte that some
/// terminals mistake for text.
pub fn osc52_copy(text: &str) -> usize {
    use std::io::Write;

    if text.is_empty() {
        return 0;
    }
    let payload = crate::base64::encode_base64(text.as_bytes());
    let mut out = std::io::stdout().lock();
    let _ = write!(out, "\x1b]52;c;{payload}\x07");
    let _ = out.flush();
    text.chars().count()
}

/// The tool this platform reads its clipboard with, tried in this order.
///
/// One per platform, first that exists wins: a machine with both `xclip` and
/// `wl-paste` (XWayland under Wayland, common) has both, and the order only
/// matters in that the first found is the one used.
const CLIPBOARD_READERS: &[&[&str]] = &[
    &["wl-paste", "--no-newline"],
    &["xclip", "-selection", "clipboard", "-o"],
    &["xsel", "--clipboard", "--output"],
    &["pbpaste"],
    #[cfg(windows)]
    &["powershell", "-NoProfile", "-Command", "Get-Clipboard"],
];

/// The tool this platform writes its clipboard with.
const CLIPBOARD_WRITERS: &[&[&str]] = &[
    &["wl-copy", "--type", "text/plain"],
    &["xclip", "-selection", "clipboard"],
    &["xsel", "--clipboard", "--input"],
    &["pbcopy"],
    #[cfg(windows)]
    &["clip"],
];

/// Read the system clipboard as text, or None when nothing can.
///
/// The terminal's own paste is a different door and arrives as a bracketed
/// paste event; this is the one a *chord* takes, and it exists so a keystroke
/// can do what a terminal's paste menu does. It is a subprocess because there
/// is no portable clipboard API without a dependency this crate does not
/// have, and the split the web client makes (`wl-paste` under
/// `navigator.clipboard.readText()`) is the one to make here.
pub fn read_system_clipboard() -> Option<String> {
    for tool in CLIPBOARD_READERS {
        let output = match std::process::Command::new(tool[0])
            .args(&tool[1..])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .output()
        {
            Ok(output) => output,
            Err(_) => continue,
        };
        if output.status.success() {
            return Some(String::from_utf8_lossy(&output.stdout).into_owned());
        }
    }
    None
}

/// Put `text` on the system clipboard, or say why not.
///
/// The same tool list the reader uses, in the other direction. A tool that
/// exists but is not usable — a `wl-copy` with no Wayland display — fails here,
/// and the next one is tried.
pub fn write_system_clipboard(text: &str) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err("nothing to copy".to_string());
    }
    for tool in CLIPBOARD_WRITERS {
        let mut child = match std::process::Command::new(tool[0])
            .args(&tool[1..])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(_) => continue,
        };
        if let Some(stdin) = child.stdin.as_mut() {
            use std::io::Write;
            if stdin.write_all(text.as_bytes()).is_ok() {
                drop(child.stdin.take());
                if child.wait().map(|s| s.success()).unwrap_or(false) {
                    return Ok(());
                }
            }
        }
    }
    Err("no clipboard tool worked".to_string())
}
