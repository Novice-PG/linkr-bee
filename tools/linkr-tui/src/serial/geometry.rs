//! Tell the target its terminal size, but only at an idle shell prompt.
//!
//! The UART carries a live console, so an `stty` line sent while a command owns
//! the line would be typed into that command. The web client gates on the same
//! condition (`web/terminal_geometry.js`), and the Python reference client
//! mirrors it (`tools/linkr_ble_terminal.py`); this is the third implementation
//! and it keeps the command string byte-identical across all three.
//!
//! The target's tty is a separate screen from the pane. A board that was left
//! at 57 columns by an earlier client keeps wrapping there however wide the
//! local pane is, and readline's wrap-around — a prompt written to column 57,
//! then `\r`, then the rest on the same row — lands on top of the prompt in a
//! wider emulator. Sending the pane's real size is what keeps the two widthes
//! the same. The pane never invents one: it only reports the size it was given
//! by the event loop.

/// The smallest geometry the web client will send, in columns.
pub const MIN_TERMINAL_COLUMNS: u16 = 2;
/// The smallest geometry the web client will send, in rows.
pub const MIN_TERMINAL_ROWS: u16 = 2;
/// Anything past this is a bug in a caller, not a window.
pub const MAX_TERMINAL_DIMENSION: u16 = 1000;

/// How much of the current line is kept for prompt detection. A boot banner
/// has no line terminator for a while, and the tail is the only part that can
/// still become a prompt.
const LINE_TAIL: usize = 1024;

/// A normalized terminal size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    pub cols: u16,
    pub rows: u16,
}

impl Geometry {
    /// The identity of a size, for the sync's dedupe. The string form is the
    /// same one the web client keys on, so a diagnostic reads the same on both.
    pub fn key(self) -> String {
        format!("{}x{}", self.cols, self.rows)
    }
}

/// Clamp a size the way `normalizeTerminalGeometry()` does.
pub fn normalize(cols: u16, rows: u16) -> Geometry {
    Geometry {
        cols: cols.clamp(MIN_TERMINAL_COLUMNS, MAX_TERMINAL_DIMENSION),
        rows: rows.clamp(MIN_TERMINAL_ROWS, MAX_TERMINAL_DIMENSION),
    }
}

/// The line sent to the target, byte-identical to the web and Python clients.
pub fn command(cols: u16, rows: u16) -> String {
    let geometry = normalize(cols, rows);
    format!(
        "stty rows {} cols {} >/dev/null 2>&1\r",
        geometry.rows, geometry.cols
    )
}

/// Whether the tail of the target's output is an idle shell prompt.
///
/// Port of `looksLikeShellPrompt()` from `web/terminal_geometry.js`. The web
/// reads the emulator's cursor line and additionally requires that nothing is
/// typed to its right; this reads the output stream, which is enough for the
/// TUI because the pump only asks after a burst has been parsed. A prompt is a
/// heuristic in every client: a custom `PS1` that looks like nothing here is
/// not synced rather than guessed at.
pub fn looks_like_shell_prompt(text: &str) -> bool {
    if !text.contains("$ ") && !text.contains("# ") {
        return false;
    }

    let chars: Vec<char> = text.chars().collect();
    if chars.len() < 2 {
        return false;
    }
    let prefix: String = chars[..chars.len() - 2].iter().collect();
    let prefix = prefix.trim();
    if prefix.is_empty() {
        return true;
    }

    // `@\S+(?::\S*)?$`: an address or a user@host, with an optional path.
    if let Some(at) = prefix.rfind('@') {
        let tail = &prefix[at + 1..];
        if !tail.is_empty() && !tail.chars().any(char::is_whitespace) {
            return true;
        }
    }

    // `^(?:ba|da|a|z)?sh(?:-[\d.]+)?$`.
    if is_shell_name(prefix) {
        return true;
    }

    // `^(?:~|/\S*)$`: a bare home or an absolute path.
    if prefix == "~" {
        return true;
    }
    if prefix.starts_with('/') && !prefix.chars().any(char::is_whitespace) {
        return true;
    }

    // `^\[[^\]]+\]$`: `[user@host dir]`, the root prompt several boards use.
    if prefix.starts_with('[') && prefix.ends_with(']') {
        let inner = &prefix[1..prefix.len() - 1];
        if !inner.is_empty() && !inner.contains(']') {
            return true;
        }
    }

    false
}

