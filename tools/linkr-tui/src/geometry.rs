//! Telling the target how big the terminal is.
//!
//! The UART carries a live console, and a shell that is never told its window
//! size falls back to the kernel default — which is how a 255-column window
//! ends up with the target wrapping its own output at 80. The web client fixes
//! that by typing `stty rows … cols …` into the target once a shell prompt is
//! idle on the line; this is the same fix for the terminal, kept byte for byte
//! compatible with `web/terminal_geometry.js` and the CLI's
//! `TerminalGeometrySync`, because two clients telling one board two different
//! things is one board with two opinions about its width.
//!
//! The gate matters as much as the command: the line would be typed into
//! whatever owns the console at the time, so the command only goes out when the
//! last thing the target said was an idle shell prompt — never while a command
//! is running, and never while the user has typed something of their own that
//! the shell has not answered yet.

use std::fmt;

/// The smallest size a terminal can be told it is.
pub const MIN_DIMENSION: u16 = 2;
/// The largest size a terminal can be told it is. A window cannot be this big
/// honestly, and a number this far out is a garbled measurement rather than a
/// window.
pub const MAX_DIMENSION: u16 = 1000;

/// How much of the target's current line is remembered. A prompt is short; a
/// kilobyte is enough for one wrapped boot line and keeps a target that never
/// ends a line from growing a buffer without end.
const LINE_LIMIT: usize = 1024;

/// A terminal size, clamped the way the web client clamps one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    /// Columns, within [`MIN_DIMENSION`]..=[`MAX_DIMENSION`].
    pub cols: u16,
    /// Rows, within [`MIN_DIMENSION`]..=[`MAX_DIMENSION`].
    pub rows: u16,
}

impl Geometry {
    /// A size, clamped: a zero or a nonsense measurement is the smallest
    /// terminal rather than an instruction to build a zero-column one.
    pub fn new(cols: u16, rows: u16) -> Geometry {
        Geometry {
            cols: clamp(cols, MIN_DIMENSION),
            rows: clamp(rows, MIN_DIMENSION),
        }
    }

    /// What identifies this size: [`Self::synced`] compares these, so the
    /// target is told again when the number changed and not when it did not.
    pub fn key(&self) -> String {
        format!("{}x{}", self.cols, self.rows)
    }

    /// The line the target is sent. Byte for byte the web client's, down to
    /// the redirect and the carriage return.
    pub fn command(&self) -> String {
        format!(
            "stty rows {} cols {} >/dev/null 2>&1\r",
            self.rows, self.cols
        )
    }
}

impl fmt::Display for Geometry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}x{}", self.cols, self.rows)
    }
}

fn clamp(value: u16, minimum: u16) -> u16 {
    value.clamp(minimum, MAX_DIMENSION)
}

/// Whether a line the target printed is an idle shell prompt.
///
/// A port of `looksLikeShellPrompt` from `web/terminal_geometry.js`: the line
/// has to end in `$ ` or `# ` and what stands before it has to look like a
/// prompt rather than like prose that happens to end in one. The rules are the
/// web client's own, because a target that talks back to both clients must not
/// hear two different answers about its own readiness.
pub fn looks_like_shell_prompt(line: &str) -> bool {
    let bytes = line.as_bytes();
    if bytes.len() < 2 || bytes[bytes.len() - 1] != b' ' {
        return false;
    }
    if !matches!(bytes[bytes.len() - 2], b'$' | b'#') {
        return false;
    }
    let prefix = line[..line.len() - 2].trim();
    if prefix.is_empty() {
        return true;
    }
    ends_with_user_host(prefix)
        || is_a_shell_name(prefix)
        || prefix == "~"
        || (prefix.starts_with('/') && !prefix.chars().any(char::is_whitespace))
        || (prefix.starts_with('[') && prefix.ends_with(']') && !prefix[1..].contains(']'))
}

