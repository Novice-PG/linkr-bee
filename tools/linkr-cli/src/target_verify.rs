//! Machine-decided verification of target file/service state.
//! Port of web/target_verify.js: match / mismatch / indeterminate / observed.
//!
//! Every other tool hands the model evidence and leaves the conclusion to it;
//! for a narrow, checkable set of claims the APPLICATION compares what the
//! target reported against what the caller expected and returns one of four
//! verdicts. The verdict cannot be produced by prose, cannot be produced by the
//! echoed command, and does not depend on the model reading output correctly.
//!
//!   match          every requested check agreed with the expectation
//!   mismatch       at least one requested check disagreed, or the thing being
//!                  checked is provably not what was expected
//!   indeterminate  the target could not answer, or the answer is unusable
//!   observed       no expectation was supplied: a measurement, never a
//!                  verification
//!
//! MARKERS. Same discipline as `target_files.js`: one marker per whole line,
//! matched only as a complete line, printed with `printf`. `done` is printed on
//! every path, early refusals included, so a missing `done` is unambiguous.

use crate::target_files::{marker_lines, quote_shell};
use anyhow::{bail, Result};
use regex::Regex;
use serde_json::json;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::OnceLock;

/// Command-size budget: one verify command is a single line of POSIX shell,
/// so it stays far below an upload chunk.
pub const MAX_VERIFY_COMMAND_BYTES: usize = 4096;
pub const MAX_VERIFY_PATH: usize = 200;
pub const MAX_VERIFY_PATTERN: usize = 200;
/// Bounds on the blocks a target writes back, so one verify cannot bury the
/// console history the operator is watching.
pub const MAX_LISTENER_ROWS: usize = 60;
pub const MAX_PROCESS_PIDS: usize = 50;

/// Evidence lines kept with a verdict.
const MAX_EVIDENCE: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Match,
    Mismatch,
    Indeterminate,
    Observed,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Match => "match",
            Verdict::Mismatch => "mismatch",
            Verdict::Indeterminate => "indeterminate",
            Verdict::Observed => "observed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckState {
    NotRequested,
    Match,
    Mismatch,
    Unknown,
}

impl CheckState {
    pub fn as_str(self) -> &'static str {
        match self {
            CheckState::NotRequested => "not-requested",
            CheckState::Match => "match",
            CheckState::Mismatch => "mismatch",
            CheckState::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceKind {
    Unit,
    Process,
    Port,
}

impl ServiceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ServiceKind::Unit => "unit",
            ServiceKind::Process => "process",
            ServiceKind::Port => "port",
        }
    }
}

fn re_path() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^LINKR_VERIFY:path=(.*)$").unwrap())
}

fn re_bytes() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^LINKR_VERIFY:bytes=\s*(\d+)$").unwrap())
}

fn re_sha() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^LINKR_VERIFY:sha256=([0-9a-f]{64}|unavailable)$").unwrap())
}

fn re_subject() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^LINKR_VERIFY:subject=(unit|process|port):(.*)$").unwrap())
}

fn re_unsupported() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^LINKR_VERIFY:unsupported=([a-z-]+)$").unwrap())
}

fn re_setting() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^([A-Za-z]+)=(\S*)$").unwrap())
}

fn re_listen_address() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^\S*:(\d+)$").unwrap())
}

fn re_listener_tool() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^LINKR_VERIFY:listener-tool=(ss|netstat)$").unwrap())
}

fn re_pid() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^\d+$").unwrap())
}

/// Findings that describe the path rather than its content, in the JS
/// insertion order (it decides which finding wins when several match).
/// `content` says whether the finding refutes specific content: a refusal to
/// read claims nothing, because the bytes were never seen.
const FILE_FINDINGS: &[(&str, &str, bool, &str)] = &[
    (
        "missing",
        "missing",
        true,
        "The target path does not exist.",
    ),
    (
        "directory",
        "directory",
        true,
        "The target path is a directory, not a file.",
    ),
    (
        "not-regular",
        "other",
        true,
        "The target path is not a regular file, so it has no content to verify.",
    ),
    (
        "not-absolute",
        "unknown",
        false,
        "The target path is not absolute, so the check never ran against a real file.",
    ),
    (
        "denied",
        "unknown",
        false,
        "The target file exists but is not readable, so its content could not be checked.",
    ),
    (
        "unreadable",
        "unknown",
        false,
        "The target file could not be measured: the size command failed.",
    ),
];

const UNSUPPORTED_TEXT: &[(&str, &str)] = &[
    (
        "no-systemctl",
        "The target has no systemctl, so it cannot report service state.",
    ),
    (
        "no-pgrep",
        "The target has no pgrep, so it cannot report matching processes.",
    ),
    (
        "no-listener-tool",
        "The target has neither ss nor netstat, so it cannot report listening ports.",
    ),
];

/// Allowed expectation per service kind, first entry = the default.
pub const SERVICE_EXPECTATIONS: &[(&str, &[&str])] = &[
    ("unit", &["active", "inactive", "failed"]),
    ("process", &["running", "absent"]),
    ("port", &["listening", "closed"]),
];

fn expectations_for(kind: &str) -> Option<&'static [&'static str]> {
    SERVICE_EXPECTATIONS
        .iter()
        .find(|(name, _)| *name == kind)
        .map(|(_, allowed)| *allowed)
}

/// The expectation is validated here rather than in the tool layer so that an
/// unsupported value can never silently become "no check".
pub fn normalize_expectation(kind: &str, value: &str) -> Result<String> {
    let Some(allowed) = expectations_for(kind) else {
        bail!("Unknown service kind: {kind}");
    };
    if value.is_empty() {
        return Ok(allowed[0].to_string());
    }
    if !allowed.contains(&value) {
        bail!(
            "Expected {kind} state must be one of {}.",
            allowed.join(", ")
        );
    }
    Ok(value.to_string())
}

fn verify_path(value: &str) -> Result<&str> {
    if value.is_empty() {
        bail!("Target path must be a non-empty string.");
    }
    if value.chars().count() > MAX_VERIFY_PATH {
        bail!("Target path must not exceed {MAX_VERIFY_PATH} characters.");
    }
    /* Control characters would break the line-oriented markers, and a forged
     * marker line could then answer for the target. A relative path is allowed
     * through: the shell reports it as not-absolute. */
    if value.chars().any(|c| c.is_control()) {
        bail!("Target path must not contain control characters.");
    }
    Ok(value)
}

/// A digest is only accepted as an expectation, never as a measurement. A
/// malformed expectation throws rather than quietly disabling the check.
pub fn normalize_expected_sha256(value: &str) -> Result<String> {
    if value.is_empty() {
        return Ok(String::new());
    }
    if !(value.len() == 64 && value.chars().all(|c| c.is_ascii_hexdigit())) {
        bail!("An expected sha256 must be 64 hexadecimal characters.");
    }
    Ok(value.to_lowercase())
}

/// A size expectation. Zero is meaningful: a truncated download of an empty
/// file is a real result, so it is accepted like any other count.
pub fn normalize_expected_bytes(value: Option<i64>) -> Result<Option<u64>> {
    match value {
        None => Ok(None),
        Some(v) if v < 0 => bail!("An expected byte count must be a non-negative integer."),
        Some(v) => Ok(Some(v as u64)),
    }
}

