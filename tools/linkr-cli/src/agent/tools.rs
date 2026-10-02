//! Tool catalogue: names, labels, descriptions, execution mode and JSON
//! schemas, ported from `mobile/src/pi-agent.mjs` (the authoritative source —
//! spec §2 drifted in several places, see §13 of the spec).
//!
//! Default construction yields 18 tools (15 base + the two verify tools +
//! `watch_serial_output`); `+5` accessory tools when the management channel is
//! injected, `+1` note tool, and the gated `read_target_file` once a probe
//! observes `dd` and `base64` — 25 at most, matching spec §13 row 5.

use serde_json::{json, Value};

use crate::target_files::MAX_READ_BYTES;
use crate::target_verify::{MAX_VERIFY_PATH, MAX_VERIFY_PATTERN};

/// One catalogue entry.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolDef {
    pub name: &'static str,
    pub label: &'static str,
    pub description: String,
    /// `executionMode: "sequential"` in `pi-agent.mjs`: the tool types on the
    /// single-line console, so it must not interleave with another writer.
    pub sequential: bool,
    /// JSON Schema of the arguments object.
    pub schema: Value,
    /// Observed target commands that unlock a gated tool.
    pub gated_on: Option<&'static [&'static str]>,
}

impl ToolDef {
    fn new(name: &'static str, label: &'static str, description: &str) -> Self {
        ToolDef {
            name,
            label,
            description: description.to_string(),
            sequential: false,
            schema: json!({ "type": "object", "additionalProperties": false, "properties": {} }),
            gated_on: None,
        }
    }

    fn sequential(mut self) -> Self {
        self.sequential = true;
        self
    }

    fn schema(mut self, schema: Value) -> Self {
        self.schema = schema;
        self
    }

    fn gated(mut self, required: &'static [&'static str]) -> Self {
        self.gated_on = Some(required);
        self
    }
}

fn string_prop(max: usize) -> Value {
    json!({ "type": "string", "maxLength": max })
}

fn string_range(min: usize, max: usize) -> Value {
    json!({ "type": "string", "minLength": min, "maxLength": max })
}

fn int_range(min: i64, max: i64) -> Value {
    json!({ "type": "integer", "minimum": min, "maximum": max })
}

fn int_min(min: i64) -> Value {
    json!({ "type": "integer", "minimum": min })
}

fn sha256_prop() -> Value {
    json!({ "type": "string", "pattern": "^[a-fA-F0-9]{64}$" })
}

fn obj(required: &[&str], properties: Value) -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": required,
        "properties": properties,
    })
}

