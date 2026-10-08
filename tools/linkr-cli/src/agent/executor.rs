//! Device execution: records, the tracked exit marker, observation windows and
//! the target download/profile probes. Ports of `web/device_executor.js`,
//! `web/serial_observation.js`, `web/download_plan.js` and
//! `web/device_profile.js` (spec §2.2–2.9, §5).
//!
//! The module is deliberately split into pure decisions plus a small record
//! store: every string the model reads back (`timedOut`, `waitStatus`, the
//! `next` hint, each validation error) is fixed by the contract, so the tests
//! assert them literally, and the only impure input is the journal behind
//! [`LogReader`].

use std::collections::HashMap;

use serde::Serialize;
use serde_json::{json, Value};

use super::console::inspect_serial_console;
use super::policy::CommandPolicy;
use crate::journal::JournalRead;
use crate::target_files::quote_shell;

/// `MAX_EXECUTION_RECORDS = 50`, oldest evicted first (spec §5.2).
pub const MAX_EXECUTION_RECORDS: usize = 50;
/// `APPROVAL_STALE_MS` — a pending approval expires after 15 minutes.
pub const APPROVAL_STALE_MS: u64 = 900_000;
/// `EXECUTION_STALE_MS` — a still-`pending` record becomes `stalled`.
pub const EXECUTION_STALE_MS: u64 = 300_000;
/// `FULL_AUTO_WINDOW_MS = 15 * 60 * 1000` (spec §5.3).
pub const FULL_AUTO_WINDOW_MS: u64 = 900_000;

/// Serial input limits (spec §14.3).
pub const MAX_COMMAND_CHARS: usize = 1024;
pub const MAX_INPUT_CHARS: usize = 2048;
/// Tracked wrapper after quoting shares the serial input budget.
pub const MAX_TRACKED_CHARS: usize = 2048;

// Exact messages (spec §2, §5.1).
pub const ERR_TRACKED_SHELL: &str =
    "Tracked commands require an idle, observed POSIX shell prompt.";
pub const ERR_TRACKED_TOO_LONG: &str = "Tracked command is too long after shell quoting.";
pub const ERR_INVALID_INPUT: &str = "Invalid serial input arguments.";
pub const ERR_DISCONNECTED: &str = "Device is disconnected.";
pub const ERR_SESSION_CHANGED: &str = "Device session changed; input was not sent.";
pub const ERR_INPUT_REVISION: &str =
    "Terminal input changed; request a new command before sending.";
pub const ERR_CONSOLE_CHANGED: &str = "Console state changed; review a new command before sending.";
pub const ERR_RECORD_UNAVAILABLE: &str = "Execution record is unavailable for this device session.";
pub const ERR_CANCELLED: &str = "Operation cancelled. Do not retry automatically.";
pub const ERR_TOOL_BUDGET: &str =
    "Tool-call budget exhausted. No further tools will run for this question.";
pub const ERR_APPROVAL_GONE: &str = "Approval not pending or expired.";
pub const ERR_NO_ACTIVE_TASK: &str = "No active task. Send a new question.";
pub const ERR_QUEUE_ITEM: &str = "Invalid queued message.";
pub const ERR_QUEUE_FULL: &str = "Queue is full (8 messages). Clear pending messages or wait.";
pub const ERR_ALREADY_RUNNING: &str =
    "Agent is already processing. Wait for the current run to stop.";
pub const ERR_SESSION_MOVED: &str = "Device session or mode changed. Start a new conversation.";

/// Model-facing hint when the target is waiting for a human.
pub const NEXT_AWAITING_INPUT: &str = "Target is waiting for interaction. Report the prompt; passwords must be entered by the user directly in the terminal.";
/// Model-facing hint when the deadline passed with no exit marker.
pub const NEXT_UNRESOLVED: &str =
    "Execution is unresolved. Monitor this same id again; do not resend the command.";
pub const NEXT_INSPECT: &str =
    "Call inspect_serial_execution with this id to inspect subsequent output. Delivery alone is not command success.";

/// `The user rejected ${toolName}. …` (spec §5.1).
pub fn rejected_message(tool_name: &str) -> String {
    format!(
        "The user rejected {tool_name}. Do not retry this action; ask for a different approach or stop."
    )
}

/// The accessory variant from `web/accessory_control.js`.
pub const REJECTED_ACCESSORY: &str =
    "The user rejected this change. Do not retry unless the user asks again.";

/// `DOWNLOAD_PROBE` (`web/download_plan.js:1`).
pub const DOWNLOAD_PROBE: &str = "for t in curl wget sha256sum shasum openssl; do command -v \"$t\" >/dev/null 2>&1 && printf 'LINKR_TOOL:%s\\n' \"$t\"; done; :";

/// `PROFILE_PROBE` (`web/device_profile.js:1`).
pub const PROFILE_PROBE: &str = "printf 'LINKR_PROFILE_BEGIN\\n'; uname -a; printf 'LINKR_OS\\n'; cat /etc/os-release 2>/dev/null; printf 'LINKR_MODEL\\n'; cat /proc/device-tree/model 2>/dev/null; printf '\\nLINKR_BOOT\\n'; cat /proc/sys/kernel/random/boot_id 2>/dev/null; printf 'LINKR_DISK\\n'; df -Pk /; printf 'LINKR_TOOLS\\n'; for t in sh curl wget sha256sum shasum openssl sudo systemctl busybox; do command -v \"$t\" >/dev/null 2>&1 && printf 'TOOL:%s\\n' \"$t\"; done; printf 'LINKR_PROFILE_END\\n'";

// ---------------------------------------------------------------------------
// Probe helpers
// ---------------------------------------------------------------------------

fn tool_name_pattern() -> &'static regex::Regex {
    use std::sync::OnceLock;
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"^[A-Za-z0-9][A-Za-z0-9_.+-]{0,63}$").expect("pattern"))
}

/// `probe_tools` argument validation (`Invalid tool names`).
pub fn validate_tool_names(names: &[String]) -> Result<Vec<String>, String> {
    if names.is_empty() || names.len() > 16 {
        return Err("Invalid tool names".to_string());
    }
    if names.iter().any(|name| !tool_name_pattern().is_match(name)) {
        return Err("Invalid tool names".to_string());
    }
    let mut seen: Vec<String> = Vec::new();
    for name in names {
        if !seen.contains(name) {
            seen.push(name.clone());
        }
    }
    Ok(seen)
}

/// The `probe_tools` shell line, with de-duplicated single-quoted names.
pub fn probe_tools_command(names: &[String]) -> String {
    // `mobile/src/pi-agent.mjs`: `[...new Set(args.names)].map(name => "'" + name + "'").join(" ")`.
    let list = names
        .iter()
        .map(|name| format!("'{}'", name.replace('\'', "'\\''")))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "for t in {list}; do if command -v \"$t\" >/dev/null 2>&1; then printf 'TOOL:%s:available\\n' \"$t\"; else printf 'TOOL:%s:missing\\n' \"$t\"; fi; done"
    )
}

/// `Tools now available: ${announced.join(", ")}.`
pub fn tools_announcement(announced: &[String]) -> String {
    format!("Tools now available: {}.", announced.join(", "))
}

// ---------------------------------------------------------------------------
// Tracked exit marker
// ---------------------------------------------------------------------------

/// The per-execution token: `LINKR_EXIT_<uuid without dashes>`.
pub fn completion_token() -> String {
    format!("LINKR_EXIT_{}", uuid::Uuid::new_v4().simple())
}

/// `sh -c '<quoted>'; printf '\n%s:%s\n' '<token>' "$?"` on one physical line.
///
/// Returns `(token, wire_text)`.
pub fn tracked_command(text: &str, token: &str) -> Result<String, String> {
    let quoted = quote_shell(text).map_err(|error| error.to_string())?;
    let wire = format!("sh -c {quoted}; printf '\\n%s:%s\\n' '{token}' \"$?\"");
    if wire.chars().count() > MAX_TRACKED_CHARS {
        return Err(ERR_TRACKED_TOO_LONG.to_string());
    }
    Ok(wire)
}

