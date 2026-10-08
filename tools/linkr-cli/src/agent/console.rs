//! Passive console-state hints: port of `web/serial_console.js`.
//!
//! These are hints only: device output is untrusted and prompts can be
//! customized, so a match never grants shell access — it only tells the
//! executor which kind of interaction the console seems to expect.

use regex::Regex;
use serde_json::{json, Value};
use std::sync::OnceLock;

/// Kinds the executor and the monitor understand.
pub const INTERACTIVE_KINDS: &[&str] = &[
    "login",
    "password",
    "sudo-password",
    "confirmation",
    "pager",
    "bootloader",
];

#[derive(Debug, Clone, PartialEq)]
pub struct ConsoleHint {
    pub kind: String,
    pub evidence: String,
    pub cursor: u64,
    pub source: &'static str,
}

impl ConsoleHint {
    pub fn unknown(cursor: u64) -> Self {
        ConsoleHint {
            kind: "unknown".into(),
            evidence: String::new(),
            cursor,
            source: "serial-output-heuristic",
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "kind": self.kind,
            "evidence": self.evidence,
            "cursor": self.cursor,
            "source": self.source,
        })
    }
}

/// Compile one pattern. All callers build each pattern once, lazily.
fn pattern(source: &str) -> Regex {
    Regex::new(source).expect("console pattern")
}

/// Match the current tail, never grant shell access from an earlier boot
/// message.
pub fn inspect_serial_console(text: &str, latest_cursor: u64) -> ConsoleHint {
    static LOGIN: OnceLock<Regex> = OnceLock::new();
    static SUDO: OnceLock<Regex> = OnceLock::new();
    static CONFIRM: OnceLock<Regex> = OnceLock::new();
    static PAGER: OnceLock<Regex> = OnceLock::new();
    static PASSWORD: OnceLock<Regex> = OnceLock::new();
    static BOOTLOADER: OnceLock<Regex> = OnceLock::new();
    static SHELL: OnceLock<Regex> = OnceLock::new();
    static PANIC: OnceLock<Regex> = OnceLock::new();

    let login = LOGIN.get_or_init(|| pattern(r"(?i)^(?:[^\s:]+\s+)?login:\s*$"));
    let sudo = SUDO.get_or_init(|| pattern(r"(?i)^\[sudo\] password for .+:\s*$"));
    let confirm = CONFIRM.get_or_init(|| {
        pattern(r"(?i)(?:\[(?:Y/n|y/N|y/n|yes/no)\]|\((?:y/n|yes/no)\)|(?:continue|proceed)\?\s*)[:?]?\s*$")
    });
    let pager = PAGER.get_or_init(|| {
        pattern(r"(?i)(?:--More--(?:\([^)]*\))?|\(END\)|Press (?:any key|ENTER|RETURN)(?: to [^.]+)?[.:]?)\s*$")
    });
    let password = PASSWORD.get_or_init(|| pattern(r"(?i)^(?:[^\r\n]{0,80}\s)?password:\s*$"));
    let bootloader = BOOTLOADER.get_or_init(|| pattern(r"^(?:=>|U-Boot>)\s*$"));
    let shell = SHELL.get_or_init(|| {
        pattern(r"^(?:[\w.-]+@[\w.-]+:[^\r\n]*|\[[\w.-]+@[\w.-]+ [^\]\r\n]*\])[$#]\s*$")
    });
    let panic = PANIC.get_or_init(|| pattern(r"Kernel panic - not syncing:"));

    let tail_text: String = {
        let sliced = if text.chars().count() > 4000 {
            text.chars().skip(text.chars().count() - 4000).collect()
        } else {
            text.to_string()
        };
        sliced.replace('\r', "\n")
    };
    let lines: Vec<&str> = tail_text.split('\n').collect();
    let tail = lines.last().copied().unwrap_or("").trim_end();

    let mut kind = "unknown";
    if login.is_match(tail) {
        kind = "login";
    } else if sudo.is_match(tail) {
        kind = "sudo-password";
    } else if confirm.is_match(tail) {
        kind = "confirmation";
    } else if pager.is_match(tail) {
        kind = "pager";
    } else if password.is_match(tail) {
        kind = "password";
    } else if bootloader.is_match(tail) {
        kind = "bootloader";
    } else if shell.is_match(tail) {
        kind = "shell";
    } else if lines.iter().any(|line| panic.is_match(line)) {
        kind = "panic";
    }

    let evidence = if kind == "unknown" {
        String::new()
    } else if kind == "panic" {
        lines
            .iter()
            .find(|line| panic.is_match(line))
            .map(|line| slice_chars(line, 240))
            .unwrap_or_default()
    } else {
        slice_chars(tail, 240)
    };

    ConsoleHint {
        kind: kind.to_string(),
        evidence,
        cursor: latest_cursor,
        source: "serial-output-heuristic",
    }
}

/// One bounded line of evidence, in characters like the rest of the crate.
pub fn slice_chars(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const JS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../web/serial_console.js");

    #[test]
    fn hints_match_the_js_heuristic() {
        let source = std::fs::read_to_string(JS).expect("read web/serial_console.js");
        assert!(source.contains("serial-output-heuristic"));

        let cases = [
            ("buildroot login: ", "login"),
            ("[sudo] password for root: ", "sudo-password"),
            ("Overwrite /tmp/x? [y/N] ", "confirmation"),
            ("--More-- ", "pager"),
            ("Password: ", "password"),
            ("=>", "bootloader"),
            ("root@target:~# ", "shell"),
            (
                "[   12.3] Kernel panic - not syncing: Attempted to kill init!",
                "panic",
            ),
            ("random noise", "unknown"),
        ];
        for (tail, expected) in cases {
            let hint = inspect_serial_console(tail, 42);
            assert_eq!(hint.kind, expected, "tail {tail:?}");
            assert_eq!(hint.cursor, 42);
            assert_eq!(hint.source, "serial-output-heuristic");
        }
    }

    #[test]
    fn interactive_kinds_drive_the_waiting_for_field() {
        // `executor.rs` gates on this same set: those are the consoles a
        // tool has to wait on instead of answering itself.
        let prompt = inspect_serial_console("Password: ", 1);
        let shell = inspect_serial_console("root@target:~# ", 1);
        assert!(INTERACTIVE_KINDS.contains(&prompt.kind.as_str()));
        assert!(!INTERACTIVE_KINDS.contains(&shell.kind.as_str()));
        assert_eq!(INTERACTIVE_KINDS.len(), 6);
    }

    #[test]
    fn panic_evidence_is_the_panic_line_not_the_tail() {
        let text = "noise\n[   12.3] Kernel panic - not syncing: Attempted to kill init!";
        let hint = inspect_serial_console(text, 7);
        assert_eq!(hint.kind, "panic");
        assert!(hint.evidence.contains("Kernel panic - not syncing:"));
        assert!(hint.evidence.chars().count() <= 240);
    }
}