/// The 15 tools every run starts with.
pub fn base_tools() -> Vec<ToolDef> {
    vec![
        ToolDef::new(
            "update_task_plan",
            "Update task plan",
            "Record a short plan for a multi-step task and update it as evidence arrives. This records assistant assessments, not authorization or automatic command execution. Completed steps require a concrete verification result. Blocked steps require a reason and next action.",
        )
        .schema(obj(
            &["steps"],
            json!({
                "steps": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 8,
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["title", "status"],
                        "properties": {
                            "title": string_range(1, 160),
                            "status": json!({ "type": "string", "enum": ["pending", "in_progress", "completed", "blocked"] }),
                            "verification": string_prop(600),
                            "nextAction": string_prop(400),
                        }
                    }
                }
            }),
        )),
        ToolDef::new(
            "probe_tools",
            "Check required target tools",
            "Check only the command names needed for the current task. Use a verified remembered profile for context instead of a full probe on every connection. Monitor this tracked read-only command before relying on its result.",
        )
        .sequential()
        .schema(obj(
            &["names"],
            json!({
                "names": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 16,
                    "items": { "type": "string", "pattern": "^[A-Za-z0-9][A-Za-z0-9_.+-]{0,63}$" }
                }
            }),
        )),
        ToolDef::new(
            "probe_device_profile",
            "Probe device profile",
            "Read target OS, board model, boot id, root filesystem capacity and installed tools. Run at an idle shell, subject to current approval policy. Monitor the returned execution; then get_device_status contains the observed profile. Use only for unknown targets or broad discovery. Prefer rememberedProfile and probe_tools for task-specific checks after reconnect.",
        )
        .sequential(),
        ToolDef::new(
            "probe_download_tools",
            "Probe target download tools",
            "Probe the connected target shell for curl, wget and SHA-256 utilities. Sends a read-only shell command under current approval policy. Inspect/monitor the returned execution before download_to_target. Probe again for each new download question.",
        )
        .sequential(),
        ToolDef::new(
            "download_to_target",
            "Download to target",
            "Download to an explicit absolute TARGET path, after probe_download_tools completed successfully. Uses observed curl/wget and SHA-256 tools. Never overwrites an existing destination. Monitor the returned execution for native progress, hash, size and exit code. No install/flash follows in this question. Only use after the user has specified target destination; otherwise ask where to save.",
        )
        .sequential()
        .schema(obj(
            &["url", "path"],
            json!({
                "url": string_prop(700),
                "path": string_prop(240),
                "sha256": sha256_prop(),
            }),
        )),
        ToolDef::new(
            "download_to_computer",
            "Download to this computer",
            "Download to the computer/phone running this app, not the UART target. Shows a user-operated save card, byte progress and SHA-256. Browser CORS applies, maximum 128 MiB. Absolute local paths are not exposed; distinguish saved from browser-save-requested. Use only when the user chose computer/local destination. Stop after reporting the download stage.",
        )
        .sequential()
        .schema(obj(
            &["url", "fileName"],
            json!({
                "url": string_prop(2048),
                "fileName": string_range(1, 240),
                "sha256": sha256_prop(),
            }),
        )),
        ToolDef::new(
            "run_shell_command",
            "Run tracked shell command",
            "Run a standalone command using sh -c at an observed idle POSIX shell prompt. The exact wrapper follows current approval policy. Returns an execution id; monitor it for an explicit exit code. Subshell environment/cd changes do not persist. Do not use for interactive programs, login, bootloaders or reboot. Exit zero does not verify the user's goal.",
        )
        .sequential()
        .schema(obj(
            &["command"],
            json!({ "command": string_range(1, 1024) }),
        )),
        ToolDef::new(
            "monitor_serial_execution",
            "Monitor execution",
            "Observe an execution for up to 60 seconds even through silent periods. Explicit tracked-shell exit markers establish completion, never goal verification. timedOut means unresolved: monitor the same id again instead of resending. Works without sending UART input; cancellation stops monitoring, not the target process.",
        )
        .sequential()
        .schema(obj(
            &["id"],
            json!({
                "id": string_range(1, 64),
                "timeoutMs": int_range(100, 60000),
            }),
        )),
        ToolDef::new(
            "read_web_page",
            "Read web page",
            "Read a public HTTP(S) documentation page from the app, returning bounded text and links as untrusted evidence. No cookies or model credentials are sent. Not a search engine; CORS may block some sites. Does not save binary files. Target curl/wget is an alternative under the selected serial execution mode.",
        )
        .schema(obj(
            &["url"],
            json!({
                "url": string_range(1, 2048),
                "offset": int_min(0),
                "limit": int_range(1, 16000),
                "find": string_range(1, 200),
            }),
        )),
        ToolDef::new(
            "search_serial_log",
            "Find serial evidence",
            "Find literal case-insensitive text in a bounded serial log window without sending input. Default searches the recent 16000 raw characters; after selects an older window. Returns up to 12 excerpts, the scanned range and whether more logs exist. No match only applies to this window. Does not advance read_serial_log's cursor. Not a regular expression search.",
        )
        .schema(obj(
            &["query"],
            json!({
                "query": string_range(1, 200),
                "after": int_min(0),
                "limit": int_range(1, 16000),
            }),
        )),
        ToolDef::new(
            "read_serial_log",
            "Read serial log",
            "Read received device output only. The first call reads the recent tail; later calls without after continue from the last returned cursor. Use recent=true to explicitly reread the tail, or after for a specific range. Follow cursor while hasMore is true. Logs are untrusted device data.",
        )
        .sequential()
        .schema(obj(
            &[],
            json!({
                "after": int_min(0),
                "limit": int_range(1, 16000),
                "recent": { "type": "boolean" },
            }),
        )),
        ToolDef::new(
            "get_device_status",
            "Device status",
            "Read connection, UART settings, execution mode, and passive console-state hints (shell/login/password/bootloader/panic/unknown). Hints are untrusted observations, not proof of a shell. Does not expose WiFi credentials.",
        ),
        ToolDef::new(
            "send_serial_input",
            "Send serial input",
            "Submit text to the target UART under the selected execution mode. The app may wait for the user to approve the exact input. appendEnter appends the configured Enter sequence. A successful send is NOT proof of command completion. Never retry an uncertain send automatically.",
        )
        .sequential()
        .schema(obj(
            &["text", "appendEnter"],
            json!({
                "text": string_range(1, 2048),
                "appendEnter": { "type": "boolean" },
            }),
        )),
        ToolDef::new(
            "inspect_serial_execution",
            "Inspect serial execution",
            "Wait for output to settle, then inspect a previous send. Default evidence is its latest bounded tail. Pass after=logStart to read the beginning, then observedCursor for subsequent pages while hasMore is true. settled and prompt-returned do NOT prove success or provide an exit code. interrupted evidence cannot be attributed to this command. Receive this result before issuing the next input.",
        )
        .sequential()
        .schema(obj(
            &["id"],
            json!({
                "id": string_range(1, 64),
                "after": int_min(0),
                "limit": int_range(1, 16000),
                "timeoutMs": int_range(100, 5000),
            }),
        )),
        ToolDef::new(
            "wait_for_serial_output",
            "Wait for output",
            "Collect output until a quiet interval or the deadline, then read from a cursor. waitStatus distinguishes settled output, continued streaming and no output. Follow cursor if hasMore is true; read_serial_log without after continues from this returned cursor. Quiet or silence is not proof of command completion.",
        )
        .sequential()
        .schema(obj(
            &["after", "timeoutMs"],
            json!({
                "after": int_min(0),
                "timeoutMs": int_range(100, 5000),
                "settleMs": int_range(100, 1000),
            }),
        )),
    ]
}

