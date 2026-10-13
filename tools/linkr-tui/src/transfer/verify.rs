//! Machine-decided verification of target state (`web/target_verify.js`).
//!
//! For a narrow, checkable set of claims the *application* compares what the
//! target reported against what the caller expected, returning
//! `match` / `mismatch` / `indeterminate` / `observed`. The verdict cannot be
//! produced by prose, cannot be produced by the echoed command, and does not
//! depend on reading output correctly.

use super::files::quote_shell;

pub const MAX_VERIFY_COMMAND_BYTES: usize = 4096;
pub const MAX_VERIFY_PATH: usize = 200;
pub const MAX_VERIFY_PATTERN: usize = 200;
pub const MAX_LISTENER_ROWS: usize = 60;
pub const MAX_PROCESS_PIDS: usize = 50;

/// The four verdicts a verify can return.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Match,
    Mismatch,
    Indeterminate,
    Observed,
}

// --- file verify ------------------------------------------------------------

fn verify_path(path: &str) -> Result<String, String> {
    if path.is_empty() {
        return Err("Target path must be a non-empty string.".to_string());
    }
    if path.len() > MAX_VERIFY_PATH {
        return Err(format!(
            "Target path must not exceed {MAX_VERIFY_PATH} characters."
        ));
    }
    if path.chars().any(|c| c.is_control()) {
        return Err("Target path must not contain control characters.".to_string());
    }
    Ok(path.to_string())
}

/// Normalize an expected sha256. Empty means "do not check".
pub fn normalize_expected_sha256(value: &str) -> Result<String, String> {
    if value.is_empty() {
        return Ok(String::new());
    }
    if value.len() != 64 || !value.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("An expected sha256 must be 64 hexadecimal characters.".to_string());
    }
    Ok(value.to_lowercase())
}

/// Normalize an expected byte count. `None` means "do not check".
pub fn normalize_expected_bytes(value: Option<u64>) -> Option<u64> {
    value
}

/// Build the `verifyFileCommand` for a path, with optional hashing.
pub fn verify_file_command(path: &str, hash: bool) -> Result<String, String> {
    let p = quote_shell(&verify_path(path)?)?;
    let mut parts = vec![
        format!("p={p}"),
        "printf '\\nLINKR_VERIFY:path=%s\\n' \"$p\"".to_string(),
        "case \"$p\" in /*) ;; *) printf 'LINKR_VERIFY:not-absolute\\nLINKR_VERIFY:done\\n'; exit 0 ;; esac".to_string(),
        "[ -e \"$p\" ] || { printf 'LINKR_VERIFY:missing\\nLINKR_VERIFY:done\\n'; exit 0; }".to_string(),
        "if [ -d \"$p\" ]; then printf 'LINKR_VERIFY:directory\\nLINKR_VERIFY:done\\n'; exit 0; fi".to_string(),
        "[ -f \"$p\" ] || { printf 'LINKR_VERIFY:not-regular\\nLINKR_VERIFY:done\\n'; exit 0; }".to_string(),
        "[ -r \"$p\" ] || { printf 'LINKR_VERIFY:denied\\nLINKR_VERIFY:done\\n'; exit 0; }".to_string(),
        "n=$(wc -c < \"$p\" 2>/dev/null) || n=".to_string(),
        "[ -n \"$n\" ] || { printf 'LINKR_VERIFY:unreadable\\nLINKR_VERIFY:done\\n'; exit 0; }".to_string(),
        "printf 'LINKR_VERIFY:file\\nLINKR_VERIFY:bytes=%s\\n' \"$n\"".to_string(),
    ];
    if hash {
        parts.push("if command -v sha256sum >/dev/null 2>&1; then h=$(sha256sum \"$p\" 2>/dev/null); elif command -v shasum >/dev/null 2>&1; then h=$(shasum -a 256 \"$p\" 2>/dev/null); else h=; fi".to_string());
        parts.push("h=${h%% *}".to_string());
        parts.push("[ -n \"$h\" ] || h=unavailable".to_string());
        parts.push("printf 'LINKR_VERIFY:sha256=%s\\n' \"$h\"".to_string());
    }
    parts.push("printf 'LINKR_VERIFY:done\\n'".to_string());
    let command = parts.join("; ");
    if command.len() > MAX_VERIFY_COMMAND_BYTES {
        return Err(format!(
            "Verify command would be {} characters, over the {MAX_VERIFY_COMMAND_BYTES}-character UART budget; shorten the path or pattern.",
            command.len()
        ));
    }
    Ok(command)
}