fn verify_pattern(value: &str) -> Result<&str> {
    if value.is_empty() {
        bail!("A process pattern must be a non-empty string.");
    }
    if value.chars().count() > MAX_VERIFY_PATTERN {
        bail!("A process pattern must not exceed {MAX_VERIFY_PATTERN} characters.");
    }
    if value.chars().any(|c| c.is_control()) {
        bail!("A process pattern must not contain control characters.");
    }
    Ok(value)
}

/// Unit names are handed to systemctl, and a name beginning with `-` would be
/// read as an option even when quoted. The character set is systemd's own.
fn verify_unit_name(value: &str) -> Result<&str> {
    let chars: Vec<char> = value.chars().collect();
    let ok = !chars.is_empty()
        && chars.len() <= 128
        && (chars[0].is_ascii_alphanumeric())
        && chars[1..]
            .iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '@' | '.' | '_' | ':' | '-'));
    if !ok {
        bail!("A unit name must start with a letter or digit and contain only letters, digits, @ . _ : -");
    }
    Ok(value)
}

fn verify_port(value: i64) -> Result<u64> {
    if !(1..=65535).contains(&value) {
        bail!("A port must be an integer between 1 and 65535.");
    }
    Ok(value as u64)
}

fn guard_command(command: String) -> Result<String> {
    if command.len() > MAX_VERIFY_COMMAND_BYTES {
        bail!(
            "Verify command would be {} characters, over the {MAX_VERIFY_COMMAND_BYTES}-character UART budget; shorten the path or pattern.",
            command.len()
        );
    }
    Ok(command)
}