/// `…@host` or `…@host:path`, with no whitespace after the `@`.
fn ends_with_user_host(prefix: &str) -> bool {
    match prefix.rfind('@') {
        Some(at) => {
            let tail = &prefix[at + 1..];
            !tail.is_empty() && !tail.chars().any(char::is_whitespace)
        }
        None => false,
    }
}

/// `sh`, `bash`, `dash`, `ash`, `zsh`, optionally with a version suffix —
/// `bash-5.1$` is a root shell in a container as much as `root@box$` is.
fn is_a_shell_name(prefix: &str) -> bool {
    let (name, suffix) = match prefix.split_once('-') {
        Some((name, suffix)) => (name, Some(suffix)),
        None => (prefix, None),
    };
    if !matches!(name, "sh" | "bash" | "dash" | "ash" | "zsh") {
        return false;
    }
    match suffix {
        None => true,
        Some(suffix) => {
            !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit() || c == '.')
        }
    }
}

/// The size the target has been told, and whether it may be told again.
///
/// One of these per link, not per frame: what it knows is a fact about the
/// conversation — which prompt is on the line, which size the shell was last
/// given — and a fresh one every frame would forget both and type `stty` after
/// every keystroke.
#[derive(Debug, Clone)]
pub struct GeometrySync {
    geometry: Geometry,
    /// Whether a pane has ever said what size it is.
    ///
    /// Nothing is typed before one has: the one-shot and resident modes have no
    /// pane, and a size command of theirs would be a line on the target's
    /// console nobody asked for — the `--exec` command's own wrapper is the
    /// thing that should be going out at that first prompt, not an `stty`.
    armed: bool,
    /// The size the target confirmed it was sent, as [`Geometry::key`].
    synced: String,
    /// The size whose command is on the wire but not yet confirmed gone.
    in_flight: String,
    /// Whether the last thing the target said was an idle prompt.
    prompt_visible: bool,
    /// The target's current line, so a prompt split across two reads is still
    /// seen as one.
    line: String,
}

impl Default for GeometrySync {
    /// The size-less state: a placeholder size and nothing owed to the target,
    /// which is what a mode without a pane runs in. [`Self::set_size`] arms it.
    fn default() -> GeometrySync {
        GeometrySync {
            armed: false,
            ..GeometrySync::new(80, 24)
        }
    }
}

impl GeometrySync {
    /// A sync for a caller that already has a size in hand — the pane's first
    /// frame, and the tests that stand in for one.
    pub fn new(cols: u16, rows: u16) -> GeometrySync {
        GeometrySync {
            geometry: Geometry::new(cols, rows),
            armed: true,
            synced: String::new(),
            in_flight: String::new(),
            prompt_visible: false,
            line: String::new(),
        }
    }

    /// The pane was resized. A size that changed is a size the target is owed,
    /// and a pane that has said a size once has said it is there to be synced.
    pub fn set_size(&mut self, cols: u16, rows: u16) {
        self.geometry = Geometry::new(cols, rows);
        self.armed = true;
    }

    /// The size the target has been told, for a caller that wants to show it.
    pub fn geometry(&self) -> Geometry {
        self.geometry
    }

    /// Local input went to the target: whatever was on the line is no longer
    /// an idle prompt, and nothing may be typed on top of it.
    pub fn mark_busy(&mut self) {
        self.prompt_visible = false;
        self.line.clear();
    }

    /// The target said something. Only the end of the last line matters, and a
    /// line that has grown past [`LINE_LIMIT`] keeps its tail rather than
    /// growing without bound.
    pub fn observe(&mut self, text: &str) {
        for ch in text.chars() {
            if ch == '\r' || ch == '\n' {
                self.line.clear();
                continue;
            }
            self.line.push(ch);
            if self.line.len() > LINE_LIMIT {
                let cut = self.line.len() - LINE_LIMIT;
                self.line.drain(..cut);
            }
        }
        self.prompt_visible = looks_like_shell_prompt(&self.line);
    }