/// `sh`, `bash`, `dash`, `ash`, `zsh`, optionally with a `-1.2.3` version.
fn is_shell_name(prefix: &str) -> bool {
    let (name, version) = match prefix.split_once('-') {
        Some((name, version)) => (name, Some(version)),
        None => (prefix, None),
    };
    if !matches!(name, "sh" | "bash" | "dash" | "ash" | "zsh") {
        return false;
    }
    match version {
        None => true,
        Some(version) => {
            !version.is_empty() && version.chars().all(|c| c.is_ascii_digit() || c == '.')
        }
    }
}

/// Tracks the target's tty size and when it is safe to tell it.
///
/// The lockstep with the other clients is deliberate: `--line-mode`, the web
/// page and this TUI can all be looking at the same board, and a size that one
/// writes while another is typing is a command line split in half.
#[derive(Debug, Clone)]
pub struct TerminalGeometrySync {
    geometry: Geometry,
    /// The last size the target acknowledged, as a key.
    synced: String,
    /// The size a command is currently out for, as a key.
    in_flight: String,
    /// Whether the tail of the output looked like an idle prompt.
    prompt_visible: bool,
    /// The current line, for prompt detection.
    line: String,
}

impl Default for TerminalGeometrySync {
    fn default() -> TerminalGeometrySync {
        TerminalGeometrySync::new(80, 24)
    }
}

impl TerminalGeometrySync {
    pub fn new(cols: u16, rows: u16) -> TerminalGeometrySync {
        TerminalGeometrySync {
            geometry: normalize(cols, rows),
            synced: String::new(),
            in_flight: String::new(),
            prompt_visible: false,
            line: String::new(),
        }
    }

    /// The size the target will be told it has.
    pub fn geometry(&self) -> Geometry {
        self.geometry
    }

    /// Give the sync the pane's new size. It does not send anything by itself:
    /// the next idle prompt is what makes it due.
    pub fn set_size(&mut self, cols: u16, rows: u16) {
        self.geometry = normalize(cols, rows);
    }

    /// Local input invalidates the idle prompt until the target returns one.
    pub fn mark_busy(&mut self) {
        self.prompt_visible = false;
        self.line.clear();
    }

    /// Forget what an *earlier* link acknowledged: a new session is a new tty,
    /// even when the size is the same.
    pub fn reset_session(&mut self) {
        self.synced.clear();
        self.in_flight.clear();
        self.prompt_visible = false;
        self.line.clear();
    }

    /// Track the target's output to know whether an idle prompt is on screen.
    pub fn observe(&mut self, text: &str) {
        for ch in text.chars() {
            if ch == '\r' || ch == '\n' {
                self.line.clear();
                continue;
            }
            self.line.push(ch);
        }
        // Keep the tail, like the Python client: a line that never terminates
        // must not grow without limit.
        let excess = self.line.chars().count().saturating_sub(LINE_TAIL);
        if excess > 0 {
            let cut = self
                .line
                .char_indices()
                .nth(excess)
                .map(|(index, _)| index)
                .unwrap_or(self.line.len());
            self.line.drain(..cut);
        }
        self.prompt_visible = looks_like_shell_prompt(&self.line);
    }

    /// The command to send now, or `None` when nothing is due.
    ///
    /// A command already in flight hides the prompt so a second prompt in the
    /// same burst cannot queue a duplicate, and a size already synced is not
    /// sent again just because the shell reprinted its prompt.
    pub fn take_pending_command(&mut self) -> Option<String> {
        let key = self.geometry.key();
        if !self.prompt_visible || key == self.synced || key == self.in_flight {
            return None;
        }
        self.in_flight = key;
        self.prompt_visible = false;
        Some(command(self.geometry.cols, self.geometry.rows))
    }

    /// The command went out: do not send it again at this size.
    pub fn confirm_sent(&mut self) {
        self.synced = std::mem::take(&mut self.in_flight);
    }