/// Verify one target file.
///
/// `hash` decides whether the digest is computed, because hashing reads the
/// whole file on the target: a size-only check on a 4 GiB image must not pay
/// for a digest nobody asked for.
pub fn verify_file_command(path: &str, hash: bool) -> Result<String> {
    let p = quote_shell(verify_path(path)?)?;
    let mut parts = vec![
        format!("p={p}"),
        // Printed before any finding so a verdict is always attributable.
        r#"printf '\nLINKR_VERIFY:path=%s\n' "$p""#.to_string(),
        r#"case "$p" in /*) ;; *) printf 'LINKR_VERIFY:not-absolute\nLINKR_VERIFY:done\n'; exit 0 ;; esac"#.to_string(),
        r#"[ -e "$p" ] || { printf 'LINKR_VERIFY:missing\nLINKR_VERIFY:done\n'; exit 0; }"#.to_string(),
        r#"if [ -d "$p" ]; then printf 'LINKR_VERIFY:directory\nLINKR_VERIFY:done\n'; exit 0; fi"#.to_string(),
        r#"[ -f "$p" ] || { printf 'LINKR_VERIFY:not-regular\nLINKR_VERIFY:done\n'; exit 0; }"#.to_string(),
        r#"[ -r "$p" ] || { printf 'LINKR_VERIFY:denied\nLINKR_VERIFY:done\n'; exit 0; }"#.to_string(),
        // The size is measured, not scraped from a tool's summary line.
        r#"n=$(wc -c < "$p" 2>/dev/null) || n="#.to_string(),
        r#"[ -n "$n" ] || { printf 'LINKR_VERIFY:unreadable\nLINKR_VERIFY:done\n'; exit 0; }"#.to_string(),
        r#"printf 'LINKR_VERIFY:file\nLINKR_VERIFY:bytes=%s\n' "$n""#.to_string(),
    ];
    if hash {
        parts.push(
            r#"if command -v sha256sum >/dev/null 2>&1; then h=$(sha256sum "$p" 2>/dev/null); elif command -v shasum >/dev/null 2>&1; then h=$(shasum -a 256 "$p" 2>/dev/null); else h=; fi"#
                .to_string(),
        );
        // Both printers write "<hash>  <file>"; keep only the digest column.
        parts.push(r#"h=${h%% *}"#.to_string());
        parts.push(r#"[ -n "$h" ] || h=unavailable"#.to_string());
        parts.push(r#"printf 'LINKR_VERIFY:sha256=%s\n' "$h""#.to_string());
    }
    parts.push(r#"printf 'LINKR_VERIFY:done\n'"#.to_string());
    guard_command(parts.join("; "))
}

/// Verify one service claim: a systemd unit, a process matching a pattern, or
/// a listening TCP port. Exactly one of `unit` / `process` / `port` must be set.
pub fn verify_service_command(
    unit: &str,
    process: &str,
    port: Option<i64>,
    expect: &str,
) -> Result<String> {
    let requested = [!unit.is_empty(), !process.is_empty(), port.is_some()];
    if requested.iter().filter(|&&r| r).count() != 1 {
        bail!("Verify exactly one of unit, process or port.");
    }

    if requested[0] {
        let name = verify_unit_name(unit)?;
        normalize_expectation("unit", expect)?;
        return guard_command(
            [
                format!("s={}", quote_shell(name)?),
                r#"printf '\nLINKR_VERIFY:subject=unit:%s\n' "$s""#.to_string(),
                r#"command -v systemctl >/dev/null 2>&1 || { printf 'LINKR_VERIFY:unsupported=no-systemctl\nLINKR_VERIFY:done\n'; exit 0; }"#.to_string(),
                // A non-zero exit still carries the properties systemd managed
                // to print, so the status must not discard that output.
                r#"o=$(systemctl show -p LoadState -p ActiveState -p SubState -p MainPID -p ExecMainStatus -- "$s" 2>/dev/null) || :"#.to_string(),
                r#"printf 'LINKR_VERIFY:unit-begin\n%s\nLINKR_VERIFY:unit-end\n' "$o""#.to_string(),
                r#"printf 'LINKR_VERIFY:done\n'"#.to_string(),
            ]
            .join("; "),
        );
    }

    if requested[1] {
        let pattern = verify_pattern(process)?;
        normalize_expectation("process", expect)?;
        return guard_command(
            [
                format!("s={}", quote_shell(pattern)?),
                r#"printf '\nLINKR_VERIFY:subject=process:%s\n' "$s""#.to_string(),
                r#"command -v pgrep >/dev/null 2>&1 || { printf 'LINKR_VERIFY:unsupported=no-pgrep\nLINKR_VERIFY:done\n'; exit 0; }"#.to_string(),
                // pgrep excludes itself, so the probe cannot match its own invocation.
                r#"printf 'LINKR_VERIFY:process-begin\n'"#.to_string(),
                format!(r#"pgrep -f "$s" 2>/dev/null | head -n {MAX_PROCESS_PIDS}"#),
                r#"printf '\nLINKR_VERIFY:process-end\n'"#.to_string(),
                r#"printf 'LINKR_VERIFY:done\n'"#.to_string(),
            ]
            .join("; "),
        );
    }

    let value = verify_port(port.unwrap())?;
    normalize_expectation("port", expect)?;
    guard_command(
        [
            format!(r#"printf '\nLINKR_VERIFY:subject=port:%s\n' '{value}'"#),
            "t=".to_string(),
            r#"if command -v ss >/dev/null 2>&1; then t=ss; elif command -v netstat >/dev/null 2>&1; then t=netstat; fi"#.to_string(),
            r#"[ -n "$t" ] || { printf 'LINKR_VERIFY:unsupported=no-listener-tool\nLINKR_VERIFY:done\n'; exit 0; }"#.to_string(),
            r#"printf 'LINKR_VERIFY:listener-tool=%s\n' "$t""#.to_string(),
            r#"printf 'LINKR_VERIFY:listeners-begin\n'"#.to_string(),
            format!(r#"$t -ltn 2>/dev/null | head -n {MAX_LISTENER_ROWS}"#),
            r#"printf '\nLINKR_VERIFY:listeners-end\n'"#.to_string(),
            r#"printf 'LINKR_VERIFY:done\n'"#.to_string(),
        ]
        .join("; "),
    )
}

fn find_marker<'a>(lines: &'a [String], pattern: &Regex) -> Option<regex::Captures<'a>> {
    lines.iter().find_map(|line| pattern.captures(line))
}

/// Collect the lines between a pair of block markers. An unterminated block is
/// returned with `closed: false`, which is how a table cut off mid-stream is
/// distinguished from an empty one.
struct Block {
    rows: Vec<String>,
    closed: bool,
    seen: bool,
}

fn block_lines(lines: &[String], begin: &str, end: &str) -> Block {
    let Some(start) = lines.iter().position(|line| line == begin) else {
        return Block {
            rows: Vec::new(),
            closed: false,
            seen: false,
        };
    };
    let stop = lines[start + 1..].iter().position(|line| line == end);
    let end_index = match stop {
        Some(offset) => start + 1 + offset,
        None => lines.len(),
    };
    let rows = lines[start + 1..end_index]
        .iter()
        .filter(|line| !line.is_empty())
        .cloned()
        .collect();
    Block {
        rows,
        closed: stop.is_some(),
        seen: true,
    }
}

fn evidence(lines: &[String], extra: &[String]) -> Vec<String> {
    let mut out: Vec<String> = lines
        .iter()
        .filter(|line| line.starts_with("LINKR_VERIFY:"))
        .cloned()
        .collect();
    out.extend(extra.iter().cloned());
    out.truncate(MAX_EVIDENCE);
    out
}

/// Expectations for [`parse_verify_result`].
#[derive(Debug, Clone, Default)]
pub struct FileExpectation {
    pub path: Option<String>,
    pub bytes: Option<i64>,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileVerify {
    pub status: Verdict,
    /// `missing` / `directory` / `other` / `unknown` / `file`.
    pub found: Option<String>,
    pub complete: bool,
    pub path: Option<String>,
    pub bytes: Option<u64>,
    pub sha256: String,
    pub sha256_unavailable: bool,
    pub expected_bytes: Option<u64>,
    pub expected_sha256: String,
    pub checks: [CheckState; 2],
    pub evidence: Vec<String>,
    pub reason: String,
}

impl FileVerify {
    pub fn check(&self, which: usize) -> CheckState {
        self.checks[which]
    }
}

/// Read back [`verify_file_command`] output.
///
/// Pass `path` to make the observation attributable, and `bytes` / `sha256`
/// to turn a measurement into a verification. Everything else is reported
/// either way.
pub fn parse_verify_result(text: &str, expected: &FileExpectation) -> Result<FileVerify> {
    let lines = marker_lines(text);
    let want_path = expected.path.clone();
    let want_bytes = normalize_expected_bytes(expected.bytes)?;
    let want_sha = normalize_expected_sha256(&expected.sha256)?;
    let asked = want_bytes.is_some() || !want_sha.is_empty();

    let mut result = FileVerify {
        status: Verdict::Indeterminate,
        found: None,
        complete: false,
        path: want_path.clone(),
        bytes: None,
        sha256: String::new(),
        sha256_unavailable: false,
        expected_bytes: want_bytes,
        expected_sha256: want_sha.clone(),
        checks: [CheckState::NotRequested, CheckState::NotRequested],
        evidence: Vec::new(),
        reason: String::new(),
    };
    let settle = |mut v: FileVerify, status: Verdict, reason: String, extra: &[String]| {
        v.status = status;
        v.reason = reason;
        v.evidence = evidence(&lines, extra);
        v
    };

    let done = lines.iter().any(|line| line == "LINKR_VERIFY:done");
    result.complete = done;

    /* The echo has to match before any finding is trusted: a window of the
     * shared journal can still hold an earlier verify of a different path. */
    let Some(path_marker) = find_marker(&lines, re_path()) else {
        let reason = if done {
            "The output does not name the path that was checked, so nothing can be attributed to this request.".to_string()
        } else {
            "No output named the path and no completion marker arrived: the check either never ran or its output was lost. Monitor the same execution again instead of sending a second verify.".to_string()
        };
        return Ok(settle(result, Verdict::Indeterminate, reason, &[]));
    };
    let echo = path_marker[1].to_string();
    if let Some(want) = &want_path {
        if &echo != want {
            return Ok(settle(
                result,
                Verdict::Indeterminate,
                format!("This output belongs to {echo}, not {want}."),
                &[],
            ));
        }
    }
    if !done {
        return Ok(settle(
            result,
            Verdict::Indeterminate,
            "The check did not finish: the command printed no completion marker. Monitor the same execution again instead of sending a second verify.".to_string(),
            &[],
        ));
    }

    if let Some((_name, label, content, text)) = FILE_FINDINGS.iter().find(|(name, _, _, _)| {
        let wanted = format!("LINKR_VERIFY:{name}");
        lines.iter().any(|line| line == &wanted)
    }) {
        result.found = Some((*label).to_string());
        if !asked {
            return Ok(settle(
                result,
                Verdict::Observed,
                format!("{text} No expectation was supplied, so this is a measurement, not a verification."),
                &[],
            ));
        }
        // "The path is not what I expected" still refutes specific content,
        // but a refusal to read claims nothing: the bytes were never seen.
        if *content {
            return Ok(settle(
                result,
                Verdict::Mismatch,
                format!("{text} The expected content therefore cannot be present."),
                &[],
            ));
        }
        return Ok(settle(
            result,
            Verdict::Indeterminate,
            text.to_string(),
            &[],
        ));
    }

    if !lines.iter().any(|line| line == "LINKR_VERIFY:file") {
        return Ok(settle(
            result,
            Verdict::Indeterminate,
            "The output reports no file state, so the target's answer is unusable.".to_string(),
            &[],
        ));
    }
    let Some(bytes_marker) = find_marker(&lines, re_bytes()) else {
        return Ok(settle(
            result,
            Verdict::Indeterminate,
            "The target reported a regular file but no size, so its content cannot be compared."
                .to_string(),
            &[],
        ));
    };
    result.found = Some("file".to_string());
    // A 20+-digit `wc` output is corrupted, not a file size; `u64::MAX` can
    // never satisfy an expectation.
    result.bytes = Some(bytes_marker[1].parse().unwrap_or(u64::MAX));

    if let Some(sha_marker) = find_marker(&lines, re_sha()) {
        if &sha_marker[1] == "unavailable" {
            result.sha256_unavailable = true;
        } else {
            result.sha256 = sha_marker[1].to_string();
        }
    }

    if let Some(want) = want_bytes {
        result.checks[0] = if result.bytes == Some(want) {
            CheckState::Match
        } else {
            CheckState::Mismatch
        };
    }
    if !want_sha.is_empty() {
        result.checks[1] = if result.sha256_unavailable || result.sha256.is_empty() {
            CheckState::Unknown
        } else if result.sha256 == want_sha {
            CheckState::Match
        } else {
            CheckState::Mismatch
        };
    }

    if result.checks.contains(&CheckState::Mismatch) {
        let mut detail = Vec::new();
        if result.checks[0] == CheckState::Mismatch {
            detail.push(format!(
                "the target holds {} bytes, not {}",
                result.bytes.unwrap_or(0),
                want_bytes.unwrap_or(0)
            ));
        }
        if result.checks[1] == CheckState::Mismatch {
            detail.push(format!(
                "the target's sha256 is {}, not {want_sha}",
                result.sha256
            ));
        }
        return Ok(settle(
            result,
            Verdict::Mismatch,
            format!(
                "The target file is not the expected content: {}.",
                detail.join("; ")
            ),
            &[],
        ));
    }
    if result.checks[1] == CheckState::Unknown {
        let reason = if result.sha256_unavailable {
            "The target has neither sha256sum nor shasum, so the expected digest could not be checked."
        } else {
            "The output carries no digest, so the expected digest could not be checked."
        };
        return Ok(settle(
            result,
            Verdict::Indeterminate,
            reason.to_string(),
            &[],
        ));
    }
    if !asked {
        let reason = if result.sha256_unavailable {
            "The target file was measured, but the target has neither sha256sum nor shasum, so no digest was produced. No expectation was supplied, so this is a measurement, not a verification."
        } else {
            "The target file was measured. No expectation was supplied, so this is a measurement, not a verification."
        };
        return Ok(settle(result, Verdict::Observed, reason.to_string(), &[]));
    }
    Ok(settle(
        result,
        Verdict::Match,
        "The target file matches every expectation that was supplied.".to_string(),
        &[],
    ))
}

/// Expectations for [`parse_service_result`].
#[derive(Debug, Clone, Default)]
pub struct ServiceExpectation {
    pub unit: String,
    pub process: String,
    pub port: Option<i64>,
    pub expect: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ServiceVerify {
    pub status: Verdict,
    pub kind: ServiceKind,
    pub subject: String,
    pub expected: String,
    pub complete: bool,
    pub unsupported: String,
    /// Unit: `{load, active, sub, mainPid, exitStatus}`. Process: `{pids,
    /// count, truncated}`. Port: `{tool, listening, rows, truncated}`.
    pub observed: Value,
    pub evidence: Vec<String>,
    pub reason: String,
}

/// Read back [`verify_service_command`] output. The subject is echoed by the
/// command and checked here so a stale window cannot answer for this request.
pub fn parse_service_result(text: &str, expected: &ServiceExpectation) -> Result<ServiceVerify> {
    let lines = marker_lines(text);
    let kind = if !expected.unit.is_empty() {
        ServiceKind::Unit
    } else if !expected.process.is_empty() {
        ServiceKind::Process
    } else if expected.port.is_some() {
        ServiceKind::Port
    } else {
        bail!("Name exactly one of unit, process or port in the expectation.");
    };
    let subject = match kind {
        ServiceKind::Port => verify_port(expected.port.unwrap())?.to_string(),
        ServiceKind::Unit => expected.unit.clone(),
        ServiceKind::Process => expected.process.clone(),
    };
    let expectation = normalize_expectation(kind.as_str(), &expected.expect)?;

    let mut result = ServiceVerify {
        status: Verdict::Indeterminate,
        kind,
        subject: subject.clone(),
        expected: expectation.clone(),
        complete: false,
        unsupported: String::new(),
        observed: Value::Null,
        evidence: Vec::new(),
        reason: String::new(),
    };
    let settle = |mut v: ServiceVerify, status: Verdict, reason: String, extra: &[String]| {
        v.status = status;
        v.reason = reason;
        v.evidence = evidence(&lines, extra);
        v
    };

    result.complete = lines.iter().any(|line| line == "LINKR_VERIFY:done");
    let Some(subject_marker) = find_marker(&lines, re_subject()) else {
        let reason = if result.complete {
            "The output does not name the claim that was checked, so nothing can be attributed to this request.".to_string()
        } else {
            "No output named the claim and no completion marker arrived: the check either never ran or its output was lost. Monitor the same execution again instead of sending a second verify.".to_string()
        };
        return Ok(settle(result, Verdict::Indeterminate, reason, &[]));
    };
    let echo_kind: &str = &subject_marker[1];
    let echo_subject: &str = &subject_marker[2];
    if echo_kind != kind.as_str() || echo_subject != subject.as_str() {
        return Ok(settle(
            result,
            Verdict::Indeterminate,
            format!(
                "This output belongs to {echo_kind} {echo_subject}, not {} {}.",
                kind.as_str(),
                subject
            ),
            &[],
        ));
    }
    if let Some(unsupported) = find_marker(&lines, re_unsupported()) {
        let code = unsupported[1].to_string();
        let reason = UNSUPPORTED_TEXT
            .iter()
            .find(|(name, _)| *name == code)
            .map(|(_, text)| (*text).to_string())
            .unwrap_or_else(|| format!("The target cannot report this: {code}."));
        result.unsupported = code;
        return Ok(settle(result, Verdict::Indeterminate, reason, &[]));
    }
    if !result.complete {
        return Ok(settle(
            result,
            Verdict::Indeterminate,
            "The check did not finish: the command printed no completion marker. Monitor the same execution again instead of sending a second verify.".to_string(),
            &[],
        ));
    }

    if kind == ServiceKind::Unit {
        let block = block_lines(&lines, "LINKR_VERIFY:unit-begin", "LINKR_VERIFY:unit-end");
        if !block.seen || !block.closed {
            return Ok(settle(
                result,
                Verdict::Indeterminate,
                "The target's unit state arrived incomplete, so its state cannot be read."
                    .to_string(),
                &block.rows,
            ));
        }
        let mut settings: BTreeMap<String, String> = BTreeMap::new();
        for row in &block.rows {
            if let Some(caps) = re_setting().captures(row) {
                settings.insert(caps[1].to_string(), caps[2].to_string());
            }
        }
        let get = |key: &str| settings.get(key).cloned().unwrap_or_default();
        if get("ActiveState").is_empty() {
            return Ok(settle(
                result,
                Verdict::Indeterminate,
                "The target reported no unit state, so there is nothing to compare.".to_string(),
                &block.rows,
            ));
        }
        /* A unit that does not exist reports no ActiveState of its own;
         * treating "not-found" as "inactive" would turn a typo into a
         * confident all-clear. */
        if get("LoadState") == "not-found" {
            result.observed = json!({ "load": get("LoadState") });
            return Ok(settle(
                result,
                Verdict::Indeterminate,
                format!("{subject} is not a unit on this target, which is not the same as a stopped service."),
                &block.rows,
            ));
        }
        let active = get("ActiveState");
        let sub = get("SubState");
        let main_pid: u64 = get("MainPID").parse().unwrap_or(0);
        let exit_status: i64 = get("ExecMainStatus").parse().unwrap_or(0);
        result.observed = json!({
            "load": get("LoadState"),
            "active": active,
            "sub": sub,
            "mainPid": main_pid,
            "exitStatus": exit_status,
        });
        let agrees = active == expectation;
        let mut detail = format!("ActiveState={active}");
        if !sub.is_empty() {
            detail.push_str(&format!(", SubState={sub}"));
        }
        if exit_status != 0 {
            detail.push_str(&format!(", exit status {exit_status}"));
        }
        let reason = if agrees {
            format!("{subject} is {expectation} ({detail}).")
        } else {
            format!("{subject} is not {expectation}: {detail}.")
        };
        return Ok(settle(
            result,
            if agrees {
                Verdict::Match
            } else {
                Verdict::Mismatch
            },
            reason,
            &block.rows,
        ));
    }

    if kind == ServiceKind::Process {
        let block = block_lines(
            &lines,
            "LINKR_VERIFY:process-begin",
            "LINKR_VERIFY:process-end",
        );
        if !block.seen || !block.closed {
            return Ok(settle(
                result,
                Verdict::Indeterminate,
                "The target's process list arrived incomplete, so it cannot be read.".to_string(),
                &block.rows,
            ));
        }
        let pids: Vec<u64> = block
            .rows
            .iter()
            .filter(|row| re_pid().is_match(row))
            .filter_map(|row| row.parse().ok())
            .collect();
        let count = pids.len();
        let truncated = count >= MAX_PROCESS_PIDS;
        result.observed = json!({ "pids": pids, "count": count, "truncated": truncated });
        let running = count > 0;
        let agrees = running == (expectation == "running");
        let count_text = format!(
            "matched {count}{} process{}",
            if truncated { " or more" } else { "" },
            if count == 1 { "" } else { "es" }
        );
        let reason =
            if agrees {
                format!(
                    "The target {} matching process ({count_text}).",
                    if expectation == "running" {
                        "has"
                    } else {
                        "has no"
                    }
                )
            } else {
                format!(
                "The target {} matching process, but {expectation} was expected ({count_text}).",
                if expectation == "running" { "has no" } else { "still has" }
            )
            };
        let extra: Vec<String> = pids.iter().take(10).map(|pid| pid.to_string()).collect();
        return Ok(settle(
            result,
            if agrees {
                Verdict::Match
            } else {
                Verdict::Mismatch
            },
            reason,
            &extra,
        ));
    }

    let tool = find_marker(&lines, re_listener_tool()).map(|caps| caps[1].to_string());
    let block = block_lines(
        &lines,
        "LINKR_VERIFY:listeners-begin",
        "LINKR_VERIFY:listeners-end",
    );
    if !block.seen || !block.closed {
        return Ok(settle(
            result,
            Verdict::Indeterminate,
            "The target's listening-socket table arrived incomplete, so it cannot be read."
                .to_string(),
            &block.rows,
        ));
    }
    let Some(tool) = tool else {
        return Ok(settle(
            result,
            Verdict::Indeterminate,
            "The output does not say which tool produced the socket table, so its rows cannot be interpreted.".to_string(),
            &block.rows,
        ));
    };
    /* Only rows whose local address ends in this port count. A listening
     * socket's peer column is always unspecified, so a port number in a row
     * belongs to the local side. */
    let rows: Vec<String> = block
        .rows
        .iter()
        .filter(|row| {
            row.split_whitespace().any(|field| {
                re_listen_address()
                    .captures(field)
                    .and_then(|caps| caps[1].parse::<u64>().ok())
                    .map(|port| port == subject.parse::<u64>().unwrap_or(0))
                    .unwrap_or(false)
            })
        })
        .cloned()
        .collect();
    let listening = !rows.is_empty();
    let truncated = block.rows.len() >= MAX_LISTENER_ROWS;
    let shown: Vec<String> = rows.iter().take(8).cloned().collect();
    result.observed = json!({
        "tool": tool,
        "listening": listening,
        "rows": shown,
        "truncated": truncated,
    });
    /* A truncated table proves nothing by absence: "no row mentions the port"
     * may only mean the row was never printed. A truncated table where the port
     * WAS found still proves presence, so only the negative case is refused. */
    if !listening && truncated {
        let tail: Vec<String> = block
            .rows
            .iter()
            .skip(block.rows.len().saturating_sub(8))
            .cloned()
            .collect();
        return Ok(settle(
            result,
            Verdict::Indeterminate,
            format!("{tool} printed the first {MAX_LISTENER_ROWS} listening sockets and stopped, so this table cannot show that nothing listens on port {subject}. Report the uncertainty instead of reporting the port as closed."),
            &tail,
        ));
    }
    let agrees = listening == (expectation == "listening");
    let reason = if listening {
        format!(
            "{tool} reports something listening on port {subject}: {}",
            rows[0].trim()
        )
    } else {
        format!("{tool} reports nothing listening on port {subject}.")
    };
    Ok(settle(
        result,
        if agrees {
            Verdict::Match
        } else {
            Verdict::Mismatch
        },
        reason,
        &shown,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn js() -> String {
        std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../web/target_verify.js"
        ))
        .expect("read web/target_verify.js")
    }

    /// The contract test: markers, labels and reason texts must exist byte for
    /// byte in the web client's module.
    #[test]
    fn markers_match_web_client_byte_for_byte() {
        let src = js();
        for marker in [
            "LINKR_VERIFY:path=",
            "LINKR_VERIFY:file",
            "LINKR_VERIFY:bytes=",
            "LINKR_VERIFY:sha256=",
            "LINKR_VERIFY:sha256=unavailable",
            "LINKR_VERIFY:missing",
            "LINKR_VERIFY:directory",
            "LINKR_VERIFY:not-regular",
            "LINKR_VERIFY:not-absolute",
            "LINKR_VERIFY:denied",
            "LINKR_VERIFY:unreadable",
            "LINKR_VERIFY:done",
            "LINKR_VERIFY:subject=",
            "LINKR_VERIFY:subject=unit:",
            "LINKR_VERIFY:subject=process:",
            "LINKR_VERIFY:subject=port:",
            "LINKR_VERIFY:unit-begin",
            "LINKR_VERIFY:unit-end",
            "LINKR_VERIFY:process-begin",
            "LINKR_VERIFY:process-end",
            "LINKR_VERIFY:listeners-begin",
            "LINKR_VERIFY:listeners-end",
            "LINKR_VERIFY:listener-tool=",
            "LINKR_VERIFY:unsupported=no-systemctl",
            "LINKR_VERIFY:unsupported=no-pgrep",
            "LINKR_VERIFY:unsupported=no-listener-tool",
        ] {
            assert!(src.contains(marker), "web/target_verify.js lost {marker:?}");
        }
        for label in ["match", "mismatch", "indeterminate", "observed"] {
            assert!(
                src.contains(&format!("\"{label}\"")),
                "missing label {label}"
            );
            assert_eq!(
                Verdict::Indeterminate.as_str(),
                "indeterminate",
                "labels must stay byte-identical"
            );
        }
        // Our emitted commands carry the same text, un-escaped once.
        let cmd = verify_file_command("/etc/hosts", true).unwrap();
        assert!(cmd.contains(r"printf '\nLINKR_VERIFY:path=%s\n'"));
        assert!(cmd.contains(r"printf 'LINKR_VERIFY:file\nLINKR_VERIFY:bytes=%s\n'"));
        assert!(src.contains(r"printf '\\nLINKR_VERIFY:path=%s\\n'"));
        assert!(cmd.contains(r"printf 'LINKR_VERIFY:done\n'"));
        assert!(src.contains(r"printf 'LINKR_VERIFY:done\\n'"));
        // Every reason string the parsers can produce is in the JS too.
        for text in [
            "The target file matches every expectation that was supplied.",
            "The output reports no file state, so the target's answer is unusable.",
            "The target reported a regular file but no size, so its content cannot be compared.",
            "The check did not finish: the command printed no completion marker. Monitor the same execution again instead of sending a second verify.",
            "The target path does not exist.",
            "The target has no systemctl, so it cannot report service state.",
            "A unit name must start with a letter or digit and contain only letters, digits, @ . _ : -",
            "Verify exactly one of unit, process or port.",
        ] {
            assert!(src.contains(text), "web/target_verify.js lost reason {text:?}");
        }
    }

    #[test]
    fn command_validation_messages() {
        assert_eq!(
            verify_file_command("", false).unwrap_err().to_string(),
            "Target path must be a non-empty string."
        );
        assert_eq!(
            verify_file_command(&format!("/{}", "a".repeat(201)), false)
                .unwrap_err()
                .to_string(),
            "Target path must not exceed 200 characters."
        );
        assert_eq!(
            verify_service_command("", "", None, "")
                .unwrap_err()
                .to_string(),
            "Verify exactly one of unit, process or port."
        );
        assert_eq!(
            verify_service_command("a b", "", None, "").unwrap_err().to_string(),
            "A unit name must start with a letter or digit and contain only letters, digits, @ . _ : -"
        );
        assert_eq!(
            verify_service_command("-sshd", "", None, "").unwrap_err().to_string(),
            "A unit name must start with a letter or digit and contain only letters, digits, @ . _ : -"
        );
        assert_eq!(
            verify_service_command("", "p", None, "sleeping")
                .unwrap_err()
                .to_string(),
            "Expected process state must be one of running, absent."
        );
        assert_eq!(
            verify_service_command("", "", Some(0), "")
                .unwrap_err()
                .to_string(),
            "A port must be an integer between 1 and 65535."
        );
        assert_eq!(
            verify_service_command("ssh.service", "", None, "bogus")
                .unwrap_err()
                .to_string(),
            "Expected unit state must be one of active, inactive, failed."
        );
        assert_eq!(
            normalize_expectation("nonsense", "")
                .unwrap_err()
                .to_string(),
            "Unknown service kind: nonsense"
        );
        assert_eq!(
            normalize_expected_sha256("abc").unwrap_err().to_string(),
            "An expected sha256 must be 64 hexadecimal characters."
        );
        assert_eq!(
            normalize_expected_bytes(Some(-1)).unwrap_err().to_string(),
            "An expected byte count must be a non-negative integer."
        );
        assert_eq!(
            verify_pattern("").unwrap_err().to_string(),
            "A process pattern must be a non-empty string."
        );
        assert_eq!(
            verify_file_command("/a\nb", false).unwrap_err().to_string(),
            "Target path must not contain control characters."
        );
    }

    #[test]
    fn command_shapes() {
        let file = verify_file_command("/etc/hosts", false).unwrap();
        assert_eq!(
            file,
            [
                "p='/etc/hosts'",
                r#"printf '\nLINKR_VERIFY:path=%s\n' "$p""#,
                r#"case "$p" in /*) ;; *) printf 'LINKR_VERIFY:not-absolute\nLINKR_VERIFY:done\n'; exit 0 ;; esac"#,
                r#"[ -e "$p" ] || { printf 'LINKR_VERIFY:missing\nLINKR_VERIFY:done\n'; exit 0; }"#,
                r#"if [ -d "$p" ]; then printf 'LINKR_VERIFY:directory\nLINKR_VERIFY:done\n'; exit 0; fi"#,
                r#"[ -f "$p" ] || { printf 'LINKR_VERIFY:not-regular\nLINKR_VERIFY:done\n'; exit 0; }"#,
                r#"[ -r "$p" ] || { printf 'LINKR_VERIFY:denied\nLINKR_VERIFY:done\n'; exit 0; }"#,
                r#"n=$(wc -c < "$p" 2>/dev/null) || n="#,
                r#"[ -n "$n" ] || { printf 'LINKR_VERIFY:unreadable\nLINKR_VERIFY:done\n'; exit 0; }"#,
                r#"printf 'LINKR_VERIFY:file\nLINKR_VERIFY:bytes=%s\n' "$n""#,
                r#"printf 'LINKR_VERIFY:done\n'"#,
            ]
            .join("; ")
        );
        assert!(!verify_file_command("/etc/hosts", false)
            .unwrap()
            .contains("sha256sum"));
        assert!(verify_file_command("/etc/hosts", true)
            .unwrap()
            .contains("sha256sum"));

        let unit = verify_service_command("ssh.service", "", None, "").unwrap();
        assert!(unit.starts_with("s='ssh.service'; printf '\\nLINKR_VERIFY:subject=unit:%s\\n'"));
        assert!(unit.contains("systemctl show -p LoadState"));

        let process = verify_service_command("", "nginx", None, "absent").unwrap();
        assert!(process.contains(&format!(
            "pgrep -f \"$s\" 2>/dev/null | head -n {MAX_PROCESS_PIDS}"
        )));

        let port = verify_service_command("", "", Some(8080), "listening").unwrap();
        assert_eq!(
            port,
            [
                r#"printf '\nLINKR_VERIFY:subject=port:%s\n' '8080'"#,
                "t=",
                r#"if command -v ss >/dev/null 2>&1; then t=ss; elif command -v netstat >/dev/null 2>&1; then t=netstat; fi"#,
                r#"[ -n "$t" ] || { printf 'LINKR_VERIFY:unsupported=no-listener-tool\nLINKR_VERIFY:done\n'; exit 0; }"#,
                r#"printf 'LINKR_VERIFY:listener-tool=%s\n' "$t""#,
                r#"printf 'LINKR_VERIFY:listeners-begin\n'"#,
                r#"$t -ltn 2>/dev/null | head -n 60"#,
                r#"printf '\nLINKR_VERIFY:listeners-end\n'"#,
                r#"printf 'LINKR_VERIFY:done\n'"#,
            ]
            .join("; ")
        );
    }

    fn file_output(extra: &[&str]) -> String {
        let mut text = String::from("LINKR_VERIFY:path=/etc/hosts\n");
        for line in extra {
            text.push_str(line);
            text.push('\n');
        }
        text.push_str("LINKR_VERIFY:done\n");
        text
    }

    #[test]
    fn file_verdict_match_mismatch_observed() {
        let sha = "c".repeat(64);
        let expectation = FileExpectation {
            path: Some("/etc/hosts".into()),
            bytes: Some(128),
            sha256: sha.clone(),
        };
        let text = file_output(&[
            "LINKR_VERIFY:file",
            "LINKR_VERIFY:bytes=128",
            &format!("LINKR_VERIFY:sha256={sha}"),
        ]);
        let result = parse_verify_result(&text, &expectation).unwrap();
        assert_eq!(result.status, Verdict::Match);
        assert_eq!(result.found.as_deref(), Some("file"));
        assert_eq!(result.bytes, Some(128));
        assert_eq!(result.checks, [CheckState::Match, CheckState::Match]);
        assert!(result.reason.contains("matches every expectation"));
        assert!(!result.evidence.is_empty());

        let text = file_output(&["LINKR_VERIFY:file", "LINKR_VERIFY:bytes=7"]);
        let result = parse_verify_result(&text, &expectation).unwrap();
        assert_eq!(result.status, Verdict::Mismatch);
        assert_eq!(
            result.reason,
            "The target file is not the expected content: the target holds 7 bytes, not 128."
        );

        // No expectation at all: a measurement, never a verification.
        let text = file_output(&["LINKR_VERIFY:file", "LINKR_VERIFY:bytes=7"]);
        let result = parse_verify_result(&text, &FileExpectation::default()).unwrap();
        assert_eq!(result.status, Verdict::Observed);
        assert!(result
            .reason
            .ends_with("this is a measurement, not a verification."));
        assert_eq!(
            result.checks,
            [CheckState::NotRequested, CheckState::NotRequested]
        );
    }

    #[test]
    fn file_verdict_findings() {
        let expectation = FileExpectation {
            path: Some("/etc/hosts".into()),
            bytes: Some(4),
            sha256: String::new(),
        };
        let text = file_output(&["LINKR_VERIFY:missing"]);
        let result = parse_verify_result(&text, &expectation).unwrap();
        assert_eq!(result.status, Verdict::Mismatch);
        assert_eq!(result.found.as_deref(), Some("missing"));
        assert_eq!(
            result.reason,
            "The target path does not exist. The expected content therefore cannot be present."
        );

        // A refusal to read claims nothing about content.
        let text = file_output(&["LINKR_VERIFY:denied"]);
        let result = parse_verify_result(&text, &expectation).unwrap();
        assert_eq!(result.status, Verdict::Indeterminate);
        assert_eq!(result.found.as_deref(), Some("unknown"));
        assert_eq!(
            result.reason,
            "The target file exists but is not readable, so its content could not be checked."
        );

        let text = file_output(&["LINKR_VERIFY:not-regular"]);
        let result = parse_verify_result(&text, &FileExpectation::default()).unwrap();
        assert_eq!(result.found.as_deref(), Some("other"));
        assert_eq!(result.status, Verdict::Observed);
    }

    #[test]
    fn file_verdict_unusable_answers() {
        let expectation = FileExpectation {
            path: Some("/etc/hosts".into()),
            ..FileExpectation::default()
        };
        // No path echo: nothing can be attributed.
        let result = parse_verify_result("LINKR_VERIFY:done\n", &expectation).unwrap();
        assert_eq!(result.status, Verdict::Indeterminate);
        assert!(result
            .reason
            .starts_with("The output does not name the path"));
        let result = parse_verify_result("silent\n", &expectation).unwrap();
        assert!(result.reason.starts_with("No output named the path"));

        // Stale window: another path's answer.
        let text = file_output(&["LINKR_VERIFY:file", "LINKR_VERIFY:bytes=3"]);
        let result = parse_verify_result(
            &text,
            &FileExpectation {
                path: Some("/etc/shadow".into()),
                ..FileExpectation::default()
            },
        )
        .unwrap();
        assert_eq!(
            result.reason,
            "This output belongs to /etc/hosts, not /etc/shadow."
        );

        // No completion marker.
        let text = "LINKR_VERIFY:path=/etc/hosts\nLINKR_VERIFY:file\n";
        let result = parse_verify_result(text, &expectation).unwrap();
        assert_eq!(result.status, Verdict::Indeterminate);
        assert!(!result.complete);
        assert!(result.reason.starts_with("The check did not finish"));

        // Missing digest when one was asked for.
        let text = file_output(&["LINKR_VERIFY:file", "LINKR_VERIFY:bytes=4"]);
        let result = parse_verify_result(
            &text,
            &FileExpectation {
                path: Some("/etc/hosts".into()),
                sha256: "d".repeat(64),
                ..FileExpectation::default()
            },
        )
        .unwrap();
        assert_eq!(result.status, Verdict::Indeterminate);
        assert_eq!(
            result.reason,
            "The output carries no digest, so the expected digest could not be checked."
        );

        let text = file_output(&[
            "LINKR_VERIFY:file",
            "LINKR_VERIFY:bytes=4",
            "LINKR_VERIFY:sha256=unavailable",
        ]);
        let result = parse_verify_result(
            &text,
            &FileExpectation {
                path: Some("/etc/hosts".into()),
                sha256: "d".repeat(64),
                ..FileExpectation::default()
            },
        )
        .unwrap();
        assert_eq!(result.status, Verdict::Indeterminate);
        assert!(result.sha256_unavailable);
        assert_eq!(
            result.reason,
            "The target has neither sha256sum nor shasum, so the expected digest could not be checked."
        );

        // Digest mismatch beats everything, even without a byte count.
        let wrong_sha = format!("LINKR_VERIFY:sha256=f{}", "0".repeat(63));
        let text = file_output(&["LINKR_VERIFY:file", "LINKR_VERIFY:bytes=4", &wrong_sha]);
        let result = parse_verify_result(
            &text,
            &FileExpectation {
                path: Some("/etc/hosts".into()),
                bytes: Some(4),
                sha256: "e".repeat(64),
            },
        )
        .unwrap();
        assert_eq!(result.status, Verdict::Mismatch);
        assert_eq!(
            result.reason,
            format!(
                "The target file is not the expected content: the target's sha256 is {}, not {}.",
                wrong_sha.trim_start_matches("LINKR_VERIFY:sha256="),
                "e".repeat(64)
            )
        );
    }

    #[test]
    fn service_unit_verdicts() {
        let expectation = ServiceExpectation {
            unit: "ssh.service".into(),
            expect: "active".into(),
            ..ServiceExpectation::default()
        };
        let text = "LINKR_VERIFY:subject=unit:ssh.service\n\
                    LINKR_VERIFY:unit-begin\n\
                    LoadState=loaded\n\
                    ActiveState=active\n\
                    SubState=running\n\
                    MainPID=1234\n\
                    ExecMainStatus=0\n\
                    LINKR_VERIFY:unit-end\n\
                    LINKR_VERIFY:done\n";
        let result = parse_service_result(text, &expectation).unwrap();
        assert_eq!(result.status, Verdict::Match);
        assert_eq!(result.kind, ServiceKind::Unit);
        assert_eq!(result.subject, "ssh.service");
        assert_eq!(result.observed["active"], "active");
        assert_eq!(result.observed["mainPid"], 1234);
        assert_eq!(
            result.reason,
            "ssh.service is active (ActiveState=active, SubState=running)."
        );
        assert!(result
            .evidence
            .iter()
            .any(|e| e.contains("ActiveState=active")));

        let text = "LINKR_VERIFY:subject=unit:ssh.service\n\
                    LINKR_VERIFY:unit-begin\n\
                    LoadState=loaded\n\
                    ActiveState=inactive\n\
                    SubState=dead\n\
                    ExecMainStatus=2\n\
                    LINKR_VERIFY:unit-end\n\
                    LINKR_VERIFY:done\n";
        let result = parse_service_result(text, &expectation).unwrap();
        assert_eq!(result.status, Verdict::Mismatch);
        assert_eq!(
            result.reason,
            "ssh.service is not active: ActiveState=inactive, SubState=dead, exit status 2."
        );

        // A typo is not a stopped service (systemd still answers ActiveState).
        let text = "LINKR_VERIFY:subject=unit:ssh.service\n\
                    LINKR_VERIFY:unit-begin\n\
                    LoadState=not-found\n\
                    ActiveState=inactive\n\
                    SubState=dead\n\
                    LINKR_VERIFY:unit-end\n\
                    LINKR_VERIFY:done\n";
        let result = parse_service_result(text, &expectation).unwrap();
        assert_eq!(result.status, Verdict::Indeterminate);
        assert_eq!(
            result.reason,
            "ssh.service is not a unit on this target, which is not the same as a stopped service."
        );
        assert_eq!(result.observed["load"], "not-found");

        // No systemctl on the target.
        let text = "LINKR_VERIFY:subject=unit:ssh.service\n\
                    LINKR_VERIFY:unsupported=no-systemctl\n\
                    LINKR_VERIFY:done\n";
        let result = parse_service_result(text, &expectation).unwrap();
        assert_eq!(result.status, Verdict::Indeterminate);
        assert_eq!(result.unsupported, "no-systemctl");
        assert_eq!(
            result.reason,
            "The target has no systemctl, so it cannot report service state."
        );

        // Cut off mid-stream: `done` arrived but the block never closed.
        let text = "LINKR_VERIFY:subject=unit:ssh.service\nLINKR_VERIFY:unit-begin\nActiveState=active\nLINKR_VERIFY:done\n";
        let result = parse_service_result(text, &expectation).unwrap();
        assert_eq!(result.status, Verdict::Indeterminate);
        assert_eq!(
            result.reason,
            "The target's unit state arrived incomplete, so its state cannot be read."
        );

        // No `done` at all is the other indeterminate: the command never finished.
        let text =
            "LINKR_VERIFY:subject=unit:ssh.service\nLINKR_VERIFY:unit-begin\nActiveState=active\n";
        let result = parse_service_result(text, &expectation).unwrap();
        assert_eq!(
            result.reason,
            "The check did not finish: the command printed no completion marker. Monitor the same execution again instead of sending a second verify."
        );

        // Wrong subject: attribution refuses the answer.
        let text = "LINKR_VERIFY:subject=unit:other.service\nLINKR_VERIFY:done\n";
        let result = parse_service_result(text, &expectation).unwrap();
        assert_eq!(
            result.reason,
            "This output belongs to unit other.service, not unit ssh.service."
        );

        assert_eq!(
            parse_service_result("LINKR_VERIFY:done\n", &ServiceExpectation::default())
                .unwrap_err()
                .to_string(),
            "Name exactly one of unit, process or port in the expectation."
        );
    }

    #[test]
    fn service_process_verdicts() {
        let expectation = ServiceExpectation {
            process: "nginx".into(),
            expect: "running".into(),
            ..ServiceExpectation::default()
        };
        let text = "LINKR_VERIFY:subject=process:nginx\n\
                    LINKR_VERIFY:process-begin\n\
                    4242\n\
                    LINKR_VERIFY:process-end\n\
                    LINKR_VERIFY:done\n";
        let result = parse_service_result(text, &expectation).unwrap();
        assert_eq!(result.status, Verdict::Match);
        assert_eq!(
            result.reason,
            "The target has matching process (matched 1 process)."
        );
        assert_eq!(result.observed["count"], 1);

        let text = "LINKR_VERIFY:subject=process:nginx\n\
                    LINKR_VERIFY:process-begin\n\
                    LINKR_VERIFY:process-end\n\
                    LINKR_VERIFY:done\n";
        let result = parse_service_result(text, &expectation).unwrap();
        assert_eq!(result.status, Verdict::Mismatch);
        assert_eq!(
            result.reason,
            "The target has no matching process, but running was expected (matched 0 processes)."
        );

        let expectation = ServiceExpectation {
            process: "nginx".into(),
            expect: "absent".into(),
            ..ServiceExpectation::default()
        };
        let result = parse_service_result(
            "LINKR_VERIFY:subject=process:nginx\n\
             LINKR_VERIFY:process-begin\n\
             4242\n\
             LINKR_VERIFY:process-end\n\
             LINKR_VERIFY:done\n",
            &expectation,
        )
        .unwrap();
        assert_eq!(result.status, Verdict::Mismatch);
        assert_eq!(
            result.reason,
            "The target still has matching process, but absent was expected (matched 1 process)."
        );

        let text = "LINKR_VERIFY:subject=process:nginx\n\
                    LINKR_VERIFY:unsupported=no-pgrep\n\
                    LINKR_VERIFY:done\n";
        let result = parse_service_result(text, &expectation).unwrap();
        assert_eq!(
            result.reason,
            "The target has no pgrep, so it cannot report matching processes."
        );
    }

    #[test]
    fn service_port_verdicts() {
        let expectation = ServiceExpectation {
            port: Some(8080),
            expect: "listening".into(),
            ..ServiceExpectation::default()
        };
        let text = "LINKR_VERIFY:subject=port:8080\n\
                    LINKR_VERIFY:listener-tool=ss\n\
                    LINKR_VERIFY:listeners-begin\n\
                    State  Recv-Q Send-Q Local Address:Port Peer Address:Port\n\
                    LISTEN 0      4096         0.0.0.0:8080     0.0.0.0:*\n\
                    LINKR_VERIFY:listeners-end\n\
                    LINKR_VERIFY:done\n";
        let result = parse_service_result(text, &expectation).unwrap();
        assert_eq!(result.status, Verdict::Match);
        assert!(result
            .reason
            .starts_with("ss reports something listening on port 8080:"));
        assert_eq!(result.observed["tool"], "ss");
        assert_eq!(result.observed["listening"], true);

        let text = "LINKR_VERIFY:subject=port:8080\n\
                    LINKR_VERIFY:listener-tool=netstat\n\
                    LINKR_VERIFY:listeners-begin\n\
                    tcp 0 0 127.0.0.1:22 0.0.0.0:*\n\
                    LINKR_VERIFY:listeners-end\n\
                    LINKR_VERIFY:done\n";
        let result = parse_service_result(text, &expectation).unwrap();
        assert_eq!(result.status, Verdict::Mismatch);
        assert_eq!(
            result.reason,
            "netstat reports nothing listening on port 8080."
        );

        // A truncated table cannot prove absence.
        let mut rows = String::new();
        rows.push_str("LINKR_VERIFY:subject=port:8080\n");
        rows.push_str("LINKR_VERIFY:listener-tool=ss\n");
        rows.push_str("LINKR_VERIFY:listeners-begin\n");
        for i in 0..MAX_LISTENER_ROWS {
            rows.push_str(&format!("LISTEN 0 4096 0.0.0.0:{} 0.0.0.0:*\n", 10000 + i));
        }
        rows.push_str("LINKR_VERIFY:listeners-end\nLINKR_VERIFY:done\n");
        let result = parse_service_result(&rows, &expectation).unwrap();
        assert_eq!(result.status, Verdict::Indeterminate);
        assert_eq!(
            result.reason,
            "ss printed the first 60 listening sockets and stopped, so this table cannot show that nothing listens on port 8080. Report the uncertainty instead of reporting the port as closed."
        );

        let text = "LINKR_VERIFY:subject=port:8080\n\
                    LINKR_VERIFY:listeners-begin\n\
                    LINKR_VERIFY:listeners-end\n\
                    LINKR_VERIFY:done\n";
        let result = parse_service_result(text, &expectation).unwrap();
        assert_eq!(
            result.reason,
            "The output does not say which tool produced the socket table, so its rows cannot be interpreted."
        );

        let text = "LINKR_VERIFY:subject=port:8080\n\
                    LINKR_VERIFY:listener-tool=ss\n\
                    LINKR_VERIFY:unsupported=no-listener-tool\n\
                    LINKR_VERIFY:done\n";
        let result = parse_service_result(text, &expectation).unwrap();
        assert_eq!(result.unsupported, "no-listener-tool");

        let text = "LINKR_VERIFY:subject=port:8080\nLINKR_VERIFY:done\n";
        let result = parse_service_result(text, &expectation).unwrap();
        assert_eq!(result.status, Verdict::Indeterminate);
        assert_eq!(
            result.reason,
            "The target's listening-socket table arrived incomplete, so it cannot be read."
        );
    }
}