/// The wrapper only runs at an idle, observed POSIX shell prompt.
pub fn track_exit_allowed(append_enter: bool, console_kind: &str, input_pending: bool) -> bool {
    append_enter && console_kind == "shell" && !input_pending
}

/// `(?:^|\n)<token>:([0-9]{1,3})\r?\n` — only an exit code ≤ 255 counts.
pub fn exit_code_from(evidence: &str, token: &str) -> Option<u8> {
    let needle = format!("{token}:");
    for (index, _) in evidence.match_indices(&needle) {
        let prefix_ok = index == 0
            || evidence[..index]
                .chars()
                .next_back()
                .is_some_and(|c| c == '\n');
        if !prefix_ok {
            continue;
        }
        let rest = &evidence[index + needle.len()..];
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() || digits.len() > 3 {
            continue;
        }
        // The marker line ends with a newline (or the end of the page).
        let after = &rest[digits.len()..];
        if !(after.is_empty() || after.starts_with('\n') || after.starts_with("\r\n")) {
            continue;
        }
        let code: u16 = digits.parse().ok()?;
        if code <= 255 {
            return Some(code as u8);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DownloadMeta {
    pub destination: String,
    pub path: String,
    pub url: String,
    pub downloader: String,
    pub checksum: String,
    #[serde(rename = "expectedSha256")]
    pub expected_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(rename = "partialPath", skip_serializing_if = "Option::is_none")]
    pub partial_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExecutionRecord {
    pub id: String,
    pub session_id: String,
    pub input_revision: u64,
    pub sent_revision: Option<u64>,
    pub mode: String,
    pub tool_name: String,
    /// The exact text the model asked to send (unquoted).
    pub text: String,
    /// The wire payload, already wrapped when the exit marker is tracked.
    pub payload: String,
    pub append_enter: bool,
    pub console_kind: String,
    pub state: String,
    pub delivery: String,
    pub execution_status: String,
    pub exit_code: Option<u8>,
    pub observation: String,
    pub observation_closed: bool,
    pub waiting_for: Option<String>,
    pub completion_token: Option<String>,
    pub tool_probe: Option<Vec<String>>,
    pub profile_probe: bool,
    pub download: Option<DownloadMeta>,
    pub evidence: String,
    pub evidence_truncated: bool,
    pub log_start: u64,
    pub observed_end: Option<u64>,
    pub error: Option<String>,
    pub created_at: u64,
    pub completed_at: Option<u64>,
    pub mode_expires_at: u64,
}

impl ExecutionRecord {
    /// Every field mirrors one entry of the JS record (`web/device_executor.js`);
    /// splitting them into groups would only hide that mapping.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: String,
        session_id: String,
        input_revision: u64,
        mode: &str,
        tool_name: &str,
        text: String,
        payload: String,
        append_enter: bool,
        console_kind: &str,
        created_at: u64,
    ) -> Self {
        ExecutionRecord {
            id,
            session_id,
            input_revision,
            sent_revision: None,
            mode: mode.to_string(),
            tool_name: tool_name.to_string(),
            text,
            payload,
            append_enter,
            console_kind: console_kind.to_string(),
            state: "proposed".to_string(),
            delivery: "not-sent".to_string(),
            execution_status: "unknown".to_string(),
            exit_code: None,
            observation: "no-output".to_string(),
            observation_closed: false,
            waiting_for: None,
            completion_token: None,
            tool_probe: None,
            profile_probe: false,
            download: None,
            evidence: String::new(),
            evidence_truncated: false,
            log_start: 0,
            observed_end: None,
            error: None,
            created_at,
            completed_at: None,
            mode_expires_at: 0,
        }
    }

    /// `delivery`/`state` transitions shared by every send path.
    pub fn mark_sending(&mut self) {
        self.state = "sending".to_string();
        self.delivery = "unknown".to_string();
    }

    pub fn mark_sent(&mut self, revision: u64, at_ms: u64) {
        self.delivery = "sent".to_string();
        self.sent_revision = Some(revision);
        self.state = "sent".to_string();
        self.completed_at = None;
        let _ = at_ms;
    }

    pub fn mark_failed(&mut self, error: String, cancelled: bool) {
        if self.state != "denied" {
            self.state = if cancelled { "cancelled" } else { "failed" }.to_string();
        }
        self.error = Some(error);
        self.observation_closed = true;
    }

    /// True while the shell may still contribute output for this record.
    pub fn is_open(&self) -> bool {
        self.delivery == "sent" && !self.observation_closed
    }

    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "sessionId": self.session_id,
            "mode": self.mode,
            "toolName": self.tool_name,
            "payload": self.payload,
            "delivery": self.delivery,
            "state": self.state,
            "executionStatus": self.execution_status,
            "exitCode": self.exit_code,
            "observation": self.observation,
            "waitingFor": self.waiting_for,
            "evidence": self.evidence,
            "evidenceTruncated": self.evidence_truncated,
            "error": self.error,
            "download": self.download.as_ref().map(|d| json!({
                "destination": d.destination,
                "path": d.path,
                "url": d.url,
                "downloader": d.downloader,
                "checksum": d.checksum,
                "expectedSha256": d.expected_sha256,
                "status": d.status,
                "partialPath": d.partial_path,
                "sha256": d.sha256,
                "bytes": d.bytes,
            })),
        })
    }
}

/// The bounded ring of execution records.
#[derive(Debug, Default)]
pub struct ExecutionStore {
    records: Vec<ExecutionRecord>,
    next_id: u64,
}