    /// The line to type now, or `None` when nothing is due.
    ///
    /// Due means: a pane has said what size it is, an idle prompt is on the
    /// line, and this size is neither one the target already confirmed nor one
    /// already on the wire. Taking the command clears the prompt, because
    /// typing it is exactly what makes the line busy again.
    pub fn take_pending_command(&mut self) -> Option<String> {
        let key = self.geometry.key();
        if !self.armed || !self.prompt_visible || key == self.synced || key == self.in_flight {
            return None;
        }
        self.in_flight = key;
        self.prompt_visible = false;
        Some(self.geometry.command())
    }

    /// The command reached the wire. From here this size is the synced one:
    /// the shell has it, and a second copy would only scroll the line.
    pub fn confirm_sent(&mut self) {
        self.synced = std::mem::take(&mut self.in_flight);
    }

    /// The command never went out — no link, or a link that refused it. The
    /// size stays owed, so the next idle prompt gets it.
    pub fn abort_sent(&mut self) {
        self.in_flight.clear();
    }

    /// A link just came up, or went down: nothing of the old conversation is
    /// true of the new one, so the target is told its size again from scratch.
    pub fn reset(&mut self) {
        self.synced.clear();
        self.in_flight.clear();
        self.prompt_visible = false;
        self.line.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_size_is_clamped_like_the_web_clients() {
        assert_eq!(Geometry::new(0, 0), Geometry::new(2, 2));
        assert_eq!(Geometry::new(1, 5), Geometry::new(2, 5));
        assert_eq!(Geometry::new(5000, 4000), Geometry::new(1000, 1000));
        assert_eq!(Geometry::new(255, 30).key(), "255x30");
    }

    #[test]
    fn the_command_is_the_web_clients_byte_for_byte() {
        let line = Geometry::new(255, 30).command();
        assert_eq!(line, "stty rows 30 cols 255 >/dev/null 2>&1\r");
        assert!(line.ends_with('\r') && !line.ends_with("\r\n"));
    }

    #[test]
    fn a_bare_prompt_is_a_prompt() {
        assert!(looks_like_shell_prompt("$ "));
        assert!(looks_like_shell_prompt("# "));
        assert!(looks_like_shell_prompt("root@linkr:~# "));
        assert!(looks_like_shell_prompt("rock@box:/tmp$ "));
        assert!(looks_like_shell_prompt("bash-5.1$ "));
        assert!(looks_like_shell_prompt("/home/rock$ "));
        assert!(looks_like_shell_prompt("~$ "));
        assert!(looks_like_shell_prompt("[root@linkr] $ "));
    }

    #[test]
    fn prose_is_not_a_prompt() {
        assert!(!looks_like_shell_prompt("total 12 files $ "));
        assert!(!looks_like_shell_prompt("it cost $ "));
        assert!(!looks_like_shell_prompt("a prompt $ "));
        assert!(!looks_like_shell_prompt("$"));
        assert!(!looks_like_shell_prompt("$ x"));
        assert!(!looks_like_shell_prompt("[  0.1] Linux version 6$ "));
    }

    #[test]
    fn nothing_goes_out_until_a_prompt_is_idle() {
        let mut sync = GeometrySync::new(120, 30);
        assert_eq!(sync.take_pending_command(), None);
        // Output with no prompt yet: a command still running.
        sync.observe("Reading package lists...\r\n");
        assert_eq!(sync.take_pending_command(), None);
        // The prompt arrives, and with it the one line that is owed.
        sync.observe("root@linkr:~# ");
        assert_eq!(
            sync.take_pending_command().as_deref(),
            Some("stty rows 30 cols 120 >/dev/null 2>&1\r")
        );
        // Taken once, not once per frame.
        assert_eq!(sync.take_pending_command(), None);
    }

    #[test]
    fn nothing_is_typed_at_a_target_no_pane_is_watching() {
        // The one-shot and resident modes have no pane, so no size was ever
        // given: an `stty` line of theirs would be a write the caller never
        // asked for, landing right where its own command is about to go.
        let mut sync = GeometrySync::default();
        sync.observe("root@linkr:~# ");
        assert_eq!(
            sync.take_pending_command(),
            None,
            "no pane, no size, no write"
        );
        // A pane that gives one arms the sync — the interface's own path.
        sync.set_size(120, 30);
        assert_eq!(
            sync.take_pending_command().as_deref(),
            Some("stty rows 30 cols 120 >/dev/null 2>&1\r")
        );
    }

    #[test]
    fn a_confirmed_size_is_not_typed_again() {
        let mut sync = GeometrySync::new(100, 40);
        sync.observe("# ");
        let line = sync
            .take_pending_command()
            .expect("the first prompt is due");
        sync.confirm_sent();
        assert!(line.contains("cols 100"));
        // The prompt is reprinted for every command the user runs; the size is
        // not.
        sync.observe("uname -a\r\nLinux linkr 6.1\r\nroot@linkr:~# ");
        assert_eq!(sync.take_pending_command(), None);
    }

    #[test]
    fn a_resize_is_told_again_on_the_next_prompt() {
        let mut sync = GeometrySync::new(80, 24);
        sync.observe("$ ");
        assert!(sync.take_pending_command().is_some());
        sync.confirm_sent();
        sync.set_size(255, 30);
        assert_eq!(sync.take_pending_command(), None, "the line is busy");
        sync.observe("\r\n$ ");
        assert_eq!(
            sync.take_pending_command().as_deref(),
            Some("stty rows 30 cols 255 >/dev/null 2>&1\r")
        );
    }

    #[test]
    fn local_input_makes_the_line_busy_until_the_target_answers() {
        let mut sync = GeometrySync::new(80, 24);
        sync.observe("$ ");
        sync.mark_busy();
        assert_eq!(sync.take_pending_command(), None);
        // The shell echoes the command, runs it, then prompts again.
        sync.observe("uname\r\n");
        assert_eq!(sync.take_pending_command(), None);
        sync.observe("Linux\r\n$ ");
        assert!(sync.take_pending_command().is_some());
    }

    #[test]
    fn a_prompt_split_across_reads_is_still_one_prompt() {
        let mut sync = GeometrySync::new(80, 24);
        sync.observe("root@li");
        assert_eq!(sync.take_pending_command(), None);
        sync.observe("nkr:~# ");
        assert!(sync.take_pending_command().is_some());
    }

    #[test]
    fn a_bracketed_host_is_a_prompt_and_prose_in_brackets_is_not() {
        // The web client's rule, exactly: a bracketed prefix counts only when
        // the bracket closes the prefix. `[root@host ~]#` is not one, as the
        // reference reads it, so it is not one here either.
        assert!(looks_like_shell_prompt("[root@linkr] $ "));
        assert!(!looks_like_shell_prompt("[root@host ~]# "));
        assert!(!looks_like_shell_prompt("[  0.1] done $ "));
    }

    #[test]
    fn a_carriage_return_ends_a_line_like_a_newline() {
        let mut sync = GeometrySync::new(80, 24);
        // A line overwritten by `\r` is gone; only what stands after the last
        // one is on the screen.
        sync.observe("$ not a prompt\r# ");
        assert!(sync.take_pending_command().is_some());
    }

    #[test]
    fn a_link_that_comes_up_gets_told_from_scratch() {
        let mut sync = GeometrySync::new(90, 25);
        sync.observe("$ ");
        sync.take_pending_command();
        sync.confirm_sent();
        sync.reset();
        sync.observe("login: ");
        assert_eq!(sync.take_pending_command(), None);
        sync.observe("linkr\r\nroot@linkr:~# ");
        assert_eq!(
            sync.take_pending_command().as_deref(),
            Some("stty rows 25 cols 90 >/dev/null 2>&1\r")
        );
    }

    #[test]
    fn a_command_that_never_left_is_owed_still() {
        let mut sync = GeometrySync::new(80, 24);
        sync.observe("$ ");
        assert!(sync.take_pending_command().is_some());
        sync.abort_sent();
        // The prompt was eaten by the take, so the target's next prompt is what
        // offers it again.
        sync.observe("\r\n$ ");
        assert!(sync.take_pending_command().is_some());
    }
}