/// The result of parsing a file verify.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyFileResult {
    pub status: Verdict,
    pub found: String,
    pub complete: bool,
    pub path: Option<String>,
    pub bytes: Option<u64>,
    pub sha256: String,
    pub sha256_unavailable: bool,
    pub expected_bytes: Option<u64>,
    pub expected_sha256: String,
    pub reason: String,
}

/// Parse `verifyFileCommand` output. Pass `path` to attribute, and optional
/// `bytes`/`sha256` to turn a measurement into a verification.
pub fn parse_verify_file_result(
    text: &str,
    path: Option<&str>,
    bytes: Option<u64>,
    sha256: &str,
) -> VerifyFileResult {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = normalized.split('\n').map(|l| l.trim_end()).collect();

    let want_bytes = bytes;
    let want_sha = normalize_expected_sha256(sha256).unwrap_or_default();
    let asked = want_bytes.is_some() || !want_sha.is_empty();

    let mut result = VerifyFileResult {
        status: Verdict::Indeterminate,
        found: String::new(),
        complete: lines.contains(&"LINKR_VERIFY:done"),
        path: path.map(|p| p.to_string()),
        bytes: None,
        sha256: String::new(),
        sha256_unavailable: false,
        expected_bytes: want_bytes,
        expected_sha256: want_sha.clone(),
        reason: String::new(),
    };

    let path_marker = lines
        .iter()
        .find_map(|l| l.strip_prefix("LINKR_VERIFY:path="));
    let Some(path_marker) = path_marker else {
        result.reason = if result.complete {
            "The output does not name the path that was checked, so nothing can be attributed to this request.".to_string()
        } else {
            "No output named the path and no completion marker arrived: the check either never ran or its output was lost.".to_string()
        };
        return result;
    };
    if let Some(want) = path {
        if path_marker != want {
            result.reason = format!("This output belongs to {path_marker}, not {want}.");
            return result;
        }
    }
    if !result.complete {
        result.reason =
            "The check did not finish: the command printed no completion marker.".to_string();
        return result;
    }

    // A finding describes the path rather than its content.
    let findings = [
        "missing",
        "directory",
        "not-regular",
        "not-absolute",
        "denied",
        "unreadable",
    ];
    if let Some(finding) = findings
        .iter()
        .find(|f| lines.iter().any(|l| l == &format!("LINKR_VERIFY:{f}")))
    {
        let label = match *finding {
            "missing" => "missing",
            "directory" => "directory",
            "not-regular" => "other",
            _ => "unknown",
        };
        let content_claim = matches!(*finding, "missing" | "directory" | "not-regular");
        let text = match *finding {
            "missing" => "The target path does not exist.",
            "directory" => "The target path is a directory, not a file.",
            "not-regular" => {
                "The target path is not a regular file, so it has no content to verify."
            }
            "not-absolute" => {
                "The target path is not absolute, so the check never ran against a real file."
            }
            "denied" => {
                "The target file exists but is not readable, so its content could not be checked."
            }
            _ => "The target file could not be measured: the size command failed.",
        };
        result.found = label.to_string();
        if !asked {
            result.status = Verdict::Observed;
            result.reason = format!(
                "{text} No expectation was supplied, so this is a measurement, not a verification."
            );
        } else if content_claim {
            result.status = Verdict::Mismatch;
            result.reason = format!("{text} The expected content therefore cannot be present.");
        } else {
            result.status = Verdict::Indeterminate;
            result.reason = text.to_string();
        }
        return result;
    }

    if !lines.contains(&"LINKR_VERIFY:file") {
        result.reason =
            "The output reports no file state, so the target's answer is unusable.".to_string();
        return result;
    }
    let bytes_marker = lines.iter().find_map(|l| {
        l.trim()
            .strip_prefix("LINKR_VERIFY:bytes=")
            .map(|s| s.trim())
    });
    let Some(bytes_marker) = bytes_marker else {
        result.reason =
            "The target reported a regular file but no size, so its content cannot be compared."
                .to_string();
        return result;
    };
    result.found = "file".to_string();
    result.bytes = bytes_marker.parse().ok();

    let sha = lines
        .iter()
        .find_map(|l| l.strip_prefix("LINKR_VERIFY:sha256="));
    if let Some(sha) = sha {
        if sha == "unavailable" {
            result.sha256_unavailable = true;
        } else {
            result.sha256 = sha.to_string();
        }
    }

    let mut checks = (None, None); // (bytes, sha256) verdicts
    if let (Some(want), Some(actual)) = (want_bytes, result.bytes) {
        checks.0 = Some(actual == want);
    }
    if !want_sha.is_empty() {
        if result.sha256_unavailable || result.sha256.is_empty() {
            checks.1 = Some(false); // unknown → indeterminate
        } else {
            checks.1 = Some(result.sha256 == want_sha);
        }
    }

    if checks.0 == Some(false) || checks.1 == Some(false) {
        let mut detail = Vec::new();
        if checks.0 == Some(false) {
            detail.push(format!(
                "the target holds {} bytes, not {}",
                result.bytes.unwrap_or(0),
                want_bytes.unwrap_or(0)
            ));
        }
        if checks.1 == Some(false) && !result.sha256_unavailable && !result.sha256.is_empty() {
            detail.push(format!(
                "the target's sha256 is {}, not {want_sha}",
                result.sha256
            ));
        }
        result.status = Verdict::Mismatch;
        result.reason = format!(
            "The target file is not the expected content: {}.",
            detail.join("; ")
        );
        return result;
    }
    // A digest that could not be produced is indeterminate when one was asked.
    if !want_sha.is_empty() && (result.sha256_unavailable || result.sha256.is_empty()) {
        result.status = Verdict::Indeterminate;
        result.reason = if result.sha256_unavailable {
            "The target has neither sha256sum nor shasum, so the expected digest could not be checked.".to_string()
        } else {
            "The output carries no digest, so the expected digest could not be checked.".to_string()
        };
        return result;
    }
    if !asked {
        result.status = Verdict::Observed;
        result.reason = "The target file was measured. No expectation was supplied, so this is a measurement, not a verification.".to_string();
        return result;
    }
    result.status = Verdict::Match;
    result.reason = "The target file matches every expectation that was supplied.".to_string();
    result
}