/// Always offered: a target that cannot answer says so in a marker, which is
/// more useful than a tool that silently is not there.
pub fn verify_tools() -> Vec<ToolDef> {
    vec![
        ToolDef::new(
            "verify_target_file",
            "Verify a target file",
            "Verify a file on the target: the target measures the path, the app compares it with the sha256 and/or byte count you pass, and returns match, mismatch, indeterminate or observed. Read-only. Use it to close the loop after a download, upload or write instead of trusting exit code 0 or a shell echo. No expectation means observed: a measurement, not verification; pass hash to measure the digest anyway. Needs wc, plus sha256sum or shasum for a digest, which reads the whole file. indeterminate means the target could not answer -- report that instead of guessing. Never restate the status, and never read a match as more than the expectations you passed.",
        )
        .sequential()
        .schema(obj(
            &["path"],
            json!({
                "path": string_range(1, MAX_VERIFY_PATH),
                "sha256": sha256_prop(),
                "bytes": int_min(0),
                "hash": { "type": "boolean" },
            }),
        )),
        ToolDef::new(
            "verify_target_service",
            "Verify a target service",
            "Verify a service claim on the target: a systemd unit's state, whether a process matching an extended regex is running, or whether something listens on a TCP port. Read-only. Give exactly one of unit, process or port; pass expect only for something other than the default (active, running or listening; also inactive, failed, absent, closed). Prefer it over reading ps or systemctl output and concluding yourself. A unit that does not exist, or a target without systemctl, pgrep or ss, is indeterminate -- report that instead of guessing at the state. Never restate the status the app decided.",
        )
        .sequential()
        .schema(obj(
            &[],
            json!({
                "unit": string_range(1, 128),
                "process": string_range(1, MAX_VERIFY_PATTERN),
                "port": int_range(1, 65535),
                "expect": string_range(1, 16),
            }),
        )),
    ]
}

