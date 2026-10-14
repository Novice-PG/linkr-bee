//! Which channel a transfer can use, asked of the device itself.
//!
//! `web/target_files.js` has no such question: the web client always pages a
//! file with `dd | base64`, because a browser has no `sz` to offer and no way
//! to run one. A host program is not a browser — it can be the far end of a
//! ZMODEM session — so which channel to use is a question this client has to
//! answer before it types anything, and the only honest place to get the answer
//! is the device: a board with `lrzsz` installed can take a 1 KiB-block
//! session, and one with nothing but busybox cannot.
//!
//! The probe is therefore a shell command and a parser, and nothing else. It
//! names the tools and lets the caller decide; it does not know about links,
//! timeouts or views. `TARGET_FILE_PROBE` of `web/target_files.js` asks for the
//! pager's tools — this asks for theirs *and* for ZMODEM's, so one round trip
//! answers both.
//!
//! Every marker is a whole line and matched only as a complete line, for the
//! same reason as in `files.rs`: the shell echoes the command back, and an echo
//! that happened to contain a marker would otherwise be read as an answer.

/// The tool list the probe asks about, in the order it prints them.
///
/// `lrzsz` ships `sz`/`rz` as one package, `lrz`/`lsz` are the same tools under
/// the names busybox systems often give them, and the rest are what the
/// base64 pager of `files.rs` builds its commands from.
pub const PROBE_TOOLS: [&str; 9] = [
    "lrzsz", "sz", "rz", "lrz", "lsz", "dd", "base64", "wc", "tr",
];

/// The digest tools, asked separately: a pager can move bytes without one, so
/// they decide whether an upload can be *verified*, not whether it can happen.
pub const DIGEST_TOOLS: [&str; 3] = ["sha256sum", "shasum", "openssl"];

const BEGIN: &str = "LINKR_XFER_BEGIN";
const END: &str = "LINKR_XFER_END";
const TOOL_PREFIX: &str = "LINKR_XTOOL:";

/// The command that asks the device what it has.
///
/// `command -v` rather than `which`: `which` is not in POSIX, is missing on
/// busybox systems that have everything else, and exits non-zero there for
/// tools that are installed. The trailing `:` keeps the command's exit status
/// at zero, because a shell that prints an error for a missing tool is a shell
/// that has answered the question already.
pub const TRANSFER_PROBE: &str = "printf 'LINKR_XFER_BEGIN\\n'; for t in lrzsz sz rz lrz lsz dd base64 wc tr sha256sum shasum openssl; do command -v \"$t\" >/dev/null 2>&1 && printf 'LINKR_XTOOL:%s\\n' \"$t\"; done; printf 'LINKR_XFER_END\\n'; :";

/// The channel a transfer would take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    /// A ZMODEM session: this host is one end, the device's `sz`/`rz` the other.
    Zmodem,
    /// The shell pager of `files.rs`: `dd | base64`, one chunk per command.
    Base64,
}

impl Channel {
    /// How the channel names itself. This is a protocol name, not a sentence,
    /// so it is the same in both languages — a translated `ZMODEM` would be a
    /// string the reader cannot search for.
    pub fn as_str(self) -> &'static str {
        match self {
            Channel::Zmodem => "ZMODEM",
            Channel::Base64 => "dd|base64",
        }
    }
}

/// What the device answered.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProbeReport {
    /// Every tool the device said it has.
    pub tools: Vec<String>,
}

impl ProbeReport {
    /// Whether the device has the named tool.
    pub fn has(&self, tool: &str) -> bool {
        self.tools.iter().any(|t| t == tool)
    }

    /// Whether the device can be one end of a ZMODEM session.
    ///
    /// `lrzsz` the *package* is not a command, and a device that reported only
    /// that name still has to be read as having `sz` and `rz`: that is what
    /// installing the package gives you, and a probe that demanded the two
    /// names separately would send a working board down the slow channel.
    pub fn zmodem(&self) -> bool {
        if self.has("lrzsz") {
            return true;
        }
        (self.has("sz") || self.has("lsz")) && (self.has("rz") || self.has("lrz"))
    }

    /// Whether the device has everything the base64 pager builds on.
    ///
    /// `wc` and `tr` are in the list because `parse_file_read` counts the
    /// bytes it was handed and strips the markers with them — a pager without
    /// them can encode and cannot verify.
    pub fn pager(&self) -> bool {
        self.has("dd") && self.has("base64") && self.has("wc") && self.has("tr")
    }