// --- service verify ---------------------------------------------------------

/// The service kinds that can be verified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceKind {
    Unit,
    Process,
    Port,
}

/// The expectations each service kind accepts.
pub const UNIT_EXPECTATIONS: [&str; 3] = ["active", "inactive", "failed"];
pub const PROCESS_EXPECTATIONS: [&str; 2] = ["running", "absent"];
pub const PORT_EXPECTATIONS: [&str; 2] = ["listening", "closed"];

fn normalize_expectation(kind: ServiceKind, value: &str) -> Result<String, String> {
    let allowed: &[&str] = match kind {
        ServiceKind::Unit => &UNIT_EXPECTATIONS,
        ServiceKind::Process => &PROCESS_EXPECTATIONS,
        ServiceKind::Port => &PORT_EXPECTATIONS,
    };
    if value.is_empty() {
        return Ok(allowed[0].to_string());
    }
    if allowed.contains(&value) {
        Ok(value.to_string())
    } else {
        Err(format!(
            "Expected {kind:?} state must be one of {}.",
            allowed.join(", ")
        ))
    }
}

/// Build a `verifyServiceCommand` for exactly one of unit/process/port.
pub fn verify_service_command(
    unit: Option<&str>,
    process: Option<&str>,
    port: Option<u16>,
    expect: &str,
) -> Result<String, String> {
    let requested = [unit.is_some(), process.is_some(), port.is_some()];
    if requested.iter().filter(|b| **b).count() != 1 {
        return Err("Verify exactly one of unit, process or port.".to_string());
    }

    let build = |parts: Vec<String>| -> Result<String, String> {
        let command = parts.join("; ");
        if command.len() > MAX_VERIFY_COMMAND_BYTES {
            return Err(format!(
                "Verify command would be {} characters, over the {MAX_VERIFY_COMMAND_BYTES}-character UART budget; shorten the path or pattern.",
                command.len()
            ));
        }
        Ok(command)
    };

    if let Some(unit) = unit {
        if unit.is_empty()
            || !unit
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphanumeric())
            || !unit
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "@._:-".contains(c))
        {
            return Err("A unit name must start with a letter or digit and contain only letters, digits, @ . _ : -".to_string());
        }
        normalize_expectation(ServiceKind::Unit, expect)?;
        let s = quote_shell(unit)?;
        return build(vec![
            format!("s={s}"),
            "printf '\\nLINKR_VERIFY:subject=unit:%s\\n' \"$s\"".to_string(),
            "command -v systemctl >/dev/null 2>&1 || { printf 'LINKR_VERIFY:unsupported=no-systemctl\\nLINKR_VERIFY:done\\n'; exit 0; }".to_string(),
            "o=$(systemctl show -p LoadState -p ActiveState -p SubState -p MainPID -p ExecMainStatus -- \"$s\" 2>/dev/null) || :".to_string(),
            "printf 'LINKR_VERIFY:unit-begin\\n%s\\nLINKR_VERIFY:unit-end\\n' \"$o\"".to_string(),
            "printf 'LINKR_VERIFY:done\\n'".to_string(),
        ]);
    }

    if let Some(process) = process {
        if process.is_empty()
            || process.len() > MAX_VERIFY_PATTERN
            || process.chars().any(|c| c.is_control())
        {
            return Err(
                "A process pattern must be a non-empty string without control characters."
                    .to_string(),
            );
        }
        normalize_expectation(ServiceKind::Process, expect)?;
        let s = quote_shell(process)?;
        return build(vec![
            format!("s={s}"),
            "printf '\\nLINKR_VERIFY:subject=process:%s\\n' \"$s\"".to_string(),
            "command -v pgrep >/dev/null 2>&1 || { printf 'LINKR_VERIFY:unsupported=no-pgrep\\nLINKR_VERIFY:done\\n'; exit 0; }".to_string(),
            "printf 'LINKR_VERIFY:process-begin\\n'".to_string(),
            format!("pgrep -f \"$s\" 2>/dev/null | head -n {MAX_PROCESS_PIDS}"),
            "printf '\\nLINKR_VERIFY:process-end\\n'".to_string(),
            "printf 'LINKR_VERIFY:done\\n'".to_string(),
        ]);
    }

    let port = port.expect("checked above");
    normalize_expectation(ServiceKind::Port, expect)?;
    build(vec![
        format!("printf '\\nLINKR_VERIFY:subject=port:%s\\n' '{port}'"),
        "t=".to_string(),
        "if command -v ss >/dev/null 2>&1; then t=ss; elif command -v netstat >/dev/null 2>&1; then t=netstat; fi".to_string(),
        "[ -n \"$t\" ] || { printf 'LINKR_VERIFY:unsupported=no-listener-tool\\nLINKR_VERIFY:done\\n'; exit 0; }".to_string(),
        "printf 'LINKR_VERIFY:listener-tool=%s\\n' \"$t\"".to_string(),
        "printf 'LINKR_VERIFY:listeners-begin\\n'".to_string(),
        format!("$t -ltn 2>/dev/null | head -n {MAX_LISTENER_ROWS}"),
        "printf '\\nLINKR_VERIFY:listeners-end\\n'".to_string(),
        "printf 'LINKR_VERIFY:done\\n'".to_string(),
    ])
}

