//! What the device can page with, asked of the device itself.
//!
//! There is no channel to choose any more, and that is the point of this
//! module's small size. `web/target_files.js` moves every file with `dd |
//! base64`, because a browser has no other way to reach a board's filesystem —
//! and the bridge cannot carry a lossless binary stream anyway: the ESP32's
//! BLE→UART queue drops bytes whenever the UART drains slower than the radio
//! delivers, and the ZMODEM sender at this end cannot see those drops to avoid
//! them. Flow control is what would fix it, and that is firmware this client
//! does not own. So the pager is the protocol, exactly as it is on the web, and
//! the only question left is whether *this* board has the tools it builds from.
//!
//! The command is `files::TARGET_FILE_PROBE` — the web's own string, character
//! for character, not a second copy of it. What lives here is the reading of
//! its answer.



/// The tools a read needs, and the reason each is asked about.
///
/// `dd` reads the byte range, `base64` makes the bytes safe to carry over a
/// console that eats control characters, and `wc` and `tr` are what the reader
/// uses to count and clean the result. Without the last two a pager could still
/// encode a page but not verify it, which is why they are required rather than
/// optional.
pub const PAGER_TOOLS: [&str; 4] = ["dd", "base64", "wc", "tr"];

/// The tools an upload's digest can be taken with, in the order the web's own
/// command asks for them. A pager can move bytes without one; they decide
/// whether an upload can be *verified*, not whether it can happen.
pub const DIGEST_TOOLS: [&str; 2] = ["sha256sum", "shasum"];

/// The command that asks the device what it has. Re-exported rather than
/// retyped, so the web's probe and this client's cannot drift apart.
/// The command that asks the device what it can page with.
///
/// It is the web client's own string, character for character, because a board
/// that answers one has to answer the other or the two clients disagree about
/// what the same hardware can do.
///
/// It used to live in the file-transfer module this crate no longer carries.
/// Nothing outside it was used except this string, so the string moved here and
/// the rest went: 2,300 lines of upload planning, chunking and verification that
/// only `web/target_files.js` drives, in a TUI that has no way to start one.
pub const TARGET_FILE_PROBE: &str =
    "for t in dd base64 wc tr sha256sum shasum; do command -v \"$t\" >/dev/null 2>&1 && printf 'LINKR_TOOL:%s\\n' \"$t\"; done; :";

pub const TRANSFER_PROBE: &str = TARGET_FILE_PROBE;

/// The marker each answered tool is printed under, from `web/target_files.js`.
const TOOL_PREFIX: &str = "LINKR_TOOL:";

/// What the device answered.
///
/// A tool that is absent is absent from `tools`: the web's command prints only
/// what `command -v` found, so a name that never appears is the negative answer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProbeReport {
    /// Every tool the device said it has, in the order the answers arrived.
    pub tools: Vec<String>,
}

impl ProbeReport {
    /// Whether the device has the named tool.
    pub fn has(&self, tool: &str) -> bool {
        self.tools.iter().any(|t| t == tool)
    }

    /// Whether the device has everything the reader's commands build on.
    pub fn pager(&self) -> bool {
        PAGER_TOOLS.iter().all(|tool| self.has(tool))
    }

    /// Whether the device has everything the writer's commands build on. The
    /// pager's tools are the same set: an upload writes the chunks it base64'd.
    pub fn writer(&self) -> bool {
        self.pager()
    }

    /// The tool an uploaded file's digest can be taken with, if any.
    pub fn digest(&self) -> Option<&str> {
        DIGEST_TOOLS.iter().copied().find(|tool| self.has(tool))
    }

    /// The pager tools the device did *not* report, in the order they are
    /// asked for. Empty when the device can take a file.
    ///
    /// Naming the missing tools rather than a single verdict is what lets the
    /// view say "this board has no `base64`" instead of "cannot transfer": the
    /// first is something an operator can act on, the second is not.
    pub fn missing(&self) -> Vec<&'static str> {
        PAGER_TOOLS
            .iter()
            .copied()
            .filter(|tool| !self.has(tool))
            .collect()
    }

    /// Whether the device can take a file at all.
    pub fn usable(&self) -> bool {
        self.pager()
    }
}