    /// The tool an uploaded file's digest can be taken with, if any.
    pub fn digest(&self) -> Option<&str> {
        DIGEST_TOOLS.iter().copied().find(|tool| self.has(tool))
    }

    /// The channel this device gets. `None` when it has neither: a board with
    /// no `lrzsz` and no pager tools cannot take a file either way, and saying
    /// so here is better than starting a run that can only fail.
    pub fn channel(&self) -> Option<Channel> {
        if self.zmodem() {
            Some(Channel::Zmodem)
        } else if self.pager() {
            Some(Channel::Base64)
        } else {
            None
        }
    }
}

/// Read a probe's answer.
///
/// `None` when the answer is not finished: the device may still be typing, and
/// a report built off a half-printed list would read "no lrzsz" to a board that
/// has it. Only the end marker closes the question.
pub fn parse_probe(text: &str) -> Option<ProbeReport> {
    let mut tools = Vec::new();
    let mut started = false;
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line == BEGIN {
            started = true;
            tools.clear();
            continue;
        }
        if line == END {
            return Some(ProbeReport { tools });
        }
        if !started {
            continue;
        }
        if let Some(name) = line.strip_prefix(TOOL_PREFIX) {
            let name = name.trim();
            if !name.is_empty() && !tools.iter().any(|t| t == name) {
                tools.push(name.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(tools: &[&str]) -> String {
        let mut text = format!("{BEGIN}\n");
        for tool in tools {
            text.push_str(&format!("{TOOL_PREFIX}{tool}\n"));
        }
        text.push_str(&format!("{END}\n"));
        text
    }

    #[test]
    fn an_unfinished_answer_is_not_a_report() {
        assert_eq!(parse_probe("LINKR_XFER_BEGIN\nLINKR_XTOOL:sz\n"), None);
        assert_eq!(parse_probe("nothing here"), None);
    }

    #[test]
    fn the_command_echo_is_not_an_answer() {
        // The shell prints the command back, and the command contains the
        // marker strings.
        let echo = format!("{TRANSFER_PROBE}\n");
        assert_eq!(parse_probe(&echo), None);
    }

    #[test]
    fn a_full_answer_lists_the_tools() {
        let report = parse_probe(&answer(&["sz", "rz", "dd", "base64", "wc", "tr"])).unwrap();
        assert!(report.zmodem());
        assert!(report.pager());
        assert_eq!(report.channel(), Some(Channel::Zmodem));
    }

    #[test]
    fn a_board_with_only_lrzsz_is_still_zmodem() {
        // The package name, not the two commands: that is what installing it
        // gives you.
        let report = parse_probe(&answer(&["lrzsz", "dd", "base64", "wc", "tr"])).unwrap();
        assert!(report.zmodem());
    }

    #[test]
    fn a_busybox_board_falls_back_to_the_pager() {
        let report = parse_probe(&answer(&["dd", "base64", "wc", "tr", "sha256sum"])).unwrap();
        assert!(!report.zmodem());
        assert_eq!(report.channel(), Some(Channel::Base64));
        assert_eq!(report.digest(), Some("sha256sum"));
    }

    #[test]
    fn a_board_with_nothing_has_no_channel() {
        let report = parse_probe(&answer(&["sh"])).unwrap();
        assert_eq!(report.channel(), None);
    }

    #[test]
    fn a_half_zmodem_is_not_zmodem() {
        // `sz` with no `rz` can send and cannot receive; falling back is right
        // for a download and the pager covers both.
        let report = parse_probe(&answer(&["sz", "dd", "base64", "wc", "tr"])).unwrap();
        assert!(!report.zmodem());
        assert_eq!(report.channel(), Some(Channel::Base64));
    }

    #[test]
    fn the_probe_command_asks_for_every_tool_it_parses() {
        for tool in PROBE_TOOLS.iter().chain(DIGEST_TOOLS.iter()) {
            assert!(
                TRANSFER_PROBE.contains(tool),
                "the probe never asks about {tool}"
            );
        }
        assert!(TRANSFER_PROBE.contains(BEGIN) && TRANSFER_PROBE.contains(END));
    }
}