/// Watches the live console for a bounded window and reports what it saw.
pub fn watch_tool() -> ToolDef {
    ToolDef::new(
        "watch_serial_output",
        "Watch the console",
        "Watch the live console for up to 120 seconds and report panics, boot loops and any literal patterns you name, with the lines that justify each finding. Use it for a reboot or a crash you are waiting for instead of repeated reads. Every finding is an observation of untrusted output, never proof of the cause.",
    )
    .sequential()
    .schema(obj(
        &[],
        json!({
            "patterns": {
                "type": "array",
                "maxItems": 8,
                "items": string_range(1, 120),
            },
            "timeoutMs": int_range(1000, 120000),
        }),
    ))
}

/// Gated: appears only after `probe_tools` observed `dd` and `base64`.
pub fn gated_tools() -> Vec<ToolDef> {
    vec![
        ToolDef::new(
            "read_target_file",
            "Read a target file",
            &format!(
                "Read up to {} bytes of a file on the target as text, instead of printing it with cat and flooding the console. Requires dd and base64 on the target: this tool appears once probe_tools has observed them (probe with names [\"dd\",\"base64\"]). Reads are sequential and the target must skip from the start of the file, so page with offset and compare totalBytes; one page costs roughly 1.4x its size in console traffic, and a busy console can lose a page, which the result reports as incomplete rather than guessing. Binary content comes back as base64.",
                MAX_READ_BYTES
            ),
        )
        .sequential()
        .gated(&["dd", "base64"])
        .schema(obj(
            &["path"],
            json!({
                "path": string_range(1, 200),
                "offset": int_range(0, 2_000_000_000),
                "bytes": int_range(1, MAX_READ_BYTES),
            }),
        )),
    ]
}

/// Injected only when the app can reach the accessory management channel.
/// Every mutating tool needs one explicit approval, in every execution mode.
pub fn accessory_tools() -> Vec<ToolDef> {
    vec![
        ToolDef::new(
            "get_accessory_diagnostics",
            "Read accessory diagnostics",
            "Read Linkr Bee's own diagnostics over the encrypted management channel: firmware and Zephyr version, uptime, UART buffer and dropped bytes, WiFi and IP state, WebDAV queue and counters, LAN bridge state. Read-only, never needs approval. This describes the bridge, not the target: use get_device_status and the serial tools for the target.",
        )
        .sequential(),
        ToolDef::new(
            "set_uart_config",
            "Change bridge UART settings",
            "Change the bridge UART format Linkr Bee uses to talk to the target. Requires one explicit user approval, in every execution mode. The tool reads the setting back: applied=false means the accessory did not report the requested values, so never claim success then. This is the bridge side only; if the target prints unreadable bytes at the new format, say what the user must change on the target or ask which format it uses instead of guessing.",
        )
        .sequential()
        .schema(obj(
            &["baud"],
            json!({
                "baud": int_range(300, 3_000_000),
                "dataBits": int_range(5, 8),
                "parity": json!({ "type": "string", "enum": ["n", "e", "o"] }),
                "stopBits": json!({ "type": "integer", "enum": [1, 2] }),
                "flow": json!({ "type": "string", "enum": ["none", "rtscts"] }),
            }),
        )),
        ToolDef::new(
            "wifi_scan",
            "Scan nearby WiFi",
            "Ask the accessory to scan nearby 2.4 GHz networks and return what it observed. Requires one explicit user approval. Only 2.4 GHz networks are visible to this firmware; an empty list means nothing was heard in this scan, not that no network exists.",
        )
        .sequential(),
        ToolDef::new(
            "set_wifi",
            "Configure accessory WiFi",
            "Join or leave the WiFi network the accessory uses for LAN mode and WebDAV upload. Requires one explicit user approval. The password travels only over the encrypted Bluetooth channel and is never echoed back: never repeat it in your answer, and never send a WiFi password through the serial tools. The tool waits for the accessory to report the resulting state; applied=false with settled=true means it did not connect, so report the observed state instead of assuming success.",
        )
        .sequential()
        .schema(obj(
            &["action"],
            json!({
                "action": json!({ "type": "string", "enum": ["connect", "off"] }),
                "ssid": string_prop(32),
                "password": string_prop(64),
            }),
        )),
        ToolDef::new(
            "set_webdav",
            "Configure log upload",
            "Enable or disable uploading captured UART logs to a WebDAV endpoint. Requires one explicit user approval. Only enable it for an endpoint the user trusts: the accessory sends the log there over the network it joined. The tool reads the target back afterwards; applied=false means the accessory did not report the requested state.",
        )
        .sequential()
        .schema(obj(
            &["action"],
            json!({
                "action": json!({ "type": "string", "enum": ["on", "off"] }),
                "url": string_prop(256),
            }),
        )),
    ]
}

