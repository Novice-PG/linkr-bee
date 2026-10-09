//! Terminal geometry sync: port of `TerminalGeometrySync` and friends.
//! The emitted command must stay byte-identical to web/terminal_geometry.js:
//! `stty rows {rows} cols {cols} >/dev/null 2>&1\r`.
//!
//! Two gates keep the command off a line the console is busy with, both lifted
//! from the web client (`scheduleTerminalGeometrySync` in web/app.js), which
//! WEB_UX_SPEC 3.7 states as: "180 ms debounce, only with an idle shell prompt
//! visible". Without them the `stty` lands between two keystrokes of a command
//! the user is still typing — the shell executes the fragments, and the
//! remainder runs as a command of its own.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use regex::Regex;

pub const MIN_DIMENSION: u32 = 2;
pub const MAX_DIMENSION: u32 = 1000;
pub const GEOMETRY_LINE_BUFFER: usize = 1024;
/// WEB_UX_SPEC 3.7: the sync waits this long after an idle prompt before it
/// writes, and re-checks the prompt, the input line and the geometry when the
/// timer fires (web: `setTimeout(…, 180)`).
pub const GEOMETRY_DEBOUNCE_MS: u64 = 180;

/// Clamp rows/cols the way the web client does
/// (`clampDimension`: non-finite falls back to the minimum, then the value is
/// truncated and clamped to 2..=1000).
pub fn terminal_geometry(cols: u32, rows: u32) -> (u32, u32, String) {
    let clamp = |value: u32, minimum: u32| value.min(MAX_DIMENSION).max(minimum);
    let cols = clamp(cols, MIN_DIMENSION);
    let rows = clamp(rows, MIN_DIMENSION);
    (cols, rows, format!("{cols}x{rows}"))
}

/// The exact wire command for a clamped geometry.
pub fn terminal_geometry_command(cols: u32, rows: u32) -> String {
    let (cols, rows, _) = terminal_geometry(cols, rows);
    format!("stty rows {rows} cols {cols} >/dev/null 2>&1\r")
}

fn re(pattern: &'static str, cache: &'static OnceLock<Regex>) -> &'static Regex {
    cache.get_or_init(|| Regex::new(pattern).expect("valid prompt regex"))
}

/// Mirror of web `looksLikeShellPrompt`.
pub fn looks_like_shell_prompt(text: &str) -> bool {
    static ENDS: OnceLock<Regex> = OnceLock::new();
    static AT: OnceLock<Regex> = OnceLock::new();
    static SH: OnceLock<Regex> = OnceLock::new();
    static PATH: OnceLock<Regex> = OnceLock::new();
    static BRACKET: OnceLock<Regex> = OnceLock::new();

    if !re(r"[$#] $", &ENDS).is_match(text) {
        return false;
    }
    let prefix = text[..text.len() - 2].trim();
    if prefix.is_empty() {
        return true;
    }
    re(r"@\S+(?::\S*)?$", &AT).is_match(prefix)
        || re(r"^(?:ba|da|a|z)?sh(?:-[\d.]+)?$", &SH).is_match(prefix)
        || re(r"^(?:~|/\S*)$", &PATH).is_match(prefix)
        || re(r"^\[[^\]]+\]$", &BRACKET).is_match(prefix)
}

/// The last local send closed its line, i.e. it carried a terminator.
///
/// Mirror of web `inputLeavesPendingLine` (web/agent_execution_policy.js):
/// every byte overwrites the flag, so only a CR, LF or `^C` that arrives *last*
/// closes the line — backspace, cursor movement and plain text all leave the
/// line's contents unknown, which is what holds the sync off a half-typed
/// command.
fn input_leaves_pending_line(bytes: &[u8], mut pending: bool) -> bool {
    for byte in bytes {
        pending = !matches!(*byte, b'\n' | b'\r' | 0x03);
    }
    pending
}

/// Tell the target its terminal size, but only at an idle shell prompt.
///
/// The UART carries a live console, so an stty line sent while a command owns
/// the line would be typed into that command. The web client gates on the
/// same condition; this mirrors it for the CLI.
pub struct TerminalGeometrySync {
    cols: u32,
    rows: u32,
    key: String,
    synced: String,
    in_flight: String,
    prompt_visible: bool,
    /// The user's input line is still open: the last tracked local send had no
    /// CR/LF/^C (web `serialInputPending`). A prompt observed while this is set
    /// — the journal replay's trailing `…~$ ` behind a few keystrokes, say —
    /// must not re-arm the sync.
    input_pending: bool,
    /// When the prompt was last seen idle; the command is due once this is
    /// [`GEOMETRY_DEBOUNCE_MS`] old.
    armed_at: Option<Instant>,
    line: String,
}