/// The result of parsing a service verify.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyServiceResult {
    pub status: Verdict,
    pub complete: bool,
    pub unsupported: String,
    pub reason: String,
}

/// Parse `verifyServiceCommand` output.
pub fn parse_verify_service_result(
    text: &str,
    kind: ServiceKind,
    subject: &str,
    expect: &str,
) -> VerifyServiceResult {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = normalized.split('\n').map(|l| l.trim_end()).collect();

    let expectation = normalize_expectation(kind, expect).unwrap_or_default();
    let mut result = VerifyServiceResult {
        status: Verdict::Indeterminate,
        complete: lines.contains(&"LINKR_VERIFY:done"),
        unsupported: String::new(),
        reason: String::new(),
    };

    let subject_marker = lines
        .iter()
        .find_map(|l| l.strip_prefix("LINKR_VERIFY:subject="));
    let Some(subject_marker) = subject_marker else {
        result.reason = if result.complete {
            "The output does not name the claim that was checked, so nothing can be attributed to this request.".to_string()
        } else {
            "No output named the claim and no completion marker arrived: the check either never ran or its output was lost.".to_string()
        };
        return result;
    };
    let expected_kind = match kind {
        ServiceKind::Unit => "unit",
        ServiceKind::Process => "process",
        ServiceKind::Port => "port",
    };
    let (marker_kind, marker_subject) = subject_marker
        .split_once(':')
        .unwrap_or((subject_marker, ""));
    if marker_kind != expected_kind || marker_subject != subject {
        result.reason = format!(
            "This output belongs to {marker_kind} {marker_subject}, not {expected_kind} {subject}."
        );
        return result;
    }

    let unsupported = lines
        .iter()
        .find_map(|l| l.strip_prefix("LINKR_VERIFY:unsupported="));
    if let Some(unsupported) = unsupported {
        result.unsupported = unsupported.to_string();
        result.reason = match unsupported {
            "no-systemctl" => {
                "The target has no systemctl, so it cannot report service state.".to_string()
            }
            "no-pgrep" => {
                "The target has no pgrep, so it cannot report matching processes.".to_string()
            }
            "no-listener-tool" => {
                "The target has neither ss nor netstat, so it cannot report listening ports."
                    .to_string()
            }
            other => format!("The target cannot report this: {other}."),
        };
        return result;
    }
    if !result.complete {
        result.reason =
            "The check did not finish: the command printed no completion marker.".to_string();
        return result;
    }

    // Extract the block and decide match/mismatch.
    match kind {
        ServiceKind::Unit => {
            let (rows, closed) = block(&lines, "LINKR_VERIFY:unit-begin", "LINKR_VERIFY:unit-end");
            if !closed {
                result.reason =
                    "The target's unit state arrived incomplete, so its state cannot be read."
                        .to_string();
                return result;
            }
            let settings: std::collections::HashMap<String, String> = rows
                .iter()
                .filter_map(|row| row.split_once('='))
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            let Some(active) = settings.get("ActiveState") else {
                result.reason =
                    "The target reported no unit state, so there is nothing to compare."
                        .to_string();
                return result;
            };
            if settings.get("LoadState").map(|s| s.as_str()) == Some("not-found") {
                result.reason = format!("{subject} is not a unit on this target, which is not the same as a stopped service.");
                return result;
            }
            result.status = if active == &expectation {
                Verdict::Match
            } else {
                Verdict::Mismatch
            };
            let sub = settings.get("SubState").map(|s| s.as_str()).unwrap_or("");
            let detail = format!(
                "ActiveState={active}{}",
                if sub.is_empty() {
                    String::new()
                } else {
                    format!(", SubState={sub}")
                }
            );
            result.reason = if result.status == Verdict::Match {
                format!("{subject} is {expectation} ({detail}).")
            } else {
                format!("{subject} is not {expectation}: {detail}.")
            };
        }
        ServiceKind::Process => {
            let (rows, closed) = block(
                &lines,
                "LINKR_VERIFY:process-begin",
                "LINKR_VERIFY:process-end",
            );
            if !closed {
                result.reason =
                    "The target's process list arrived incomplete, so it cannot be read."
                        .to_string();
                return result;
            }
            let pids: Vec<&str> = rows
                .iter()
                .filter(|r| !r.is_empty() && r.chars().all(|c| c.is_ascii_digit()))
                .copied()
                .collect();
            let running = !pids.is_empty();
            result.status = if running == (expectation == "running") {
                Verdict::Match
            } else {
                Verdict::Mismatch
            };
            let count = format!(
                "matched {} process{}",
                pids.len(),
                if pids.len() == 1 { "" } else { "es" }
            );
            result.reason = if result.status == Verdict::Match {
                format!(
                    "The target {} matching process ({count}).",
                    if expectation == "running" {
                        "has"
                    } else {
                        "has no"
                    }
                )
            } else {
                format!(
                    "The target {} matching process, but {expectation} was expected ({count}).",
                    if expectation == "running" {
                        "has no"
                    } else {
                        "still has"
                    }
                )
            };
        }
        ServiceKind::Port => {
            let tool = lines
                .iter()
                .find_map(|l| l.strip_prefix("LINKR_VERIFY:listener-tool="));
            let (rows, closed) = block(
                &lines,
                "LINKR_VERIFY:listeners-begin",
                "LINKR_VERIFY:listeners-end",
            );
            if !closed {
                result.reason =
                    "The target's listening-socket table arrived incomplete, so it cannot be read."
                        .to_string();
                return result;
            }
            let Some(tool) = tool else {
                result.reason = "The output does not say which tool produced the socket table, so its rows cannot be interpreted.".to_string();
                return result;
            };
            let port = subject.parse::<u16>().unwrap_or(0);
            let listening = rows.iter().any(|row| {
                row.split_whitespace().any(|field| {
                    field.ends_with(&format!(":{port}"))
                        && field
                            .trim_start_matches('[')
                            .trim_end_matches(']')
                            .split(':')
                            .next_back()
                            .map(|p| p == port.to_string())
                            .unwrap_or(false)
                })
            });
            let truncated = rows.len() >= MAX_LISTENER_ROWS;
            if !listening && truncated {
                result.reason = format!("{tool} printed the first {MAX_LISTENER_ROWS} listening sockets and stopped, so this table cannot show that nothing listens on port {port}.");
                return result;
            }
            result.status = if listening == (expectation == "listening") {
                Verdict::Match
            } else {
                Verdict::Mismatch
            };
            result.reason = if listening {
                format!("{tool} reports something listening on port {port}.")
            } else {
                format!("{tool} reports nothing listening on port {port}.")
            };
        }
    }
    result
}