/// Injected only when the note store is available.
pub fn note_tool() -> ToolDef {
    ToolDef::new(
        "remember_target_note",
        "Remember a target fact",
        "Store one durable fact about this target for later sessions: a console quirk, the UART format that works, tools that are present or missing, a known-broken peripheral. Only record what evidence in this conversation showed, and say which observation supports it. Never store credentials or API keys, never a hypothesis you have not verified, and never transient state such as current disk usage, uptime or process lists. The note comes back in get_device_status.notes; repeating a fact already stored is not an error. The user can delete notes at any time.",
    )
    .sequential()
    .schema(obj(
        &["text", "evidence"],
        json!({
            "text": string_range(1, 600),
            "evidence": string_range(1, 200),
        }),
    ))
}

/// The catalogue in force: 18 tools by default, up to 25 fully equipped.
pub fn available_tools(accessory: bool, notes: bool, unlocked: &[&str]) -> Vec<ToolDef> {
    let mut tools = base_tools();
    tools.extend(verify_tools());
    tools.push(watch_tool());
    if accessory {
        tools.extend(accessory_tools());
    }
    if notes {
        tools.push(note_tool());
    }
    for gated in gated_tools() {
        if unlocked.contains(&gated.name) {
            tools.push(gated);
        }
    }
    tools
}