    /// The command did not go out: a later prompt may try again.
    pub fn abort_sent(&mut self) {
        self.in_flight.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The same cases `tools/linkr_ble_terminal.py` is tested with, so the two
    /// clients cannot drift apart without a test saying so.
    #[test]
    fn a_prompt_is_recognised_in_its_common_forms() {
        for text in [
            "root@target:~$ ",
            "root@target:/tmp# ",
            "[root@target ~]# ",
            "~ # ",
            "/ # ",
            "$ ",
            "# ",
            "bash-5.2$ ",
            "sh-5.2# ",
            "deckpi@kickpi-k2b-v2:~$ ",
        ] {
            assert!(looks_like_shell_prompt(text), "{text:?}");
        }
        for text in ["", "$ x", "running", "Loading kernel...", "make[1]: done"] {
            assert!(!looks_like_shell_prompt(text), "{text:?}");
        }
    }

    #[test]
    fn the_command_is_the_web_clients_command() {
        assert_eq!(command(80, 24), "stty rows 24 cols 80 >/dev/null 2>&1\r");
        assert_eq!(
            super::super::geometry::command(117, 40),
            "stty rows 40 cols 117 >/dev/null 2>&1\r"
        );
    }

    #[test]
    fn geometry_is_clamped_like_the_web_client() {
        assert_eq!(normalize(0, 0), Geometry { cols: 2, rows: 2 });
        assert_eq!(
            normalize(5000, 5000),
            Geometry {
                cols: MAX_TERMINAL_DIMENSION,
                rows: MAX_TERMINAL_DIMENSION
            }
        );
        assert_eq!(normalize(117, 40).key(), "117x40");
    }

    #[test]
    fn sync_waits_for_an_idle_prompt_and_dedupes() {
        let mut sync = TerminalGeometrySync::new(80, 24);
        sync.observe("booting the target\r\n");
        assert_eq!(sync.take_pending_command(), None);

        sync.observe("root@target:~$ ");
        assert_eq!(
            sync.take_pending_command().as_deref(),
            Some("stty rows 24 cols 80 >/dev/null 2>&1\r")
        );
        // In flight: a second prompt must not queue a duplicate.
        assert_eq!(sync.take_pending_command(), None);
        sync.confirm_sent();
        sync.observe("root@target:~$ ");
        assert_eq!(sync.take_pending_command(), None);
    }

    #[test]
    fn a_resize_is_sent_at_the_next_prompt() {
        let mut sync = TerminalGeometrySync::new(80, 24);
        sync.observe("root@target:~$ ");
        sync.take_pending_command();
        sync.confirm_sent();

        sync.set_size(120, 40);
        assert_eq!(sync.take_pending_command(), None);
        sync.observe("root@target:~$ ");
        assert_eq!(
            sync.take_pending_command().as_deref(),
            Some("stty rows 40 cols 120 >/dev/null 2>&1\r")
        );
    }

    #[test]
    fn local_input_hides_the_prompt_until_the_target_prints_another() {
        let mut sync = TerminalGeometrySync::new(80, 24);
        sync.observe("root@target:~$ ");
        sync.mark_busy();
        assert_eq!(sync.take_pending_command(), None);

        // The echo of what was typed is not a prompt either.
        sync.observe("root@target:~$ ls\r\n");
        assert_eq!(sync.take_pending_command(), None);
        sync.observe("root@target:~$ ");
        assert!(sync.take_pending_command().is_some());
    }

    #[test]
    fn a_failed_send_is_retried_at_the_next_prompt() {
        let mut sync = TerminalGeometrySync::new(80, 24);
        sync.observe("root@target:~$ ");
        sync.take_pending_command();
        sync.abort_sent();
        sync.observe("root@target:~$ ");
        assert!(sync.take_pending_command().is_some());
    }

    #[test]
    fn a_new_session_forgets_the_last_ones_acknowledgement() {
        let mut sync = TerminalGeometrySync::new(80, 24);
        sync.observe("root@target:~$ ");
        sync.take_pending_command();
        sync.confirm_sent();

        sync.reset_session();
        sync.observe("root@target:~$ ");
        assert!(sync.take_pending_command().is_some());
    }

    #[test]
    fn a_prompt_split_across_reads_is_still_recognised() {
        let mut sync = TerminalGeometrySync::new(80, 24);
        sync.observe("root@tar");
        assert_eq!(sync.take_pending_command(), None);
        sync.observe("get:~$ ");
        assert!(sync.take_pending_command().is_some());
    }

    #[test]
    fn a_line_that_never_ends_keeps_only_its_tail() {
        let mut sync = TerminalGeometrySync::new(80, 24);
        sync.observe(&"x".repeat(LINE_TAIL * 2));
        assert_eq!(sync.line.chars().count(), LINE_TAIL);
        assert!(sync.line.chars().all(|c| c == 'x'));
    }
}