impl TerminalGeometrySync {
    pub fn new(cols: u32, rows: u32) -> Self {
        let (cols, rows, key) = terminal_geometry(cols, rows);
        Self {
            cols,
            rows,
            key,
            synced: String::new(),
            in_flight: String::new(),
            prompt_visible: false,
            input_pending: false,
            armed_at: None,
            line: String::new(),
        }
    }

    pub fn set_size(&mut self, cols: u32, rows: u32) {
        let (cols, rows, key) = terminal_geometry(cols, rows);
        self.cols = cols;
        self.rows = rows;
        self.key = key;
    }

    /// The current clamped geometry as `COLSxROWS`.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Called on every local send: the prompt is no longer idle
    /// (PYTHON_CLI_SPEC 7: `mark_busy(): prompt_visible = false; line = ""`).
    pub fn mark_busy(&mut self) {
        self.prompt_visible = false;
        self.line.clear();
        self.armed_at = None;
    }

    /// Terminal input: latch the line open as well (web `enqueueBytes(…,
    /// {trackPending: true})`), so nothing goes out until the shell has taken
    /// the line and printed a fresh prompt.
    pub fn mark_input(&mut self, bytes: &[u8]) {
        self.input_pending = input_leaves_pending_line(bytes, self.input_pending);
        self.mark_busy();
    }

    /// A send that failed mid-line leaves the line's contents unknown: web
    /// latches `serialInputPending` in `enqueueBytes`'s catch for the same
    /// reason, so the sync waits for a terminator instead of guessing.
    pub fn latch_input(&mut self) {
        self.input_pending = true;
    }

    /// Called with every received payload (UTF-8 lossy text).
    pub fn observe(&mut self, text: &str) {
        for ch in text.chars() {
            if ch == '\r' || ch == '\n' {
                self.line.clear();
                continue;
            }
            self.line.push(ch);
            if self.line.chars().count() > GEOMETRY_LINE_BUFFER {
                let first = self.line.chars().next().map(char::len_utf8).unwrap_or(0);
                self.line.drain(..first);
            }
        }
        let visible = looks_like_shell_prompt(&self.line);
        self.prompt_visible = visible;
        // Every idle prompt (re)starts the debounce: web re-arms its 180 ms
        // timer on each parse, so a prompt that is immediately superseded by
        // output never reaches the wire.
        self.armed_at = visible.then(Instant::now);
    }

    /// Whether a command is queued but not yet due.
    fn armed(&self) -> bool {
        self.prompt_visible
            && !self.input_pending
            && self.key != self.synced
            && self.key != self.in_flight
    }

    /// When [`Self::take_pending_command`] first becomes due, so the session
    /// can arm a timer: an idle console sends no further RX, and without a
    /// timer the first size push would wait for bytes that never come.
    pub fn deadline(&self) -> Option<Instant> {
        if !self.armed() {
            return None;
        }
        Some(self.armed_at? + Duration::from_millis(GEOMETRY_DEBOUNCE_MS))
    }

    /// Returns the stty command when a new, idle, unsynced geometry is due.
    pub fn take_pending_command(&mut self) -> Option<String> {
        if !self.armed() {
            return None;
        }
        if self.armed_at?.elapsed() < Duration::from_millis(GEOMETRY_DEBOUNCE_MS) {
            return None;
        }
        self.in_flight = self.key.clone();
        self.prompt_visible = false;
        self.armed_at = None;
        Some(terminal_geometry_command(self.cols, self.rows))
    }

    pub fn confirm_sent(&mut self) {
        self.synced = std::mem::take(&mut self.in_flight);
    }

