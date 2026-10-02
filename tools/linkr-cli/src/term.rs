//! Cross-platform terminal input: raw mode (POSIX termios via crossterm),
//! line mode, key decoding and the escape-byte exit rule.

use std::io::{Read, Write};

/// `true` when a controlling TTY is available for raw mode.
pub fn stdin_is_tty() -> bool {
    use std::io::IsTerminal as _;
    std::io::stdin().is_terminal()
}

/// Marker returned by [`read_hidden`] when the user presses Ctrl-C.
pub const INTERRUPTED: &str = "interrupted";

/// Guard that leaves the terminal in raw mode while it is alive. Dropping it
/// (including through unwinding) restores the previous mode, which is what the
/// Python CLI does with its `RawTerminal` context manager.
pub struct RawModeGuard {
    active: bool,
}

impl RawModeGuard {
    /// Enable raw mode when `enabled` is set; a no-op otherwise.
    pub fn enable(enabled: bool) -> anyhow::Result<Self> {
        if !enabled {
            return Ok(Self { active: false });
        }
        crossterm::terminal::enable_raw_mode().map_err(|_| {
            anyhow::anyhow!("raw terminal mode needs POSIX termios; use --line-mode instead")
        })?;
        Ok(Self { active: true })
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        if self.active {
            let _ = crossterm::terminal::disable_raw_mode();
        }
    }
}

/// Runs `body` with raw mode enabled on TTYs (no-op in line mode / no TTY).
pub fn with_raw_mode<R>(raw: bool, body: impl FnOnce() -> R) -> anyhow::Result<R> {
    let _guard = RawModeGuard::enable(raw)?;
    Ok(body())
}

/// Blocking stdin reader used by the CLI loop: raw chunks in raw mode, whole
/// lines in line mode. Returns `None` on EOF.
pub fn read_stdin_line_or_chunk(line_mode: bool) -> Option<Vec<u8>> {
    let mut stdin = std::io::stdin().lock();
    if line_mode {
        let mut buf = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            match stdin.read(&mut byte) {
                Ok(0) => break,
                Ok(_) => {
                    buf.push(byte[0]);
                    if byte[0] == b'\n' {
                        break;
                    }
                }
                Err(_) => return None,
            }
        }
        if buf.is_empty() {
            None
        } else {
            Some(buf)
        }
    } else {
        let mut buf = [0u8; 1024];
        match stdin.read(&mut buf) {
            Ok(0) | Err(_) => None,
            Ok(n) => Some(buf[..n].to_vec()),
        }
    }
}

/// Terminal size of stdin, if available.
pub fn terminal_size() -> Option<(u16, u16)> {
    crossterm::terminal::size().ok()
}

/// Prompt for a secret without echoing it back (the `getpass.getpass`
/// replacement used for `--wifi` on a TTY). Reads from stdin in raw mode, so
/// the keystrokes never reach the screen.
pub fn read_hidden(prompt: &str) -> anyhow::Result<String> {
    if !stdin_is_tty() {
        return Err(anyhow::anyhow!(INTERACTIVE_UNAVAILABLE));
    }
    let mut stderr = std::io::stderr();
    let _ = write!(stderr, "{prompt}");
    let _ = stderr.flush();
    let result = (|| -> anyhow::Result<String> {
        let _guard = RawModeGuard::enable(true)?;
        let mut stdin = std::io::stdin().lock();
        let mut out = std::io::stdout();
        let mut secret = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            match stdin.read(&mut byte) {
                Ok(0) | Err(_) => break,
                Ok(_) => match byte[0] {
                    b'\r' | b'\n' => break,
                    0x03 => return Err(anyhow::anyhow!(INTERRUPTED)),
                    0x08 | 0x7f => {
                        if secret.pop().is_some() {
                            // Rub out the echoed-free backspace: no echo, so
                            // nothing to erase on screen; keep the buffer true.
                        }
                    }
                    _ => secret.push(byte[0]),
                },
            }
        }
        let _ = out.flush();
        Ok(String::from_utf8_lossy(&secret).into_owned())
    })();
    let _ = writeln!(std::io::stderr());
    result
}

/// Message used when a hidden prompt is impossible (mirrors the Python CLI,
/// which only prompts when stdin is a TTY).
pub const INTERACTIVE_UNAVAILABLE: &str = "no interactive terminal for a password prompt";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stdin_tty_matches_std() {
        use std::io::IsTerminal;
        assert_eq!(stdin_is_tty(), std::io::stdin().is_terminal());
    }

    #[test]
    fn raw_guard_is_a_noop_when_disabled() {
        let guard = RawModeGuard::enable(false).unwrap();
        assert!(!guard.active);
        drop(guard);
    }

    #[test]
    fn with_raw_mode_runs_body() {
        let value = with_raw_mode(false, || 7).unwrap();
        assert_eq!(value, 7);
    }
}