/// Read a probe's answer out of whatever the console has printed so far.
///
/// Returns `None` only when nothing was recognised at all, and that is a
/// deliberate distinction: the answer has no end marker — it is `command -v`
/// output, and the last tool's line is the last line — so the caller cannot ask
/// "is it finished?" and must instead ask "has it said anything yet?". The
/// caller settles the question with time, which is what `App::fold_probe` does.
///
/// Only whole lines are accepted. The shell echoes the command back, and the
/// command contains the marker string, so a substring match would read the echo
/// as an answer.
pub fn parse_probe(text: &str) -> Option<ProbeReport> {
    let mut tools = Vec::new();
    let mut answered = false;
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        let Some(name) = line.strip_prefix(TOOL_PREFIX) else {
            continue;
        };
        let name = name.trim();
        // A name with anything but a tool name in it is not this protocol:
        // `LINKR_TOOL:...` appears inside the echoed command followed by the
        // rest of the line, and `command -v` prints nothing else.
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            continue;
        }
        answered = true;
        if !tools.iter().any(|t| t == name) {
            tools.push(name.to_string());
        }
    }
    answered.then_some(ProbeReport { tools })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(tools: &[&str]) -> String {
        tools
            .iter()
            .map(|tool| format!("{TOOL_PREFIX}{tool}\r\n"))
            .collect()
    }

    #[test]
    fn a_device_with_the_pagers_tools_can_take_a_file() {
        let report = parse_probe(&answer(&["dd", "base64", "wc", "tr"])).unwrap();
        assert!(report.usable());
        assert!(report.writer());
        assert!(report.missing().is_empty());
    }

    #[test]
    fn a_digest_tool_is_reported_when_it_is_there() {
        let report = parse_probe(&answer(&["dd", "base64", "wc", "tr", "sha256sum"])).unwrap();
        assert_eq!(report.digest(), Some("sha256sum"));
    }

    #[test]
    fn shasum_is_taken_when_sha256sum_is_not() {
        let report = parse_probe(&answer(&["dd", "base64", "wc", "tr", "shasum"])).unwrap();
        assert_eq!(report.digest(), Some("shasum"));
    }

    #[test]
    fn a_board_without_a_digest_tool_can_still_move_bytes() {
        let report = parse_probe(&answer(&["dd", "base64", "wc", "tr"])).unwrap();
        assert!(report.usable());
        assert_eq!(report.digest(), None);
    }

    #[test]
    fn the_missing_tools_are_named_in_the_order_they_are_asked_for() {
        // The one tool this board lacks is the one worth naming.
        let report = parse_probe(&answer(&["dd", "wc", "tr"])).unwrap();
        assert!(!report.usable());
        assert_eq!(report.missing(), vec!["base64"]);
    }

    #[test]
    fn a_board_with_nothing_is_unusable_and_names_everything() {
        let report = parse_probe(&answer(&["sh", "sed"])).unwrap();
        assert!(!report.usable());
        assert_eq!(report.missing(), PAGER_TOOLS.to_vec());
    }

    #[test]
    fn the_command_echo_is_not_an_answer() {
        // The shell prints the command back and the command contains the
        // marker. No whole line of it is a tool name, so nothing is read.
        let echo = format!("$ {TRANSFER_PROBE}\r\n");
        assert_eq!(parse_probe(&echo), None);
    }

    #[test]
    fn nothing_printed_yet_is_no_report() {
        assert_eq!(parse_probe(""), None);
        assert_eq!(parse_probe("$ \r\n"), None);
    }

    #[test]
    fn a_tool_named_twice_is_listed_once() {
        let report = parse_probe(&answer(&["dd", "dd", "base64", "wc", "tr"])).unwrap();
        assert_eq!(report.tools.len(), 4);
    }

    #[test]
    fn the_probe_asks_for_every_tool_it_reads() {
        for tool in PAGER_TOOLS.iter().chain(DIGEST_TOOLS.iter()) {
            assert!(
                TRANSFER_PROBE.contains(tool),
                "the probe never asks about {tool}"
            );
        }
        // And what it asks for is the web's own command, not a rewrite of it.
        assert_eq!(TRANSFER_PROBE, TARGET_FILE_PROBE);
    }
}