    pub fn abort_sent(&mut self) {
        self.in_flight.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The debounce runs on wall time (web `setTimeout(…, 180)`), so a test
    /// that wants the command has to let it elapse first.
    fn due(sync: &mut TerminalGeometrySync) -> Option<String> {
        std::thread::sleep(Duration::from_millis(GEOMETRY_DEBOUNCE_MS + 10));
        sync.take_pending_command()
    }

    // ---- TerminalGeometryTests ------------------------------------------

    #[test]
    fn geometry_is_clamped_like_the_web_client() {
        assert_eq!(terminal_geometry(0, 0).2, "2x2");
        assert_eq!(terminal_geometry(5000, 5000).2, "1000x1000");
        assert_eq!(terminal_geometry(120, 30).2, "120x30");
        assert_eq!(terminal_geometry(1, 1001).0, 2);
        assert_eq!(terminal_geometry(1, 1001).1, 1000);
    }

    fn web_source() -> String {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/terminal_geometry.js");
        std::fs::read_to_string(path).expect("read web/terminal_geometry.js")
    }

    #[test]
    fn geometry_command_is_byte_identical_to_the_web_client() {
        assert_eq!(
            terminal_geometry_command(80, 24),
            "stty rows 24 cols 80 >/dev/null 2>&1\r"
        );
        assert_eq!(
            terminal_geometry_command(120, 40),
            "stty rows 40 cols 120 >/dev/null 2>&1\r"
        );
        assert_eq!(
            terminal_geometry_command(0, 0),
            "stty rows 2 cols 2 >/dev/null 2>&1\r"
        );
        let web = web_source();
        assert!(
            web.contains("stty rows ${geometry.rows} cols ${geometry.cols} >/dev/null 2>&1\\r"),
            "web/terminal_geometry.js changed its stty command"
        );
    }

    #[test]
    fn prompt_detection_matches_the_web_client_rules() {
        for text in [
            "user@host:~$ ",
            "# ",
            "/srv/app$ ",
            "sh-5.1$ ",
            "[root@host]# ",
        ] {
            assert!(
                looks_like_shell_prompt(text),
                "expected prompt for {text:?}"
            );
        }
        for text in ["", "$ x", "running", "Loading kernel..."] {
            assert!(
                !looks_like_shell_prompt(text),
                "expected non-prompt for {text:?}"
            );
        }
        // An empty prefix after the trailing "$ " is still a prompt.
        assert!(looks_like_shell_prompt("$ "));
        assert!(looks_like_shell_prompt("~# "));

        let web = web_source();
        assert!(web.contains(r"/[$#] $/"), "web prompt regex changed");
        assert!(
            web.contains(r"/@\S+(?::\S*)?$/"),
            "web prompt regex changed"
        );
    }

    #[test]
    fn geometry_sync_waits_for_an_idle_prompt_and_dedupes() {
        let mut sync = TerminalGeometrySync::new(80, 24);
        sync.observe("booting the target\r\n");
        assert_eq!(sync.take_pending_command(), None);

        sync.observe("root@target:~$ ");
        // WEB_UX_SPEC 3.7: 180 ms debounce — the tick that saw the prompt
        // must not write anything.
        assert_eq!(sync.take_pending_command(), None);
        assert_eq!(
            due(&mut sync).as_deref(),
            Some("stty rows 24 cols 80 >/dev/null 2>&1\r")
        );
        // In flight: a second prompt must not queue a duplicate.
        assert_eq!(sync.take_pending_command(), None);
        sync.confirm_sent();
        sync.observe("root@target:~$ ");
        assert_eq!(sync.take_pending_command(), None);
    }

    #[test]
    fn geometry_sync_resends_after_a_resize() {
        let mut sync = TerminalGeometrySync::new(80, 24);
        sync.observe("root@target:~$ ");
        due(&mut sync);
        sync.confirm_sent();

        sync.set_size(120, 40);
        assert_eq!(sync.key(), "120x40");
        assert_eq!(sync.take_pending_command(), None);
        sync.observe("root@target:~$ ");
        assert_eq!(
            due(&mut sync).as_deref(),
            Some("stty rows 40 cols 120 >/dev/null 2>&1\r")
        );
    }

    #[test]
    fn geometry_sync_stops_at_a_prompt_that_is_not_idle() {
        let mut sync = TerminalGeometrySync::new(80, 24);
        sync.observe("root@target:~$ ");
        sync.mark_busy();
        assert_eq!(sync.take_pending_command(), None);
        assert_eq!(sync.deadline(), None, "an unobserved prompt arms no timer");
    }

    #[test]
    fn geometry_sync_retries_after_a_failed_send() {
        let mut sync = TerminalGeometrySync::new(80, 24);
        sync.observe("root@target:~$ ");
        due(&mut sync);
        sync.abort_sent();
        sync.observe("root@target:~$ ");
        assert!(due(&mut sync).is_some());
    }

    #[test]
    fn geometry_line_keeps_the_last_1024_characters() {
        let mut sync = TerminalGeometrySync::new(80, 24);
        let long: String = "x".repeat(GEOMETRY_LINE_BUFFER + 500);
        sync.observe(&long);
        assert_eq!(sync.line.chars().count(), GEOMETRY_LINE_BUFFER);

        // A prompt that arrives in two chunks still lines up.
        sync.observe("root@target:~");
        assert!(!looks_like_shell_prompt(&sync.line));
        sync.observe("$ ");
        assert!(looks_like_shell_prompt(&sync.line));
        assert_eq!(
            due(&mut sync).as_deref(),
            Some("stty rows 24 cols 80 >/dev/null 2>&1\r")
        );

        // \r and \n restart the line buffer.
        sync.observe("noise\r");
        assert!(sync.line.is_empty());
        assert!(!sync.prompt_visible);
    }

    #[test]
    fn prompt_detection_handles_multibyte_text_without_panicking() {
        // The 1024 cap counts characters, never bytes, so draining the buffer
        // must not split a multi-byte sequence.
        let mut sync = TerminalGeometrySync::new(80, 24);
        let long: String = "é".repeat(GEOMETRY_LINE_BUFFER + 10);
        sync.observe(&long);
        assert_eq!(sync.line.chars().count(), GEOMETRY_LINE_BUFFER);
        assert!(sync.line.is_char_boundary(sync.line.len()));
    }

    // ---- WEB_UX_SPEC 3.7: 180 ms debounce + idle input line ---------------

    #[test]
    fn geometry_sync_debounces_before_writing() {
        let mut sync = TerminalGeometrySync::new(80, 24);
        sync.observe("root@target:~$ ");
        // The tick that saw the prompt only arms the timer; nothing is due
        // until `GEOMETRY_DEBOUNCE_MS` has passed (web: `setTimeout(…, 180)`).
        assert_eq!(sync.take_pending_command(), None);
        let deadline = sync
            .deadline()
            .expect("an idle prompt arms the session timer");
        assert!(
            deadline > Instant::now(),
            "the deadline must sit in the future"
        );
        assert!(due(&mut sync).is_some());
    }

    #[test]
    fn geometry_sync_stays_off_while_the_input_line_is_open() {
        // The race this covers: the journal replay's trailing `…~$ ` arrives
        // behind keystrokes the user has already sent. The prompt is real, but
        // the line is not idle — writing the `stty` there splits the command
        // and the shell executes both halves as separate commands.
        let mut sync = TerminalGeometrySync::new(80, 24);
        sync.mark_input(b"echo ZMQ");
        assert!(sync.input_pending, "a line without a terminator is open");

        sync.observe("kickpi@kickpi-k2b:~$ ");
        std::thread::sleep(Duration::from_millis(GEOMETRY_DEBOUNCE_MS + 10));
        assert_eq!(
            sync.take_pending_command(),
            None,
            "an open input line must hold the sync back"
        );
        assert_eq!(
            sync.deadline(),
            None,
            "an open input line must not arm the timer"
        );

        // Terminating the line reopens the gate: once the shell prints a fresh
        // prompt behind it, the sync goes out there and only there.
        sync.mark_input(b"\r");
        assert!(!sync.input_pending, "CR closes the line");
        sync.observe("kickpi@kickpi-k2b:~$ ");
        assert!(due(&mut sync).is_some());
    }

    #[test]
    fn a_partially_delivered_line_latches_the_sync() {
        let mut sync = TerminalGeometrySync::new(80, 24);
        sync.mark_input(b"uname -a\r");
        assert!(!sync.input_pending);
        sync.latch_input();
        sync.observe("root@target:~$ ");
        assert_eq!(sync.deadline(), None);
        assert_eq!(sync.take_pending_command(), None);
    }

    #[test]
    fn answers_to_the_device_never_latch_the_line() {
        // A `DSR`/`CPR` reply is not terminal input (web sends it with
        // `trackPending: false`): it must leave the line closed, or a single
        // answer would hold the size sync back until the next Enter.
        let mut sync = TerminalGeometrySync::new(80, 24);
        sync.mark_input(b"echo hi\r");
        sync.mark_busy(); // what a transport reply does
        assert!(!sync.input_pending);
        sync.observe("root@target:~$ ");
        assert!(sync.deadline().is_some());
        assert!(due(&mut sync).is_some());
    }

    #[test]
    fn the_pending_line_rule_matches_the_web_client() {
        assert!(input_leaves_pending_line(b"echo hi", false));
        assert!(!input_leaves_pending_line(b"\r", true));
        // Only the last byte decides; backspace and cursor movement leave the
        // line's contents unknown, so they keep it pending.
        assert!(input_leaves_pending_line(b"ab\x7f", false));
        assert!(input_leaves_pending_line(b"ab\x1b[C", false));
        assert!(!input_leaves_pending_line(b"ab\x03", false));
        assert!(input_leaves_pending_line(b"", true), "no send, no change");

        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../web/agent_execution_policy.js");
        let web = std::fs::read_to_string(path).expect("read agent_execution_policy.js");
        assert!(
            web.contains("pending = byte !== 10 && byte !== 13 && byte !== 3"),
            "web inputLeavesPendingLine changed its terminator rule"
        );
    }
}
