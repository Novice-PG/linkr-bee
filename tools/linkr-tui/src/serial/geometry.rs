//! Tell the target its terminal size, but only at an idle shell prompt.
//!
//! The UART carries a live console, so an `stty` line sent while a command owns
//! the line would be typed into that command. The web client gates on the same
//! condition (`web/terminal_geometry.js`), and the Python reference client
//! mirrors it (`tools/linkr_ble_terminal.py`). The command is byte-identical
//! across all three clients; the TUI gate uses the existing VT parser's screen.
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
/// typed to its right; observe_screen applies the same conservative checks.
/// A custom prompt that looks like nothing here is not synced rather than
/// guessed at.
pub fn looks_like_shell_prompt(text: &str) -> bool {
    if !text.ends_with("$ ") && !text.ends_with("# ") {
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
    input_pending: bool,
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
            input_pending: false,
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
        self.input_pending = true;
    }

    /// Editing remains uncertain until Enter or Ctrl-C ends the input line.
    pub fn note_input(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.prompt_visible = false;
        for byte in bytes {
            self.input_pending = !matches!(byte, b'\r' | b'\n' | 3);
        }
    }

    /// Forget what an *earlier* link acknowledged: a new session is a new tty,
    /// even when the size is the same.
    pub fn reset_session(&mut self) {
        self.synced.clear();
        self.in_flight.clear();
        self.prompt_visible = false;
        self.input_pending = false;
    }

    /// Only the parsed screen can establish whether readline has editable
    /// text to the right of its cursor or a full-screen program owns the tty.
    pub fn observe_screen(&mut self, terminal: &crate::vt::Vt) {
        self.prompt_visible = false;
        if terminal.on_alt_screen() {
            return;
        }
        let screen = terminal.screen();
        let (row, col) = screen.cursor();
        let cells = screen.row(row);
        if col > cells.len()
            || cells[col..].iter().any(|cell| !cell.is_blank())
            || (row + 1..screen.rows())
                .any(|next| screen.row(next).iter().any(|cell| !cell.is_blank()))
        {
            return;
        }
        let text: String = cells[..col]
            .iter()
            .filter(|cell| !cell.spacer)
            .map(|cell| cell.glyph())
            .collect();
        self.prompt_visible = looks_like_shell_prompt(&text);
    }

    /// The command to send now, or `None` when nothing is due.
    ///
    /// A command already in flight hides the prompt so a second prompt in the
    /// same burst cannot queue a duplicate, and a size already synced is not
    /// sent again just because the shell reprinted its prompt.
    pub fn take_pending_command(&mut self) -> Option<String> {
        let key = self.geometry.key();
        if self.input_pending || !self.prompt_visible || key == self.synced || key == self.in_flight
        {
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
    use crate::views::terminal::TerminalPane;

    fn observe(sync: &mut TerminalGeometrySync, pane: &mut TerminalPane, text: &str) {
        pane.push_text(text);
        sync.observe_screen(pane.vt());
    }

    #[test]
    fn only_a_prompt_at_the_end_of_the_line_is_idle() {
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
        ] {
            assert!(looks_like_shell_prompt(text), "{text:?}");
        }
        for text in [
            "",
            "$ x",
            "root@target:~$ ls",
            "root@target:~$ x",
            "root@target:~$ echo hello",
            "running",
            "Loading kernel...",
        ] {
            assert!(!looks_like_shell_prompt(text), "{text:?}");
        }
    }

    #[test]
    fn command_and_bounds_match_the_web_client() {
        assert_eq!(command(117, 40), "stty rows 40 cols 117 >/dev/null 2>&1\r");
        assert_eq!(normalize(0, 0), Geometry { cols: 2, rows: 2 });
        assert_eq!(
            normalize(5000, 5000),
            Geometry {
                cols: 1000,
                rows: 1000
            }
        );
        assert_eq!(normalize(117, 40).key(), "117x40");
    }

    #[test]
    fn sync_waits_for_an_idle_prompt_and_dedupes() {
        let mut sync = TerminalGeometrySync::default();
        let mut pane = TerminalPane::new();
        observe(&mut sync, &mut pane, "booting\r\n");
        assert_eq!(sync.take_pending_command(), None);
        observe(&mut sync, &mut pane, "root@target:~$ ");
        assert!(sync.take_pending_command().is_some());
        assert_eq!(sync.take_pending_command(), None);
        sync.confirm_sent();
        observe(&mut sync, &mut pane, "\r\nroot@target:~$ ");
        assert_eq!(sync.take_pending_command(), None);
        sync.set_size(117, 40);
        observe(&mut sync, &mut pane, "\r\nroot@target:~$ ");
        assert_eq!(sync.take_pending_command(), Some(command(117, 40)));
    }

    #[test]
    fn typed_input_stays_busy_despite_prompt_redraws_and_status_requests() {
        let mut sync = TerminalGeometrySync::default();
        let mut pane = TerminalPane::new();
        observe(&mut sync, &mut pane, "root@target:~$ ");
        sync.note_input(b"ls");
        observe(&mut sync, &mut pane, "\x1b[6n");
        assert_eq!(sync.take_pending_command(), None);
        observe(&mut sync, &mut pane, "\rroot@target:~$ ls");
        assert_eq!(sync.take_pending_command(), None);
        sync.note_input(b"\r");
        observe(&mut sync, &mut pane, "\r\nroot@target:~$ ");
        assert!(sync.take_pending_command().is_some());
    }

    #[test]
    fn cursor_at_prompt_with_editable_text_to_its_right_is_not_idle() {
        let mut sync = TerminalGeometrySync::default();
        let mut pane = TerminalPane::new();
        observe(&mut sync, &mut pane, "root@target:~$ ls\x1b[2D");
        assert_eq!(sync.take_pending_command(), None);
    }

    #[test]
    fn possible_wrapped_input_below_the_cursor_is_not_idle() {
        let mut sync = TerminalGeometrySync::default();
        let mut pane = TerminalPane::new();
        observe(&mut sync, &mut pane, "root@target:~$ \r\nls\x1b[1A\x1b[15G");
        assert_eq!(sync.take_pending_command(), None);
    }

    #[test]
    fn alternate_screen_prompts_do_not_trigger_shell_commands() {
        let mut sync = TerminalGeometrySync::default();
        let mut pane = TerminalPane::new();
        observe(&mut sync, &mut pane, "\x1b[?1049hroot@target:~$ ");
        assert_eq!(sync.take_pending_command(), None);
    }

    #[test]
    fn coloured_prompt_split_across_reads_uses_the_parsed_screen() {
        let mut sync = TerminalGeometrySync::default();
        let mut pane = TerminalPane::new();
        observe(&mut sync, &mut pane, "\x1b[3");
        observe(&mut sync, &mut pane, "2mroot@tar");
        assert_eq!(sync.take_pending_command(), None);
        observe(&mut sync, &mut pane, "get:~$ \x1b[0m");
        assert!(sync.take_pending_command().is_some());
    }

    #[test]
    fn a_held_line_waits_until_submission_or_cancellation() {
        let mut sync = TerminalGeometrySync::default();
        let mut pane = TerminalPane::new();
        sync.mark_busy();
        observe(&mut sync, &mut pane, "root@target:~$ ");
        assert_eq!(sync.take_pending_command(), None);
        sync.note_input(b"\x03");
        observe(&mut sync, &mut pane, "\r\nroot@target:~$ ");
        assert!(sync.take_pending_command().is_some());
    }

    #[test]
    fn failed_sends_and_new_sessions_can_retry() {
        let mut sync = TerminalGeometrySync::default();
        let mut pane = TerminalPane::new();
        observe(&mut sync, &mut pane, "root@target:~$ ");
        sync.take_pending_command();
        sync.abort_sent();
        observe(&mut sync, &mut pane, "\r\nroot@target:~$ ");
        assert!(sync.take_pending_command().is_some());
        sync.confirm_sent();
        sync.reset_session();
        observe(&mut sync, &mut pane, "\r\nroot@target:~$ ");
        assert!(sync.take_pending_command().is_some());
    }
}