impl ExecutionStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn allocate_id(&mut self) -> String {
        self.next_id += 1;
        format!("serial-{}", self.next_id)
    }

    pub fn push(&mut self, record: ExecutionRecord) {
        self.records.push(record);
        if self.records.len() > MAX_EXECUTION_RECORDS {
            self.records.remove(0);
        }
    }

    pub fn get(&self, id: &str) -> Option<&ExecutionRecord> {
        self.records.iter().find(|record| record.id == id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut ExecutionRecord> {
        self.records.iter_mut().find(|record| record.id == id)
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub fn records(&self) -> &[ExecutionRecord] {
        &self.records
    }

    /// Close every open record that no longer belongs to this session.
    pub fn close_foreign(&mut self, session_id: &str) {
        for record in &mut self.records {
            if record.is_open() && record.session_id != session_id {
                record.observation = "interrupted".to_string();
                record.observation_closed = true;
            }
        }
    }

    /// Records older than [`EXECUTION_STALE_MS`] that never completed.
    pub fn apply_staleness(&mut self, now_ms: u64) {
        for record in &mut self.records {
            if record.execution_status == "unknown"
                && record.delivery == "sent"
                && now_ms.saturating_sub(record.created_at) > EXECUTION_STALE_MS
            {
                record.execution_status = "stalled".to_string();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Observation windows (web/serial_observation.js)
// ---------------------------------------------------------------------------

/// Clamp `timeoutMs` for `waitForSerialOutput`: 100..=5000.
pub fn clamp_wait_timeout(timeout_ms: i64) -> u64 {
    timeout_ms.clamp(100, 5000) as u64
}

/// Clamp `settleMs`: 100..=1000.
pub fn clamp_settle(settle_ms: i64) -> u64 {
    settle_ms.clamp(100, 1000) as u64
}

/// Clamp `timeoutMs` for `monitor_serial_execution`: 100..=60000, default 30000.
pub fn clamp_monitor_timeout(timeout_ms: Option<i64>) -> u64 {
    timeout_ms.unwrap_or(30_000).clamp(100, 60_000) as u64
}

#[derive(Debug, Clone, PartialEq)]
pub struct WaitOutcome {
    pub has_more: bool,
    pub has_new_output: bool,
    pub quiet_for_ms: u64,
    pub wait_status: &'static str,
    pub timed_out: bool,
}

/// One iteration of the quiet-interval detector. `now`, `started` and
/// `changed` are monotonic milliseconds supplied by the caller.
#[allow(clippy::too_many_arguments)]
pub fn wait_decide(
    after: u64,
    latest_cursor: u64,
    now_ms: u64,
    started_ms: u64,
    changed_ms: u64,
    cursor_seen: u64,
    timeout_ms: u64,
    settle_ms: u64,
) -> (bool, WaitOutcome) {
    let changed = if latest_cursor != cursor_seen {
        now_ms
    } else {
        changed_ms
    };
    let has_new_output = latest_cursor > after;
    let quiet_for_ms = now_ms.saturating_sub(changed);
    let settled = has_new_output && quiet_for_ms >= settle_ms;
    let timed_out = now_ms.saturating_sub(started_ms) >= timeout_ms;
    let outcome = WaitOutcome {
        has_more: false,
        has_new_output,
        quiet_for_ms,
        wait_status: if settled {
            "settled"
        } else if has_new_output {
            "streaming"
        } else {
            "no-output"
        },
        timed_out,
    };
    // The caller reads the journal for `has_more`; the loop stops here.
    (settled || timed_out, outcome)
}

#[derive(Debug, Clone, PartialEq)]
pub struct MonitorOutcome {
    pub wait_status: Option<&'static str>,
    pub next: Option<&'static str>,
    pub timed_out: bool,
    pub done: bool,
}

/// One iteration of `monitorSerialExecution`: never stop on silence, only on
/// completion, interruption or the deadline.
pub fn monitor_decide(
    execution_status: &str,
    observation_closed: bool,
    delivery: &str,
    waiting_for: Option<&str>,
    now_ms: u64,
    deadline_ms: u64,
) -> MonitorOutcome {
    if execution_status == "completed" || observation_closed || delivery != "sent" {
        return MonitorOutcome {
            wait_status: None,
            next: None,
            timed_out: false,
            done: true,
        };
    }
    if let Some(kind) = waiting_for {
        let _ = kind;
        return MonitorOutcome {
            wait_status: Some("awaiting-input"),
            next: Some(NEXT_AWAITING_INPUT),
            timed_out: false,
            done: true,
        };
    }
    if now_ms >= deadline_ms {
        return MonitorOutcome {
            wait_status: None,
            next: Some(NEXT_UNRESOLVED),
            timed_out: true,
            done: true,
        };
    }
    MonitorOutcome {
        wait_status: None,
        next: None,
        timed_out: false,
        done: false,
    }
}

/// Poll interval for both loops (≤500 ms monitor, ≤50 ms wait).
pub fn monitor_poll_ms(remaining_ms: u64) -> u64 {
    remaining_ms.clamp(1, 500)
}

pub fn wait_poll_ms(remaining_ms: u64) -> u64 {
    remaining_ms.clamp(1, 50)
}

// ---------------------------------------------------------------------------
// Evidence paging
// ---------------------------------------------------------------------------

/// The journal slice an execution may expose (`executionPage`).
#[derive(Debug, Clone, PartialEq)]
pub struct PageResult {
    pub evidence: String,
    pub evidence_start: u64,
    pub evidence_truncated: bool,
    pub observed_cursor: u64,
    pub latest_cursor: u64,
    pub has_more: bool,
}

/// Any log the executor reads: `read(after, limit)` plus the live cursor.
pub trait LogReader {
    fn read(&self, after: Option<u64>, limit: usize) -> JournalRead;
    fn latest_cursor(&self) -> u64;
}

impl LogReader for crate::journal::SerialJournal {
    fn read(&self, after: Option<u64>, limit: usize) -> JournalRead {
        self.read(after, limit)
    }
    fn latest_cursor(&self) -> u64 {
        self.latest_cursor()
    }
}

/// `executionPage(record, {after, limit})`: evidence never crosses a closed
/// execution's end, and a ring-buffer eviction is re-bounded before it is
/// exposed.
pub fn execution_page<R: LogReader>(
    record: &ExecutionRecord,
    reader: &R,
    after: Option<u64>,
    limit: usize,
) -> PageResult {
    let end = record.observed_end.unwrap_or(record.log_start);
    let limit = (limit as i64).clamp(1, 16_000) as u64;
    let start = end.min(record.log_start.max(match after {
        Some(value) => value,
        None => end.saturating_sub(limit),
    }));
    let mut output = if start < end {
        Some(reader.read(Some(start), (end - start).min(limit) as usize))
    } else {
        None
    };
    let missing_history = output.as_ref().is_some_and(|page| page.truncated);
    if let Some(page) = output.as_ref() {
        if page.start < end && page.cursor > end {
            output = Some(reader.read(Some(page.start), (end - page.start) as usize));
        }
    }
    let available = output.as_ref().is_some_and(|page| page.start < end);
    let cursor = match output.as_ref() {
        Some(page) if available => end.min(page.cursor),
        _ => end,
    };
    let truncated = missing_history
        || output.as_ref().is_some_and(|page| page.truncated)
        || (output.as_ref().is_some() && !available)
        || match after {
            Some(_) => false,
            None => start > record.log_start,
        }
        || cursor < end;
    PageResult {
        evidence: if available {
            output
                .as_ref()
                .map(|page| page.text.clone())
                .unwrap_or_default()
        } else {
            String::new()
        },
        evidence_start: if available {
            output.as_ref().map(|page| page.start).unwrap_or(start)
        } else {
            start
        },
        evidence_truncated: truncated,
        observed_cursor: cursor,
        latest_cursor: end,
        has_more: cursor < end,
    }
}

// ---------------------------------------------------------------------------
// Inspection
// ---------------------------------------------------------------------------

/// Tool capabilities observed by a completed `probe_tools`.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCapability {
    pub available: bool,
    pub observed_at: u64,
    pub session_id: String,
    pub execution_id: String,
    pub cursor: u64,
    pub source: &'static str,
    pub stale: bool,
}

/// Extract `TOOL:<name>:available|missing` for every probed name. Partial or
/// conflicting output yields nothing: it cannot replace a verified capability.
pub fn parse_tool_probe(
    evidence: &str,
    names: &[String],
    exit_code: Option<u8>,
    truncated: bool,
) -> Option<Vec<(String, bool)>> {
    if exit_code != Some(0) || truncated {
        return None;
    }
    let evidence = evidence.replace('\r', "");
    let lines: Vec<&str> = evidence.split('\n').collect();
    let mut out = Vec::new();
    for name in names {
        let available_line = format!("TOOL:{name}:available");
        let missing_line = format!("TOOL:{name}:missing");
        let hits: Vec<&&str> = lines
            .iter()
            .filter(|line| **line == available_line || **line == missing_line)
            .collect();
        if hits.len() != 1 {
            return None;
        }
        out.push((name.clone(), hits[0].ends_with(":available")));
    }
    Some(out)
}

/// Apply one completed inspection to a record: classify the evidence, parse the
/// download markers and settle the exit marker.
pub fn refresh_record(
    record: &mut ExecutionRecord,
    page: PageResult,
    now_ms: u64,
    log_reader_connected: bool,
    same_session: bool,
) {
    record.evidence_truncated = page.evidence_truncated;
    if !log_reader_connected || !same_session {
        record.observation = "interrupted".to_string();
        record.observation_closed = true;
        return;
    }
    record.observed_end = Some(page.latest_cursor);
    record.evidence = page.evidence.clone();
    let hint = inspect_serial_console(&record.evidence, page.latest_cursor);
    record.observation = if record.evidence.is_empty() {
        "no-output".to_string()
    } else if hint.kind == "shell" {
        "prompt-returned".to_string()
    } else {
        "output-observed".to_string()
    };
    record.waiting_for = if super::console::INTERACTIVE_KINDS.contains(&hint.kind.as_str()) {
        Some(hint.kind)
    } else {
        None
    };
    if let Some(download) = record.download.as_mut() {
        for line in record.evidence.replace('\r', "").split('\n') {
            if let Some(rest) = line.strip_prefix("LINKR_PART:") {
                download.partial_path = Some(rest.trim().to_string());
            } else if let Some(rest) = line.strip_prefix("LINKR_SHA256:") {
                if rest.len() == 64 && rest.chars().all(|c| c.is_ascii_hexdigit()) {
                    download.sha256 = Some(rest.to_lowercase());
                }
            } else if let Some(rest) = line.strip_prefix("LINKR_BYTES:") {
                let digits: String = rest.trim().chars().filter(|c| c.is_ascii_digit()).collect();
                if !digits.is_empty() {
                    download.bytes = digits.parse().ok();
                }
            }
        }
    }
    if let Some(token) = record.completion_token.clone() {
        if let Some(code) = exit_code_from(&record.evidence, &token) {
            record.exit_code = Some(code);
            record.execution_status = if code == 0 { "completed" } else { "failed" }.to_string();
            record.observation_closed = true;
            record.completed_at = Some(now_ms);
            if let Some(download) = record.download.as_mut() {
                let verified = code == 0 && download.sha256.is_some() && download.bytes.is_some();
                download.status = Some(
                    if verified {
                        "saved"
                    } else {
                        "failed-or-unverified"
                    }
                    .to_string(),
                );
            }
        }
    }
}

/// Observed capabilities for the next `probe_tools` decision.
pub fn capability_map(store: &ExecutionStore, now_ms: u64) -> HashMap<String, ToolCapability> {
    let mut out: HashMap<String, ToolCapability> = HashMap::new();
    for record in store.records() {
        let Some(token) = record.completion_token.clone() else {
            continue;
        };
        let _ = token;
        if record.tool_probe.is_none() || record.exit_code != Some(0) {
            continue;
        }
        let Some(names) = record.tool_probe.clone() else {
            continue;
        };
        let Some(observed) = parse_tool_probe(
            &record.evidence,
            &names,
            record.exit_code,
            record.evidence_truncated,
        ) else {
            continue;
        };
        for (name, available) in observed {
            out.insert(
                name,
                ToolCapability {
                    available,
                    observed_at: now_ms,
                    session_id: record.session_id.clone(),
                    execution_id: record.id.clone(),
                    cursor: record.observed_end.unwrap_or(record.log_start),
                    source: "untrusted-target-output",
                    stale: false,
                },
            );
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Downloads (web/download_plan.js)
// ---------------------------------------------------------------------------

/// `validateDownload`: http(s), no embedded credentials, 64 hex digits.
pub fn validate_download(url: &str, sha256: &str) -> Result<(String, String), String> {
    let parsed = reqwest::Url::parse(url)
        .map_err(|_| "Download requires an HTTP(S) URL without credentials.".to_string())?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err("Download requires an HTTP(S) URL without credentials.".to_string());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("Download requires an HTTP(S) URL without credentials.".to_string());
    }
    let hash = sha256.trim();
    if !hash.is_empty() && (hash.len() != 64 || !hash.chars().all(|c| c.is_ascii_hexdigit())) {
        return Err("Expected SHA-256 must contain 64 hexadecimal characters.".to_string());
    }
    Ok((parsed.to_string(), hash.to_lowercase()))
}

/// The plan a completed `probe_download_tools` unlocks.
#[derive(Debug, Clone, PartialEq)]
pub struct DownloadPlan {
    pub command: String,
    pub metadata: DownloadMeta,
}

/// Probe evidence → the tools that exist on the target.
pub fn probe_downloads(evidence: &str) -> (Option<String>, Option<String>) {
    let mut downloader = None;
    let mut checksum = None;
    for line in evidence.replace('\r', "").split('\n') {
        let Some(name) = line.strip_prefix("LINKR_TOOL:") else {
            continue;
        };
        match name {
            "curl" if downloader.is_none() => downloader = Some("curl".to_string()),
            "wget" if downloader.is_none() => downloader = Some("wget".to_string()),
            "sha256sum" | "shasum" | "openssl" if checksum.is_none() => {
                checksum = Some(name.to_string())
            }
            _ => {}
        }
    }
    (downloader, checksum)
}

/// `targetDownloadPlan(args, probe)`.
pub fn target_download_plan(
    url: &str,
    sha256: &str,
    path: &str,
    probe: Option<&ExecutionRecord>,
) -> Result<DownloadPlan, String> {
    let (url, sha256) = validate_download(url, sha256)?;
    let ready = probe.is_some_and(|record| {
        record.execution_status == "completed"
            && record.exit_code == Some(0)
            && !record.evidence_truncated
    });
    if !ready {
        return Err(
            "Complete probe_download_tools and inspect its result before downloading.".to_string(),
        );
    }
    let evidence = probe.map(|record| record.evidence.as_str()).unwrap_or("");
    let (downloader, checksum) = probe_downloads(evidence);
    let (Some(downloader), Some(checksum)) = (downloader, checksum) else {
        return Err(
            "Target needs curl/wget and a SHA-256 tool. Report missing tools before choosing another action."
                .to_string(),
        );
    };
    if !path.starts_with('/')
        || path.ends_with('/')
        || path.chars().any(|c| (c as u32) < 0x20 || c as u32 == 0x7f)
    {
        return Err(
            "Target destination must be an absolute file path without control characters."
                .to_string(),
        );
    }
    let quoted_path = quote_shell(path).map_err(|error| error.to_string())?;
    let quoted_url = quote_shell(&url).map_err(|error| error.to_string())?;
    let fetch = if downloader == "curl" {
        "curl -fL --progress-bar -o \"$p\" "
    } else {
        "wget -O \"$p\" "
    };
    let hash = match checksum.as_str() {
        "sha256sum" => "sha256sum \"$p\"",
        "shasum" => "shasum -a 256 \"$p\"",
        _ => "openssl dgst -sha256 \"$p\"",
    };
    let strip = if checksum == "openssl" {
        "##* "
    } else {
        "%% *"
    };
    let verify = if sha256.is_empty() {
        String::new()
    } else {
        format!(
            "test \"$h\" = {} || exit 65; ",
            quote_shell(&sha256).map_err(|error| error.to_string())?
        )
    };
    let command = format!(
        "d={quoted_path}; test ! -e \"$d\" || exit 73; p=$(mktemp \"$d.part.XXXXXX\") || exit; printf 'LINKR_PART:%s\n' \"$p\"; {fetch}{quoted_url} || exit; h=$({hash}) || exit; h=${{h{strip}}}; printf '\nLINKR_SHA256:%s\n' \"$h\"; {verify}ln \"$p\" \"$d\" || exit; rm \"$p\"; printf 'LINKR_BYTES:'; wc -c < \"$d\""
    );
    Ok(DownloadPlan {
        command,
        metadata: DownloadMeta {
            destination: "target".to_string(),
            path: path.to_string(),
            url,
            downloader,
            checksum,
            expected_sha256: if sha256.is_empty() {
                None
            } else {
                Some(sha256)
            },
            status: None,
            partial_path: None,
            sha256: None,
            bytes: None,
        },
    })
}

// ---------------------------------------------------------------------------
// Device profile (web/device_profile.js)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DeviceProfile {
    pub system: String,
    pub os: String,
    pub model: String,
    #[serde(rename = "bootId")]
    pub boot_id: String,
    pub storage: String,
    pub tools: Vec<String>,
    pub observed_at: u64,
    pub source: &'static str,
}

/// `parseDeviceProfile`: markers are matched after CRLF normalization.
pub fn parse_device_profile(text: &str, now_ms: u64) -> Option<DeviceProfile> {
    let normalized = text.replace('\r', "");
    let start = normalized.find("LINKR_PROFILE_BEGIN\n")?;
    let body = &normalized[start + "LINKR_PROFILE_BEGIN\n".len()..];
    let end = body.find("\nLINKR_PROFILE_END\n")?;
    let clean: String = body[..end].replace('\0', "");
    let field = |from: &str, to: &str| -> String {
        // JS: `clean.split(a + '\n')[1]?.split(b)[0]` — the text *after* the marker.
        let Some((_, after)) = clean.split_once(&format!("{from}\n")) else {
            return String::new();
        };
        after
            .split(to)
            .next()
            .unwrap_or("")
            .trim()
            .chars()
            .take(1200)
            .collect()
    };
    let system = clean
        .split("\nLINKR_OS")
        .next()
        .unwrap_or("")
        .chars()
        .take(500)
        .collect();
    let mut tools = Vec::new();
    for line in clean.split('\n') {
        if let Some(name) = line.strip_prefix("TOOL:") {
            tools.push(name.to_string());
        }
    }
    Some(DeviceProfile {
        system,
        os: field("LINKR_OS", "LINKR_MODEL"),
        model: field("LINKR_MODEL", "LINKR_BOOT"),
        boot_id: field("LINKR_BOOT", "LINKR_DISK"),
        storage: field("LINKR_DISK", "LINKR_TOOLS"),
        tools,
        observed_at: now_ms,
        source: "untrusted-target-output",
    })
}

// ---------------------------------------------------------------------------
// Approval requests
// ---------------------------------------------------------------------------

/// Decide whether a serial send must go through the broker, in the exact order
/// of `web/device_executor.js`.
pub fn needs_approval(
    mode: super::ExecMode,
    text: &str,
    append_enter: bool,
    payload: &str,
    input_pending: bool,
    console_kind: &str,
    policy: Option<&CommandPolicy>,
) -> bool {
    use super::policy::requires_input_approval;
    if requires_input_approval(mode, text, append_enter, payload, input_pending, policy) {
        return true;
    }
    // In Auto an unobserved console is not a known shell: ask first.
    mode == super::ExecMode::Auto && console_kind != "shell"
}

/// Guard called before every send (`device_executor.js` `check`).
pub fn check_send(
    connected: bool,
    status_session: &str,
    record_session: &str,
    status_revision: u64,
    record_revision: u64,
    console_changed: bool,
) -> Result<(), String> {
    if !connected || status_session != record_session {
        return Err(ERR_SESSION_CHANGED.to_string());
    }
    if status_revision != record_revision {
        return Err(ERR_INPUT_REVISION.to_string());
    }
    if console_changed {
        return Err(ERR_CONSOLE_CHANGED.to_string());
    }
    Ok(())
}

/// Argument validation for `send_serial_input` — the text half of
/// `web/device_executor.js:228`. `appendEnter` is validated where the
/// arguments are read, because there it can still tell "absent" from "false".
pub fn validate_input(text: &str) -> Result<(), String> {
    if text.is_empty() || text.chars().count() > MAX_INPUT_CHARS {
        return Err(ERR_INVALID_INPUT.to_string());
    }
    Ok(())
}

/// Command length budget for `run_shell_command`.
pub fn validate_command(command: &str) -> Result<(), String> {
    if command.is_empty() || command.chars().count() > MAX_COMMAND_CHARS {
        return Err(ERR_INVALID_INPUT.to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::ExecMode;

    const DOWNLOAD_JS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../web/download_plan.js");
    const PROFILE_JS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../web/device_profile.js");
    const OBSERVATION_JS: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../web/serial_observation.js"
    );
    const EXECUTOR_JS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../web/device_executor.js");

    fn read(path: &str) -> String {
        std::fs::read_to_string(path).expect("contract source")
    }

    #[test]
    fn probes_match_the_js_sources() {
        // The JS sources are template literals: a backslash in the runtime
        // command is written twice in the file.
        let download = read(DOWNLOAD_JS);
        assert!(
            download.contains(&DOWNLOAD_PROBE.replace('\\', "\\\\")),
            "DOWNLOAD_PROBE drifted"
        );
        let profile = read(PROFILE_JS);
        assert!(profile.contains("printf 'LINKR_PROFILE_BEGIN\\\\n'; uname -a"));
        assert!(profile
            .contains("for t in sh curl wget sha256sum shasum openssl sudo systemctl busybox; do"));
        // The probe is a JS string literal: `\\n` in the source is `\n` in the
        // emitted command, which is what the shell must see.
        assert!(PROFILE_PROBE.contains("\\n"));
        assert!(!PROFILE_PROBE.contains('\n'));
        assert!(!DOWNLOAD_PROBE.contains('\n'));
    }

    #[test]
    fn constants_match_the_spec_table() {
        assert_eq!(MAX_EXECUTION_RECORDS, 50);
        assert_eq!(APPROVAL_STALE_MS, 900_000);
        assert_eq!(EXECUTION_STALE_MS, 300_000);
        assert_eq!(FULL_AUTO_WINDOW_MS, 900_000);
        assert_eq!(MAX_COMMAND_CHARS, 1024);
        assert_eq!(MAX_INPUT_CHARS, 2048);
        let executor = read(EXECUTOR_JS);
        assert!(executor.contains("const FULL_AUTO_WINDOW_MS = 15 * 60 * 1000;"));
        assert!(executor.contains("if (records.length > 50) records.shift();"));
        let observation = read(OBSERVATION_JS);
        assert!(observation.contains("timeoutMs = Math.max(100, Math.min(5000, timeoutMs));"));
        assert!(observation.contains("settleMs = Math.max(100, Math.min(1000, settleMs));"));
        assert!(observation.contains("Math.max(100, Math.min(60000, timeoutMs))"));
        assert_eq!(clamp_wait_timeout(0), 100);
        assert_eq!(clamp_wait_timeout(9999), 5000);
        assert_eq!(clamp_wait_timeout(1200), 1200);
        assert_eq!(clamp_settle(50), 100);
        assert_eq!(clamp_settle(4000), 1000);
        assert_eq!(clamp_monitor_timeout(None), 30_000);
        assert_eq!(clamp_monitor_timeout(Some(60_001)), 60_000);
        assert_eq!(clamp_monitor_timeout(Some(50)), 100);
    }

    #[test]
    fn exact_error_strings() {
        assert_eq!(
            rejected_message("send_serial_input"),
            "The user rejected send_serial_input. Do not retry this action; ask for a different approach or stop."
        );
        assert_eq!(ERR_APPROVAL_GONE, "Approval not pending or expired.");
        assert_eq!(
            ERR_QUEUE_FULL,
            "Queue is full (8 messages). Clear pending messages or wait."
        );
        assert_eq!(
            ERR_ALREADY_RUNNING,
            "Agent is already processing. Wait for the current run to stop."
        );
        assert_eq!(
            ERR_SESSION_MOVED,
            "Device session or mode changed. Start a new conversation."
        );
        assert_eq!(ERR_NO_ACTIVE_TASK, "No active task. Send a new question.");
        assert_eq!(ERR_QUEUE_ITEM, "Invalid queued message.");
        assert_eq!(
            ERR_TRACKED_SHELL,
            "Tracked commands require an idle, observed POSIX shell prompt."
        );
        assert_eq!(
            ERR_CANCELLED,
            "Operation cancelled. Do not retry automatically."
        );
        assert_eq!(
            NEXT_UNRESOLVED,
            "Execution is unresolved. Monitor this same id again; do not resend the command."
        );
        assert_eq!(
            NEXT_AWAITING_INPUT,
            "Target is waiting for interaction. Report the prompt; passwords must be entered by the user directly in the terminal."
        );
        assert_eq!(
            NEXT_INSPECT,
            "Call inspect_serial_execution with this id to inspect subsequent output. Delivery alone is not command success."
        );
    }

    #[test]
    fn tracked_wrapper_shape() {
        let token = "LINKR_EXIT_abc123";
        let wire = tracked_command("ls -l", token).unwrap();
        assert_eq!(
            wire,
            "sh -c 'ls -l'; printf '\\n%s:%s\\n' 'LINKR_EXIT_abc123' \"$?\""
        );
        // Quotes in the command survive shell quoting.
        let wire = tracked_command("echo 'hi'", token).unwrap();
        assert!(wire.starts_with("sh -c 'echo '\\''hi'\\'''; printf "));
        // Overlong wrappers are rejected with the exact message.
        let long = "x".repeat(2100);
        assert_eq!(
            tracked_command(&long, token).unwrap_err(),
            ERR_TRACKED_TOO_LONG
        );
    }

    #[test]
    fn exit_marker_matching() {
        let token = "LINKR_EXIT_deadbeef";
        let evidence = format!("before\n{token}:0\nafter");
        assert_eq!(exit_code_from(&evidence, token), Some(0));
        assert_eq!(exit_code_from(&format!("{token}:12\n"), token), Some(12));
        assert_eq!(exit_code_from(&format!("{token}:255\n"), token), Some(255));
        assert_eq!(exit_code_from(&format!("{token}:256\n"), token), None);
        // A token only counts at the start of a line.
        assert_eq!(exit_code_from(&format!("x{token}:0\n"), token), None);
        assert_eq!(exit_code_from(&format!("{token}:0"), token), Some(0));
        assert_eq!(exit_code_from("no marker\n", token), None);
        assert_eq!(exit_code_from(&format!("{token}:12x\n"), token), None);
    }

    #[test]
    fn probe_tools_validation_and_command() {
        let names = vec!["dd".to_string(), "base64".to_string()];
        assert_eq!(validate_tool_names(&names).unwrap(), names);
        assert_eq!(validate_tool_names(&[]).unwrap_err(), "Invalid tool names");
        assert_eq!(
            validate_tool_names(&["-bad".to_string()]).unwrap_err(),
            "Invalid tool names"
        );
        let too_many: Vec<String> = (0..17).map(|i| format!("t{i}")).collect();
        assert_eq!(
            validate_tool_names(&too_many).unwrap_err(),
            "Invalid tool names"
        );
        // De-duplication.
        let dupes = vec!["dd".to_string(), "dd".to_string()];
        assert_eq!(validate_tool_names(&dupes).unwrap(), vec!["dd".to_string()]);
        let command = probe_tools_command(&names);
        assert_eq!(
            command,
            "for t in 'dd' 'base64'; do if command -v \"$t\" >/dev/null 2>&1; then printf 'TOOL:%s:available\\n' \"$t\"; else printf 'TOOL:%s:missing\\n' \"$t\"; fi; done"
        );
        assert_eq!(
            tools_announcement(&["dd".to_string(), "base64".to_string()]),
            "Tools now available: dd, base64."
        );
    }

    #[test]
    fn tool_probe_requires_clean_completion() {
        let names = vec!["dd".to_string(), "base64".to_string()];
        let evidence = "TOOL:dd:available\nTOOL:base64:missing\n";
        assert_eq!(
            parse_tool_probe(evidence, &names, Some(0), false),
            Some(vec![
                ("dd".to_string(), true),
                ("base64".to_string(), false)
            ])
        );
        assert_eq!(parse_tool_probe(evidence, &names, Some(1), false), None);
        assert_eq!(parse_tool_probe(evidence, &names, Some(0), true), None);
        assert_eq!(
            parse_tool_probe("TOOL:dd:available\n", &names, Some(0), false),
            None
        );
        assert_eq!(
            parse_tool_probe(
                "TOOL:dd:available\nTOOL:dd:available\nTOOL:base64:missing\n",
                &names,
                Some(0),
                false
            ),
            None
        );
    }

    #[test]
    fn record_ring_caps_at_fifty() {
        let mut store = ExecutionStore::new();
        for index in 0..60 {
            let id = store.allocate_id();
            store.push(ExecutionRecord::new(
                id,
                "session".into(),
                1,
                "auto",
                "send_serial_input",
                "cmd".into(),
                "cmd\r".into(),
                true,
                "shell",
                index,
            ));
        }
        assert_eq!(store.len(), MAX_EXECUTION_RECORDS);
        assert_eq!(store.records()[0].id, "serial-11");
        assert_eq!(store.records()[49].id, "serial-60");
        assert!(store.get("serial-1").is_none());
        assert!(store.get("serial-60").is_some());
    }

    #[test]
    fn staleness_marks_old_pending_records() {
        let mut store = ExecutionStore::new();
        let id = store.allocate_id();
        store.push(ExecutionRecord::new(
            id.clone(),
            "session".into(),
            1,
            "auto",
            "send_serial_input",
            "cmd".into(),
            "cmd\r".into(),
            true,
            "shell",
            1_000,
        ));
        store.get_mut(&id).unwrap().delivery = "sent".into();
        store.apply_staleness(1_000 + EXECUTION_STALE_MS);
        assert_eq!(store.get(&id).unwrap().execution_status, "unknown");
        store.apply_staleness(1_000 + EXECUTION_STALE_MS + 1);
        assert_eq!(store.get(&id).unwrap().execution_status, "stalled");
    }

    #[test]
    fn foreign_sessions_close_their_records() {
        let mut store = ExecutionStore::new();
        let id = store.allocate_id();
        store.push(ExecutionRecord::new(
            id.clone(),
            "old".into(),
            1,
            "auto",
            "send_serial_input",
            "cmd".into(),
            "cmd\r".into(),
            true,
            "shell",
            0,
        ));
        store.get_mut(&id).unwrap().delivery = "sent".into();
        store.close_foreign("new");
        let record = store.get(&id).unwrap();
        assert!(record.observation_closed);
        assert_eq!(record.observation, "interrupted");
    }

    // --- observation ------------------------------------------------------

    #[test]
    fn wait_decide_statuses() {
        // No new output yet: no-output, not timed out.
        let (stop, outcome) = wait_decide(100, 100, 0, 0, 0, 100, 5000, 400);
        assert!(!stop);
        assert_eq!(outcome.wait_status, "no-output");
        // New output that has been quiet long enough: settled.
        let (stop, _outcome) = wait_decide(100, 200, 0, 0, 0, 100, 5000, 400);
        assert!(!stop);
        let (stop, outcome) = wait_decide(100, 200, 500, 0, 0, 200, 5000, 400);
        assert!(stop);
        assert_eq!(outcome.wait_status, "settled");
        assert_eq!(outcome.quiet_for_ms, 500);
        // Output still arriving: streaming.
        let (stop, outcome) = wait_decide(100, 200, 100, 0, 0, 200, 5000, 400);
        assert!(!stop);
        assert_eq!(outcome.wait_status, "streaming");
        // Deadline first: timed out.
        let (stop, outcome) = wait_decide(100, 100, 5000, 0, 0, 100, 5000, 400);
        assert!(stop);
        assert!(outcome.timed_out);
        assert_eq!(outcome.wait_status, "no-output");
    }

    #[test]
    fn monitor_decide_statuses() {
        let base = monitor_decide("unknown", false, "sent", None, 0, 1000);
        assert!(!base.done && !base.timed_out);
        let done = monitor_decide("completed", false, "sent", None, 0, 1000);
        assert!(done.done && !done.timed_out);
        let closed = monitor_decide("unknown", true, "sent", None, 0, 1000);
        assert!(closed.done && !closed.timed_out);
        let unsent = monitor_decide("unknown", false, "not-sent", None, 0, 1000);
        assert!(unsent.done && !unsent.timed_out);
        let waiting = monitor_decide("unknown", false, "sent", Some("password"), 0, 1000);
        assert!(waiting.done);
        assert_eq!(waiting.wait_status, Some("awaiting-input"));
        assert_eq!(waiting.next, Some(NEXT_AWAITING_INPUT));
        let expired = monitor_decide("unknown", false, "sent", None, 1000, 1000);
        assert!(expired.done && expired.timed_out);
        assert_eq!(expired.next, Some(NEXT_UNRESOLVED));
        assert_eq!(monitor_poll_ms(900), 500);
        assert_eq!(monitor_poll_ms(100), 100);
        assert_eq!(wait_poll_ms(900), 50);
        assert_eq!(wait_poll_ms(10), 10);
    }

    // --- a fake journal for paging ---------------------------------------

    struct FakeLog {
        text: String,
        start: u64,
        latest: u64,
    }

    impl LogReader for FakeLog {
        fn read(&self, after: Option<u64>, limit: usize) -> JournalRead {
            // `web/serial_journal.js`: `truncated: requested < oldest`.
            let limit = limit.max(1);
            let requested = after.unwrap_or_else(|| self.latest.saturating_sub(limit as u64));
            let truncated = requested < self.start;
            let from = requested.saturating_sub(self.start) as usize;
            let chars: Vec<char> = self.text.chars().collect();
            let from = from.min(chars.len());
            let take = limit.min(chars.len() - from);
            let text: String = chars[from..from + take].iter().collect();
            JournalRead {
                start: self.start + from as u64,
                cursor: self.start + (from + take) as u64,
                latest: self.latest,
                text,
                truncated,
                updated_at: 0,
            }
        }
        fn latest_cursor(&self) -> u64 {
            self.latest
        }
    }

    fn record(log_start: u64) -> ExecutionRecord {
        let mut record = ExecutionRecord::new(
            "serial-1".into(),
            "session".into(),
            1,
            "auto",
            "send_serial_input",
            "cmd".into(),
            "cmd\r".into(),
            true,
            "shell",
            0,
        );
        record.delivery = "sent".into();
        record.log_start = log_start;
        record
    }

    #[test]
    fn execution_page_never_crosses_the_record_end() {
        let log = FakeLog {
            text: "abcdefghij".into(),
            start: 10,
            latest: 20,
        };
        let mut record = record(12);
        record.observed_end = Some(16);
        let page = execution_page(&record, &log, None, 100);
        assert_eq!(page.evidence, "cdef");
        assert_eq!(page.evidence_start, 12);
        assert_eq!(page.observed_cursor, 16);
        assert!(!page.has_more);
        // The default window covers the whole record range, and the retained
        // journal reaches back to the record's start: nothing is missing.
        assert!(!page.evidence_truncated);

        let page = execution_page(&record, &log, Some(12), 2);
        assert_eq!(page.evidence, "cd");
        assert_eq!(page.observed_cursor, 14);
        assert!(page.has_more);
        // `cursor < end`: the page stops before the record's end.
        assert!(page.evidence_truncated);
    }

    #[test]
    fn refresh_classifies_evidence_and_settles_the_exit_marker() {
        let mut record = record(0);
        record.observed_end = Some(20);
        record.completion_token = Some("LINKR_EXIT_tok".into());
        let log = FakeLog {
            text: "prompt$ ls\nDONE\nLINKR_EXIT_tok:0\n".into(),
            start: 0,
            latest: 33,
        };
        // The refresh path advances observedEnd to the journal's latest cursor
        // before paging (`agent/mod.rs`, `device_executor.js`).
        record.observed_end = Some(log.latest_cursor());
        let page = execution_page(&record, &log, None, 100);
        refresh_record(&mut record, page, 5_000, true, true);
        assert_eq!(record.execution_status, "completed");
        assert_eq!(record.exit_code, Some(0));
        assert!(record.observation_closed);
        assert_eq!(record.observation, "output-observed");
        assert_eq!(record.completed_at, Some(5_000));
    }

    #[test]
    fn refresh_marks_interrupted_when_the_session_moved() {
        let mut record = record(0);
        record.observed_end = Some(5);
        record.delivery = "sent".into();
        let log = FakeLog {
            text: "hello".into(),
            start: 0,
            latest: 5,
        };
        let page = execution_page(&record, &log, None, 100);
        refresh_record(&mut record, page, 0, true, false);
        assert_eq!(record.observation, "interrupted");
        assert!(record.observation_closed);
    }

    #[test]
    fn download_markers_update_the_metadata() {
        let mut record = record(0);
        record.observed_end = Some(60);
        record.download = Some(DownloadMeta {
            destination: "target".into(),
            path: "/tmp/app.bin".into(),
            url: "https://example.com/app.bin".into(),
            downloader: "curl".into(),
            checksum: "sha256sum".into(),
            expected_sha256: Some("a".repeat(64)),
            status: None,
            partial_path: None,
            sha256: None,
            bytes: None,
        });
        record.completion_token = Some("LINKR_EXIT_tok".into());
        let text = format!(
            "LINKR_PART:/tmp/app.bin.part.abc\nLINKR_SHA256:{}\nLINKR_BYTES:  4096\nLINKR_EXIT_tok:0\n",
            "AB".repeat(32)
        );
        let log = FakeLog {
            text: text.clone(),
            start: 0,
            latest: text.chars().count() as u64,
        };
        record.observed_end = Some(log.latest_cursor());
        let page = execution_page(&record, &log, None, 4000);
        refresh_record(&mut record, page, 42, true, true);
        let download = record.download.as_ref().unwrap();
        assert_eq!(
            download.partial_path.as_deref(),
            Some("/tmp/app.bin.part.abc")
        );
        assert_eq!(download.sha256.as_deref(), Some("ab".repeat(32).as_str()));
        assert_eq!(download.bytes, Some(4096));
        assert_eq!(download.status.as_deref(), Some("saved"));
    }

    // --- downloads --------------------------------------------------------

    #[test]
    fn download_validation_messages() {
        assert_eq!(
            validate_download("ftp://x/y", "").unwrap_err(),
            "Download requires an HTTP(S) URL without credentials."
        );
        assert_eq!(
            validate_download("https://user:pass@x/y", "").unwrap_err(),
            "Download requires an HTTP(S) URL without credentials."
        );
        assert_eq!(
            validate_download("https://x/y", "zz").unwrap_err(),
            "Expected SHA-256 must contain 64 hexadecimal characters."
        );
        let (url, hash) = validate_download("https://x/y", &"A".repeat(64)).unwrap();
        assert_eq!(url, "https://x/y");
        assert_eq!(hash, "a".repeat(64));
    }

    fn probe_record() -> ExecutionRecord {
        let mut record = record(0);
        record.execution_status = "completed".into();
        record.exit_code = Some(0);
        record.evidence = "LINKR_TOOL:curl\nLINKR_TOOL:sha256sum\n".into();
        record.evidence_truncated = false;
        record
    }

    #[test]
    fn download_plan_requires_a_clean_probe() {
        let mut probe = probe_record();
        probe.execution_status = "unknown".into();
        assert_eq!(
            target_download_plan("https://x/y", "", "/tmp/z", Some(&probe)).unwrap_err(),
            "Complete probe_download_tools and inspect its result before downloading."
        );
        assert_eq!(
            target_download_plan("https://x/y", "", "/tmp/z", None).unwrap_err(),
            "Complete probe_download_tools and inspect its result before downloading."
        );
        let mut probe = probe_record();
        probe.exit_code = Some(1);
        assert_eq!(
            target_download_plan("https://x/y", "", "/tmp/z", Some(&probe)).unwrap_err(),
            "Complete probe_download_tools and inspect its result before downloading."
        );
        let mut probe = probe_record();
        probe.evidence = "LINKR_TOOL:curl\n".into();
        assert_eq!(
            target_download_plan("https://x/y", "", "/tmp/z", Some(&probe)).unwrap_err(),
            "Target needs curl/wget and a SHA-256 tool. Report missing tools before choosing another action."
        );
        let probe = probe_record();
        assert_eq!(
            target_download_plan("https://x/y", "", "relative", Some(&probe)).unwrap_err(),
            "Target destination must be an absolute file path without control characters."
        );
        assert_eq!(
            target_download_plan("https://x/y", "", "/tmp/", Some(&probe)).unwrap_err(),
            "Target destination must be an absolute file path without control characters."
        );
    }

    #[test]
    fn download_plan_command_shape() {
        let probe = probe_record();
        let plan = target_download_plan(
            "https://example.com/app.bin",
            &"ab".repeat(32),
            "/tmp/app.bin",
            Some(&probe),
        )
        .unwrap();
        assert_eq!(
            plan.command,
            format!(
                "d='/tmp/app.bin'; test ! -e \"$d\" || exit 73; p=$(mktemp \"$d.part.XXXXXX\") || exit; printf 'LINKR_PART:%s\n' \"$p\"; curl -fL --progress-bar -o \"$p\" 'https://example.com/app.bin' || exit; h=$(sha256sum \"$p\") || exit; h=${{h%% *}}; printf '\nLINKR_SHA256:%s\n' \"$h\"; test \"$h\" = '{}' || exit 65; ln \"$p\" \"$d\" || exit; rm \"$p\"; printf 'LINKR_BYTES:'; wc -c < \"$d\"",
                "ab".repeat(32)
            )
        );
        assert_eq!(plan.metadata.destination, "target");
        assert_eq!(plan.metadata.downloader, "curl");
        assert_eq!(plan.metadata.checksum, "sha256sum");
        assert_eq!(
            plan.metadata.expected_sha256.as_deref(),
            Some("abababababababababababababababababababababababababababababababab")
        );
        assert_eq!(plan.metadata.path, "/tmp/app.bin");

        // wget + shasum + no expectation takes the other branches.
        let mut probe = probe_record();
        probe.evidence = "LINKR_TOOL:wget\nLINKR_TOOL:shasum\n".into();
        let plan =
            target_download_plan("https://example.com/a", "", "/tmp/a", Some(&probe)).unwrap();
        assert!(plan
            .command
            .contains("wget -O \"$p\" 'https://example.com/a'"));
        assert!(plan.command.contains("h=$(shasum -a 256 \"$p\")"));
        assert!(plan.command.contains("h=${h%% *}"));
        assert!(!plan.command.contains("exit 65"));
        assert_eq!(plan.metadata.expected_sha256, None);

        // openssl strips the "SHA2-256= " prefix with ##* .
        let mut probe = probe_record();
        probe.evidence = "LINKR_TOOL:curl\nLINKR_TOOL:openssl\n".into();
        let plan =
            target_download_plan("https://example.com/a", "", "/tmp/a", Some(&probe)).unwrap();
        assert!(plan.command.contains("h=$(openssl dgst -sha256 \"$p\")"));
        assert!(plan.command.contains("h=${h##* }"));
    }

    #[test]
    fn probe_downloads_picks_the_first_of_each_kind() {
        let (downloader, checksum) =
            probe_downloads("LINKR_TOOL:sha256sum\nLINKR_TOOL:curl\nLINKR_TOOL:wget\n");
        assert_eq!(downloader.as_deref(), Some("curl"));
        assert_eq!(checksum.as_deref(), Some("sha256sum"));
        let (downloader, checksum) = probe_downloads("nothing here");
        assert_eq!((downloader, checksum), (None, None));
    }

    // --- profile ----------------------------------------------------------

    #[test]
    fn profile_probe_parses_every_field() {
        let text = "LINKR_PROFILE_BEGIN\r\nLinux 6.8 armv7l\r\nLINKR_OS\r\nNAME=Buildroot\r\nVERSION=2024\r\nLINKR_MODEL\r\nRaspberry Pi 4\r\n\r\nLINKR_BOOT\r\nabcd-1234\r\nLINKR_DISK\r\n/dev/root 1000 500 500 50% /\r\nLINKR_TOOLS\r\nTOOL:sh\r\nTOOL:curl\r\nLINKR_PROFILE_END\r\n";
        let profile = parse_device_profile(text, 7).unwrap();
        assert_eq!(profile.os, "NAME=Buildroot\nVERSION=2024");
        assert_eq!(profile.model, "Raspberry Pi 4");
        assert_eq!(profile.boot_id, "abcd-1234");
        assert_eq!(profile.storage, "/dev/root 1000 500 500 50% /");
        assert_eq!(profile.tools, vec!["sh", "curl"]);
        assert_eq!(profile.source, "untrusted-target-output");
        assert_eq!(profile.observed_at, 7);
        assert!(profile.system.starts_with("Linux 6.8 armv7l"));
        assert_eq!(parse_device_profile("no markers", 0), None);
    }

    // --- approval decisions ------------------------------------------------

    #[test]
    fn approval_needs_follow_the_mode_table() {
        let policy = None;
        // Manual always asks.
        assert!(needs_approval(
            ExecMode::Manual,
            "ls -l",
            true,
            "ls -l\r",
            false,
            "shell",
            policy
        ));
        // Auto sends an exact query only at an observed shell.
        assert!(!needs_approval(
            ExecMode::Auto,
            "ls -l",
            true,
            "ls -l\r",
            false,
            "shell",
            policy
        ));
        // Auto at an unknown console asks even for a query.
        assert!(needs_approval(
            ExecMode::Auto,
            "ls -l",
            true,
            "ls -l\r",
            false,
            "unknown",
            policy
        ));
        // Full Auto sends without asking at a shell.
        assert!(!needs_approval(
            ExecMode::FullAuto,
            "reboot",
            true,
            "reboot\r",
            false,
            "shell",
            policy
        ));
        // A guarded command always asks, even in Full Auto.
        assert!(needs_approval(
            ExecMode::FullAuto,
            "rm -rf /tmp/x",
            true,
            "rm -rf /tmp/x\r",
            false,
            "shell",
            policy
        ));
        // The user's always-ask list outranks Full Auto.
        let always_reboot = CommandPolicy {
            always_ask: vec!["reboot".into()],
            allow: vec![],
        };
        let policy = Some(&always_reboot);
        assert!(needs_approval(
            ExecMode::FullAuto,
            "reboot",
            true,
            "reboot\r",
            false,
            "shell",
            policy
        ));
    }

    #[test]
    fn send_guards_report_the_exact_reason() {
        assert_eq!(
            check_send(false, "a", "a", 1, 1, false).unwrap_err(),
            ERR_SESSION_CHANGED
        );
        assert_eq!(
            check_send(true, "new", "old", 1, 1, false).unwrap_err(),
            ERR_SESSION_CHANGED
        );
        assert_eq!(
            check_send(true, "a", "a", 2, 1, false).unwrap_err(),
            ERR_INPUT_REVISION
        );
        assert_eq!(
            check_send(true, "a", "a", 1, 1, true).unwrap_err(),
            ERR_CONSOLE_CHANGED
        );
        assert!(check_send(true, "a", "a", 1, 1, false).is_ok());
        assert_eq!(validate_input("").unwrap_err(), ERR_INVALID_INPUT);
        assert!(validate_input("x").is_ok());
        assert_eq!(validate_command("").unwrap_err(), ERR_INVALID_INPUT);
        assert_eq!(
            validate_command(&"c".repeat(MAX_COMMAND_CHARS + 1)).unwrap_err(),
            ERR_INVALID_INPUT
        );
    }

    #[test]
    fn capability_map_reads_completed_probes() {
        let mut store = ExecutionStore::new();
        let id = store.allocate_id();
        let mut record = record(0);
        record.id = id;
        record.tool_probe = Some(vec!["dd".to_string(), "base64".to_string()]);
        // JS updates the capability map only inside the settled
        // `if (record.completionToken)` branch, after the exit marker matched.
        record.completion_token = Some("LINKR_EXIT_tok".into());
        record.exit_code = Some(0);
        record.observed_end = Some(40);
        record.evidence = "TOOL:dd:available\nTOOL:base64:available\n".into();
        store.push(record);
        let map = capability_map(&store, 1_000);
        assert_eq!(map.len(), 2);
        assert!(map["dd"].available);
        assert_eq!(map["dd"].execution_id, store.records()[0].id);
        assert_eq!(map["dd"].source, "untrusted-target-output");
        assert!(!map["dd"].stale);
    }

    #[test]
    fn report_records_round_trip() {
        let mut record = record(0);
        record.delivery = "sent".into();
        record.execution_status = "completed".into();
        record.exit_code = Some(0);
        let value = record.to_json();
        assert_eq!(value["id"], json!("serial-1"));
        assert_eq!(value["delivery"], json!("sent"));
        assert_eq!(value["exitCode"], json!(0));
        assert_eq!(value["executionStatus"], json!("completed"));
    }
}