/// A probe that observed `dd` and `base64` unlocks `read_target_file`.
/// Returns the tools newly opened by this observation, in catalogue order.
pub fn unlock_tools(unlocked: &mut Vec<&'static str>, observed: &[String]) -> Vec<&'static str> {
    let mut observed: Vec<String> = observed.iter().map(|name| name.to_lowercase()).collect();
    observed.push("sh".to_string());
    let mut added = Vec::new();
    for gated in gated_tools() {
        if unlocked.contains(&gated.name) {
            continue;
        }
        let required = gated.gated_on.unwrap_or(&[]);
        if required
            .iter()
            .all(|command| observed.iter().any(|name| name == command))
        {
            unlocked.push(gated.name);
            added.push(gated.name);
        }
    }
    added
}

/// `(name, label, sequential)` for every tool that is currently offered; used
/// by the TUI's tool chip and by introspection requests.
pub fn tool_catalog(
    accessory: bool,
    notes: bool,
    unlocked: &[&str],
) -> Vec<(&'static str, &'static str, bool)> {
    available_tools(accessory, notes, unlocked)
        .into_iter()
        .map(|tool| (tool.name, tool.label, tool.sequential))
        .collect()
}

/// The 16000-character backstop applied to every tool result
/// (`afterToolCall` in `pi-agent.mjs`).
pub const TOOL_RESULT_LIMIT: usize = 16_000;

pub fn truncate_tool_result(text: &str) -> String {
    if text.chars().count() <= TOOL_RESULT_LIMIT {
        return text.to_string();
    }
    let total = text.chars().count();
    let head: String = text.chars().take(TOOL_RESULT_LIMIT).collect();
    format!(
        "{}\n[truncated {} characters; request a narrower range or a more specific query]",
        head,
        total - TOOL_RESULT_LIMIT
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const PI_AGENT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../mobile/src/pi-agent.mjs");

    fn source() -> String {
        std::fs::read_to_string(PI_AGENT).expect("read mobile/src/pi-agent.mjs")
    }

    fn every_tool() -> Vec<ToolDef> {
        let mut tools = base_tools();
        tools.extend(verify_tools());
        tools.push(watch_tool());
        tools.extend(gated_tools());
        tools.extend(accessory_tools());
        tools.push(note_tool());
        tools
    }

    /// Names and labels are the contract the panel's tool chip renders; they
    /// must appear byte-for-byte in the authoritative source.
    #[test]
    fn names_and_labels_match_pi_agent() {
        let source = source();
        for tool in every_tool() {
            let pair = format!("name: \"{}\", label: \"{}\"", tool.name, tool.label);
            assert!(source.contains(&pair), "missing in pi-agent.mjs: {pair}");
        }
    }

    /// Descriptions are quoted from the source too, except the gated file read
    /// whose description interpolates `MAX_READ_BYTES`.
    #[test]
    fn descriptions_match_pi_agent() {
        let source = source();
        for tool in every_tool() {
            if tool.name == "read_target_file" {
                assert!(source.contains(
                    "Read up to ${MAX_READ_BYTES} bytes of a file on the target as text"
                ));
                assert!(tool
                    .description
                    .starts_with("Read up to 1024 bytes of a file on the target as text"));
                continue;
            }
            let quoted_double = format!("description: \"{}\"", tool.description);
            let quoted_backtick = format!("description: `{}`", tool.description);
            assert!(
                source.contains(&quoted_double) || source.contains(&quoted_backtick),
                "description not found verbatim for {}",
                tool.name
            );
        }
    }

    #[test]
    fn default_catalogue_has_the_confirmed_shape() {
        let tools = available_tools(false, false, &[]);
        assert_eq!(tools.len(), 18, "15 base + 2 verify + watch");
        assert!(tools.iter().any(|t| t.name == "watch_serial_output"));
        assert!(!tools.iter().any(|t| t.name == "read_target_file"), "gated");

        let full = available_tools(true, true, &["read_target_file"]);
        assert_eq!(full.len(), 25, "18 + 5 accessory + notes + gated read");

        let catalog = tool_catalog(false, false, &[]);
        assert_eq!(catalog.len(), 18);
        assert_eq!(catalog[0], ("update_task_plan", "Update task plan", false));
    }

    #[test]
    fn sequential_flags_match_pi_agent() {
        let source = source();
        let sequential = [
            "probe_tools",
            "probe_device_profile",
            "probe_download_tools",
            "download_to_target",
            "download_to_computer",
            "run_shell_command",
            "monitor_serial_execution",
            "read_serial_log",
            "send_serial_input",
            "inspect_serial_execution",
            "wait_for_serial_output",
            "read_target_file",
            "verify_target_file",
            "verify_target_service",
            "watch_serial_output",
            "get_accessory_diagnostics",
            "set_uart_config",
            "wifi_scan",
            "set_wifi",
            "set_webdav",
            "remember_target_note",
        ];
        let tools = every_tool();
        for name in sequential {
            let tool = tools.iter().find(|t| t.name == name).expect(name);
            assert!(tool.sequential, "{name} must be sequential");
            // The source declares it on the following lines of the tool entry.
            let block = tool_block(&source, name);
            assert!(
                block.contains("executionMode: \"sequential\""),
                "{name} not sequential in pi-agent.mjs"
            );
        }
        for name in [
            "update_task_plan",
            "get_device_status",
            "read_web_page",
            "search_serial_log",
        ] {
            let tool = tools.iter().find(|t| t.name == name).expect(name);
            assert!(!tool.sequential, "{name} may run in parallel");
        }
    }

    fn tool_block(source: &str, name: &str) -> String {
        let marker = format!("name: \"{}\"", name);
        let start = source.find(&marker).unwrap_or(0);
        let end = source[start..]
            .find("\n    },")
            .map(|offset| start + offset)
            .unwrap_or(source.len());
        source[start..end].to_string()
    }

    #[test]
    fn schemas_are_objects_with_additional_properties_off() {
        for tool in every_tool() {
            assert_eq!(tool.schema["type"], "object", "{}", tool.name);
            assert_eq!(tool.schema["additionalProperties"], false, "{}", tool.name);
            assert!(tool.schema["properties"].is_object(), "{}", tool.name);
        }
        // A couple of exact shapes from pi-agent's Type.Object definitions.
        let tools = every_tool();
        let probe = tools.iter().find(|t| t.name == "probe_tools").unwrap();
        assert_eq!(probe.schema["properties"]["names"]["maxItems"], 16);
        assert_eq!(
            probe.schema["properties"]["names"]["items"]["pattern"],
            "^[A-Za-z0-9][A-Za-z0-9_.+-]{0,63}$"
        );
        let send = tools
            .iter()
            .find(|t| t.name == "send_serial_input")
            .unwrap();
        assert_eq!(send.schema["required"], json!(["text", "appendEnter"]));
        let watch = tools
            .iter()
            .find(|t| t.name == "watch_serial_output")
            .unwrap();
        assert_eq!(watch.schema["properties"]["timeoutMs"]["maximum"], 120000);
    }

    #[test]
    fn gating_unlocks_only_on_observed_commands() {
        let mut unlocked: Vec<&'static str> = Vec::new();
        assert!(unlocked.is_empty());
        let added = unlock_tools(&mut unlocked, &["dd".to_string(), "base64".to_string()]);
        assert_eq!(added, vec!["read_target_file"]);
        assert_eq!(
            unlock_tools(&mut unlocked, &["dd".to_string(), "base64".to_string()]),
            Vec::<&str>::new(),
            "already unlocked"
        );

        let mut fresh: Vec<&'static str> = Vec::new();
        assert!(
            unlock_tools(&mut fresh, &["dd".to_string()]).is_empty(),
            "one missing command keeps the gate shut"
        );
    }

    #[test]
    fn tool_results_truncate_with_the_exact_marker() {
        let short = "x".repeat(100);
        assert_eq!(truncate_tool_result(&short), short);
        let long = "y".repeat(17_000);
        let out = truncate_tool_result(&long);
        assert!(out.starts_with(&"y".repeat(16_000)));
        assert!(out.ends_with(
            "\n[truncated 1000 characters; request a narrower range or a more specific query]"
        ));
        assert_eq!(TOOL_RESULT_LIMIT, 16_000);
    }

    /// The fixed-cost guard: prompt plus every tool description stays under
    /// `AGENT_FIXED_CONTEXT_TOKENS * 3` characters.
    #[test]
    fn fixed_part_stays_inside_the_token_budget() {
        let prompt =
            crate::agent::prompt::serial_system_prompt(crate::agent::ExecMode::Auto, true, true);
        let tools = available_tools(true, true, &["read_target_file"]);
        let fixed: usize = prompt.chars().count()
            + tools
                .iter()
                .map(|tool| tool.name.len() + tool.description.chars().count())
                .sum::<usize>();
        assert!(
            fixed <= 24_000,
            "fixed part is {fixed} chars, budget is 24000"
        );
    }
}