/// Collect the lines between a pair of block markers.
fn block<'a>(lines: &[&'a str], begin: &str, end: &str) -> (Vec<&'a str>, bool) {
    let Some(start) = lines.iter().position(|l| *l == begin) else {
        return (Vec::new(), false);
    };
    let stop = lines.iter().skip(start + 1).position(|l| *l == end);
    let rows: Vec<&str> = match stop {
        Some(rel) => lines[start + 1..start + 1 + rel]
            .iter()
            .filter(|l| !l.is_empty())
            .copied()
            .collect(),
        None => lines[start + 1..]
            .iter()
            .filter(|l| !l.is_empty())
            .copied()
            .collect(),
    };
    (rows, stop.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_a_file_verify_command() {
        let cmd = verify_file_command("/etc/hostname", false).unwrap();
        assert!(cmd.contains("LINKR_VERIFY:path="));
        assert!(cmd.contains("LINKR_VERIFY:done"));
        assert!(cmd.contains("wc -c"));
        assert!(!cmd.contains("sha256sum"));
        // hashing is opt-in
        let cmd = verify_file_command("/etc/hostname", true).unwrap();
        assert!(cmd.contains("sha256sum"));
    }

    #[test]
    fn parses_a_matching_file_verify() {
        let out = "LINKR_VERIFY:path=/etc/hostname\nLINKR_VERIFY:file\nLINKR_VERIFY:bytes=12\nLINKR_VERIFY:done\n";
        let r = parse_verify_file_result(out, Some("/etc/hostname"), Some(12), "");
        assert_eq!(r.status, Verdict::Match);
        assert_eq!(r.bytes, Some(12));
    }

    #[test]
    fn parses_a_missing_file_as_observed() {
        let out = "LINKR_VERIFY:path=/nope\nLINKR_VERIFY:missing\nLINKR_VERIFY:done\n";
        let r = parse_verify_file_result(out, Some("/nope"), None, "");
        assert_eq!(r.status, Verdict::Observed);
        assert_eq!(r.found, "missing");
    }

    #[test]
    fn a_missing_file_refutes_expected_content() {
        let out = "LINKR_VERIFY:path=/nope\nLINKR_VERIFY:missing\nLINKR_VERIFY:done\n";
        let r = parse_verify_file_result(out, Some("/nope"), Some(10), "");
        assert_eq!(r.status, Verdict::Mismatch);
    }

    #[test]
    fn a_size_mismatch_is_reported() {
        let out =
            "LINKR_VERIFY:path=/f\nLINKR_VERIFY:file\nLINKR_VERIFY:bytes=99\nLINKR_VERIFY:done\n";
        let r = parse_verify_file_result(out, Some("/f"), Some(100), "");
        assert_eq!(r.status, Verdict::Mismatch);
    }

    #[test]
    fn a_digest_check_matches_and_mismatches() {
        let digest = "a".repeat(64);
        let ok = format!("LINKR_VERIFY:path=/f\nLINKR_VERIFY:file\nLINKR_VERIFY:bytes=10\nLINKR_VERIFY:sha256={digest}\nLINKR_VERIFY:done\n");
        let r = parse_verify_file_result(&ok, Some("/f"), Some(10), &digest);
        assert_eq!(r.status, Verdict::Match);

        let bad = format!("LINKR_VERIFY:path=/f\nLINKR_VERIFY:file\nLINKR_VERIFY:bytes=10\nLINKR_VERIFY:sha256={}\nLINKR_VERIFY:done\n", "b".repeat(64));
        let r = parse_verify_file_result(&bad, Some("/f"), Some(10), &digest);
        assert_eq!(r.status, Verdict::Mismatch);
    }

    #[test]
    fn an_unattributed_verify_is_indeterminate() {
        let out = "LINKR_VERIFY:file\nLINKR_VERIFY:bytes=10\nLINKR_VERIFY:done\n";
        let r = parse_verify_file_result(out, Some("/f"), Some(10), "");
        assert_eq!(r.status, Verdict::Indeterminate);
    }

    #[test]
    fn builds_unit_process_and_port_commands() {
        let unit = verify_service_command(Some("sshd.service"), None, None, "active").unwrap();
        assert!(unit.contains("subject=unit"));
        let proc = verify_service_command(None, Some("nginx"), None, "running").unwrap();
        assert!(proc.contains("subject=process"));
        let port = verify_service_command(None, None, Some(80), "listening").unwrap();
        assert!(port.contains("subject=port"));
        // exactly one
        assert!(verify_service_command(Some("a"), Some("b"), None, "").is_err());
        assert!(verify_service_command(None, None, None, "").is_err());
    }

    #[test]
    fn parses_a_matching_unit_state() {
        let out = "LINKR_VERIFY:subject=unit:sshd.service\nLINKR_VERIFY:unit-begin\nLoadState=loaded\nActiveState=active\nSubState=running\nLINKR_VERIFY:unit-end\nLINKR_VERIFY:done\n";
        let r = parse_verify_service_result(out, ServiceKind::Unit, "sshd.service", "active");
        assert_eq!(r.status, Verdict::Match);
    }

    #[test]
    fn parses_a_mismatching_process_state() {
        let out = "LINKR_VERIFY:subject=process:nginx\nLINKR_VERIFY:process-begin\nLINKR_VERIFY:process-end\nLINKR_VERIFY:done\n";
        let r = parse_verify_service_result(out, ServiceKind::Process, "nginx", "running");
        assert_eq!(r.status, Verdict::Mismatch);
    }

    #[test]
    fn parses_a_listening_port() {
        let out = "LINKR_VERIFY:subject=port:80\nLINKR_VERIFY:listener-tool=ss\nLINKR_VERIFY:listeners-begin\n0.0.0.0:80 0.0.0.0:* LISTEN\nLINKR_VERIFY:listeners-end\nLINKR_VERIFY:done\n";
        let r = parse_verify_service_result(out, ServiceKind::Port, "80", "listening");
        assert_eq!(r.status, Verdict::Match);
    }

    #[test]
    fn an_unsupported_service_check_is_indeterminate() {
        let out = "LINKR_VERIFY:subject=unit:x\nLINKR_VERIFY:unsupported=no-systemctl\nLINKR_VERIFY:done\n";
        let r = parse_verify_service_result(out, ServiceKind::Unit, "x", "active");
        assert_eq!(r.status, Verdict::Indeterminate);
        assert_eq!(r.unsupported, "no-systemctl");
    }
}
