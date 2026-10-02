//! Terminal geometry sync: port of `TerminalGeometrySync` and friends.
//! The emitted command must stay byte-identical to web/terminal_geometry.js:
//! `stty rows {rows} cols {cols} >/dev/null 2>&1\r`.

use std::sync::OnceLock;

use regex::Regex;

pub const MIN_DIMENSION: u32 = 2;
pub const MAX_DIMENSION: u32 = 1000;
pub const GEOMETRY_LINE_BUFFER: usize = 1024;

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

    /// Called on every local send: the prompt is no longer idle.
    pub fn mark_busy(&mut self) {
        self.prompt_visible = false;
        self.line.clear();
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
        self.prompt_visible = looks_like_shell_prompt(&self.line);
    }

    /// Returns the stty command when a new, idle, unsynced geometry is due.
    pub fn take_pending_command(&mut self) -> Option<String> {
        if !self.prompt_visible || self.key == self.synced || self.key == self.in_flight {
            return None;
        }
        self.in_flight = self.key.clone();
        self.prompt_visible = false;
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
    fn geometry_sync_resends_after_a_resize() {
        let mut sync = TerminalGeometrySync::new(80, 24);
        sync.observe("root@target:~$ ");
        sync.take_pending_command();
        sync.confirm_sent();

        sync.set_size(120, 40);
        assert_eq!(sync.key(), "120x40");
        assert_eq!(sync.take_pending_command(), None);
        sync.observe("root@target:~$ ");
        assert_eq!(
            sync.take_pending_command().as_deref(),
            Some("stty rows 40 cols 120 >/dev/null 2>&1\r")
        );
    }

    #[test]
    fn geometry_sync_stops_at_a_prompt_that_is_not_idle() {
        let mut sync = TerminalGeometrySync::new(80, 24);
        sync.observe("root@target:~$ ");
        sync.mark_busy();
        assert_eq!(sync.take_pending_command(), None);
    }

    #[test]
    fn geometry_sync_retries_after_a_failed_send() {
        let mut sync = TerminalGeometrySync::new(80, 24);
        sync.observe("root@target:~$ ");
        sync.take_pending_command();
        sync.abort_sent();
        sync.observe("root@target:~$ ");
        assert!(sync.take_pending_command().is_some());
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
            sync.take_pending_command().as_deref(),
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
}
