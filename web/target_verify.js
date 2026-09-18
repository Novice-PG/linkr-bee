/* Machine-decided verification of target state: file content and service state.
 *
 * Every other tool in this project hands the model evidence and leaves the
 * conclusion to it: a marker line, a serial excerpt, an exit code. That is the
 * right shape for diagnosis and the wrong shape for verification. An exit code
 * of zero means the shell finished, not that the user's goal is met, and a model
 * that reads `mv -f "$t" "$d" && printf ok` in a log has no way to tell a landed
 * file from a command that never ran. Asking the model to decide "verified" from
 * prose makes the verdict as reliable as its reading of the log.
 *
 * So this module is the other half. For a narrow, checkable set of claims the
 * APPLICATION compares what the target reported against what the caller expected
 * and returns match / mismatch / indeterminate. The verdict cannot be produced
 * by prose, cannot be produced by the echoed command, and does not depend on the
 * model reading output correctly.
 *
 * `status` means exactly this, in both parsers:
 *
 *   match          every requested check agreed with the expectation
 *   mismatch       at least one requested check disagreed, or the thing being
 *                  checked is provably not what was expected
 *   indeterminate  the target could not answer, or the answer is unusable, so
 *                  nothing is claimed either way
 *   observed       no expectation was supplied: this is a measurement, not a
 *                  verification, and it must never be reported as one
 *
 * It is pure: no DOM, no serial port, no storage. It builds one shell command
 * and parses its output.
 *
 * MARKERS. Same discipline as target_files.js: one marker per whole line,
 * matched only as a complete line, printed with `printf` (busybox `echo` has no
 * portable escape handling). The command is a single line joined with `; ` for
 * the same reason: a multi-line script would let the shell's echo of the command
 * start a line with a marker that no output produced.
 *
 *   LINKR_VERIFY:path=<path>              which path this output belongs to
 *   LINKR_VERIFY:file                     readable regular file; size follows
 *   LINKR_VERIFY:bytes=<n>                `wc -c`, whitespace tolerated
 *   LINKR_VERIFY:sha256=<hash>            digest, only when one was asked for
 *   LINKR_VERIFY:sha256=unavailable       no sha256sum and no shasum on target
 *   LINKR_VERIFY:missing                  path does not exist
 *   LINKR_VERIFY:directory                path exists and is a directory
 *   LINKR_VERIFY:not-regular              fifo/device/socket: no content
 *   LINKR_VERIFY:denied                   exists but is not readable
 *   LINKR_VERIFY:not-absolute             relative path: refused, nothing read
 *   LINKR_VERIFY:unreadable               regular and readable, but `wc` failed
 *   LINKR_VERIFY:done                     the command ran to the end
 *
 *   LINKR_VERIFY:subject=<kind>:<value>    which claim this output belongs to
 *   LINKR_VERIFY:unit-begin / -end           block of `Key=Value` from systemctl
 *   LINKR_VERIFY:process-begin / -end        block of pids from pgrep
 *   LINKR_VERIFY:listeners-begin / -end      block of the listening-socket table
 *   LINKR_VERIFY:listener-tool=ss|netstat    which tool produced that table
 *   LINKR_VERIFY:unsupported=<reason>        the target has no way to answer
 *   LINKR_VERIFY:done                      the command ran to the end
 *
 * `done` is printed on every path, early refusals included. So a missing `done`
 * is unambiguous: the command did not finish (output lost, target busy, command
 * interrupted), which is `indeterminate` -- never a finding. Refusals exit 0 on
 * purpose, because "the file is not there" is an answer to read from the
 * marker, not a transport failure.
 *
 * PATH ATTRIBUTION. Each command echoes its own subject on the first line and
 * the parsers check it against what the caller asked for. Console evidence is a
 * window into a shared journal, and an earlier verify of a different path can
 * sit inside it; comparing the echo keeps a stale finding from being attributed
 * to the current request.
 */

import { quoteShell } from "./target_files.js";

/* One verify command is a single line of POSIX shell with a handful of
 * conditionals, so it stays far below an upload chunk. The ceiling exists for
 * the same reason as in target_files.js: the console must not be filled by the
 * command itself, or the shell's echo can be cut mid-line and swallow a marker. */
export const MAX_VERIFY_COMMAND_BYTES = 4096;
export const MAX_VERIFY_PATH = 200;
export const MAX_VERIFY_PATTERN = 200;
/* Bounds on the blocks a target writes back, so one verify cannot bury the
 * console history the operator is watching. The listening table is kept to a
 * size that still fits inside the bridge's 8192-byte RX ring together with the
 * echo of the command and the next prompt -- an unescaped table is what makes a
 * response get cut in half -- at roughly 65 characters per row. The pid list is
 * not ring-bound, only console-bound. */
export const MAX_LISTENER_ROWS = 60;
export const MAX_PROCESS_PIDS = 50;

const PATH_MARKER = /^LINKR_VERIFY:path=(.*)$/;
const BYTES_MARKER = /^LINKR_VERIFY:bytes=\s*(\d+)$/;
const SHA_MARKER = /^LINKR_VERIFY:sha256=([0-9a-f]{64}|unavailable)$/;
const SUBJECT_MARKER = /^LINKR_VERIFY:subject=(unit|process|port):(.*)$/;
const UNSUPPORTED_MARKER = /^LINKR_VERIFY:unsupported=([a-z-]+)$/;
const SETTING_LINE = /^([A-Za-z]+)=(\S*)$/;
const LISTEN_ADDRESS = /^\S*:(\d+)$/;

/* Findings that describe the path rather than its content. Each is a definite
 * observation, which is why the verdict still depends on what was asked for:
 * "the file is missing" answers "does it exist" and refutes "it holds this
 * digest". `denied` and `unreadable` are the two that claim nothing about
 * content, because the content was never seen. */
const FILE_FINDINGS = {
  missing: { label: "missing", content: true, text: "The target path does not exist." },
  directory: { label: "directory", content: true, text: "The target path is a directory, not a file." },
  "not-regular": { label: "other", content: true, text: "The target path is not a regular file, so it has no content to verify." },
  "not-absolute": { label: "unknown", content: false, text: "The target path is not absolute, so the check never ran against a real file." },
  denied: { label: "unknown", content: false, text: "The target file exists but is not readable, so its content could not be checked." },
  unreadable: { label: "unknown", content: false, text: "The target file could not be measured: the size command failed." },
};

const UNSUPPORTED_TEXT = {
  "no-systemctl": "The target has no systemctl, so it cannot report service state.",
  "no-pgrep": "The target has no pgrep, so it cannot report matching processes.",
  "no-listener-tool": "The target has neither ss nor netstat, so it cannot report listening ports.",
};

const UNIT_EXPECTATIONS = ["active", "inactive", "failed"];
const PROCESS_EXPECTATIONS = ["running", "absent"];
const PORT_EXPECTATIONS = ["listening", "closed"];
export const SERVICE_EXPECTATIONS = {
  unit: UNIT_EXPECTATIONS, process: PROCESS_EXPECTATIONS, port: PORT_EXPECTATIONS,
};

// Serial consoles send CRLF, and a bare CR is its own line break on progress output.
const markerLines = (text) => String(text ?? "").replace(/\r\n?/g, "\n").split("\n").map((line) => line.trimEnd());

function verifyPath(value) {
  if (typeof value !== "string" || value === "") throw new Error("Target path must be a non-empty string.");
  if (value.length > MAX_VERIFY_PATH) throw new Error(`Target path must not exceed ${MAX_VERIFY_PATH} characters.`);
  /* Control characters would break the line-oriented markers, and a forged
   * marker line could then answer for the target. A relative path is allowed
   * through: the shell reports it as not-absolute, which is a diagnosis the
   * caller can act on instead of an exception. */
  if (/[\x00-\x1f\x7f]/.test(value)) throw new Error("Target path must not contain control characters.");
  return value;
}

/* A digest is only accepted as an expectation, never as a measurement: the
 * whole point is that the target's content is compared against a value the
 * caller already trusts. A malformed expectation throws rather than quietly
 * disabling the check. */
export function normalizeExpectedSha256(value) {
  if (value === undefined || value === null || value === "") return "";
  if (typeof value !== "string" || !/^[0-9a-fA-F]{64}$/.test(value)) throw new Error("An expected sha256 must be 64 hexadecimal characters.");
  return value.toLowerCase();
}

/* A size expectation. Zero is meaningful: a truncated download of an empty file
 * is a real result, so it is accepted like any other count. */
export function normalizeExpectedBytes(value) {
  if (value === undefined || value === null || value === "") return null;
  if (!Number.isSafeInteger(value) || value < 0) throw new Error("An expected byte count must be a non-negative integer.");
  return value;
}

function verifyPattern(value) {
  if (typeof value !== "string" || value === "") throw new Error("A process pattern must be a non-empty string.");
  if (value.length > MAX_VERIFY_PATTERN) throw new Error(`A process pattern must not exceed ${MAX_VERIFY_PATTERN} characters.`);
  if (/[\x00-\x1f\x7f]/.test(value)) throw new Error("A process pattern must not contain control characters.");
  return value;
}

/* Unit names are handed to systemctl, and a name beginning with `-` would be
 * read as an option even when quoted. The character set is systemd's own. */
function verifyUnitName(value) {
  if (typeof value !== "string" || !/^[A-Za-z0-9][A-Za-z0-9@._:-]{0,127}$/.test(value)) {
    throw new Error("A unit name must start with a letter or digit and contain only letters, digits, @ . _ : -");
  }
  return value;
}

function verifyPort(value) {
  if (!Number.isSafeInteger(value) || value < 1 || value > 65535) throw new Error("A port must be an integer between 1 and 65535.");
  return value;
}

function guardCommand(command) {
  if (command.length > MAX_VERIFY_COMMAND_BYTES) {
    throw new Error(`Verify command would be ${command.length} characters, over the ${MAX_VERIFY_COMMAND_BYTES}-character UART budget; shorten the path or pattern.`);
  }
  return command;
}

/* Verify one target file.
 *
 * `hash` decides whether the digest is computed, because hashing reads the whole
 * file on the target: a size-only check on a 4 GiB image must not pay for a
 * digest nobody asked for. The caller sets it when a digest is expected, or when
 * the user explicitly wants the target's current digest.
 *
 * Only `wc` is required, and it is POSIX. `sha256sum`/`shasum` are needed only
 * for a digest, and their absence is reported as a marker so the caller gets an
 * indeterminate verdict with a reason instead of a command-not-found failure.
 */
export function verifyFileCommand({ path, hash = false } = {}) {
  const p = quoteShell(verifyPath(path));
  const parts = [
    `p=${p}`,
    // Printed before any finding so a verdict is always attributable to a path.
    `printf '\\nLINKR_VERIFY:path=%s\\n' "$p"`,
    `case "$p" in /*) ;; *) printf 'LINKR_VERIFY:not-absolute\\nLINKR_VERIFY:done\\n'; exit 0 ;; esac`,
    `[ -e "$p" ] || { printf 'LINKR_VERIFY:missing\\nLINKR_VERIFY:done\\n'; exit 0; }`,
    `if [ -d "$p" ]; then printf 'LINKR_VERIFY:directory\\nLINKR_VERIFY:done\\n'; exit 0; fi`,
    `[ -f "$p" ] || { printf 'LINKR_VERIFY:not-regular\\nLINKR_VERIFY:done\\n'; exit 0; }`,
    `[ -r "$p" ] || { printf 'LINKR_VERIFY:denied\\nLINKR_VERIFY:done\\n'; exit 0; }`,
    // The size is measured, not scraped from a tool's summary line, and the
    // whitespace some `wc` implementations pad with is tolerated by the parser.
    `n=$(wc -c < "$p" 2>/dev/null) || n=`,
    `[ -n "$n" ] || { printf 'LINKR_VERIFY:unreadable\\nLINKR_VERIFY:done\\n'; exit 0; }`,
    `printf 'LINKR_VERIFY:file\\nLINKR_VERIFY:bytes=%s\\n' "$n"`,
  ];
  if (hash === true) parts.push(
    `if command -v sha256sum >/dev/null 2>&1; then h=$(sha256sum "$p" 2>/dev/null); elif command -v shasum >/dev/null 2>&1; then h=$(shasum -a 256 "$p" 2>/dev/null); else h=; fi`,
    // Both printers write "<hash>  <file>"; keep only the digest column.
    `h=\${h%% *}`,
    `[ -n "$h" ] || h=unavailable`,
    `printf 'LINKR_VERIFY:sha256=%s\\n' "$h"`,
  );
  parts.push(`printf 'LINKR_VERIFY:done\\n'`);
  return guardCommand(parts.join('; '));
}

/* Verify one service claim: a systemd unit, a process matching a pattern, or a
 * listening TCP port.
 *
 * `systemctl show` output is passed through as `Key=Value` lines instead of
 * being rewritten into markers on the target, which keeps the command free of
 * awk/sed. The app does the interpreting, which is the point of this module.
 *
 * A listening-socket table is dumped whole and filtered by the parser: ss and
 * netstat disagree about filter syntax, and a target-side `grep` would hide the
 * difference between "nothing is listening" and "the filter never ran".
 */
export function verifyServiceCommand({ unit = "", process = "", port = null, expect = "" } = {}) {
  const requested = [unit !== "" && unit !== undefined && unit !== null,
    process !== "" && process !== undefined && process !== null,
    port !== null && port !== undefined && port !== ""];
  if (requested.filter(Boolean).length !== 1) throw new Error("Verify exactly one of unit, process or port.");

  if (requested[0]) {
    const name = verifyUnitName(unit);
    normalizeExpectation("unit", expect);
    return guardCommand([
      `s=${quoteShell(name)}`,
      `printf '\\nLINKR_VERIFY:subject=unit:%s\\n' "$s"`,
      `command -v systemctl >/dev/null 2>&1 || { printf 'LINKR_VERIFY:unsupported=no-systemctl\\nLINKR_VERIFY:done\\n'; exit 0; }`,
      // A non-zero exit still carries the properties systemd managed to print,
      // so the status is deliberately not allowed to discard that output.
      `o=$(systemctl show -p LoadState -p ActiveState -p SubState -p MainPID -p ExecMainStatus -- "$s" 2>/dev/null) || :`,
      `printf 'LINKR_VERIFY:unit-begin\\n%s\\nLINKR_VERIFY:unit-end\\n' "$o"`,
      `printf 'LINKR_VERIFY:done\\n'`,
    ].join('; '));
  }

  if (requested[1]) {
    const pattern = verifyPattern(process);
    normalizeExpectation("process", expect);
    return guardCommand([
      `s=${quoteShell(pattern)}`,
      `printf '\\nLINKR_VERIFY:subject=process:%s\\n' "$s"`,
      `command -v pgrep >/dev/null 2>&1 || { printf 'LINKR_VERIFY:unsupported=no-pgrep\\nLINKR_VERIFY:done\\n'; exit 0; }`,
      // pgrep excludes itself, so the probe cannot match its own invocation.
      `printf 'LINKR_VERIFY:process-begin\\n'`,
      `pgrep -f "$s" 2>/dev/null | head -n ${MAX_PROCESS_PIDS}`,
      `printf '\\nLINKR_VERIFY:process-end\\n'`,
      `printf 'LINKR_VERIFY:done\\n'`,
    ].join('; '));
  }

  const value = verifyPort(port);
  normalizeExpectation("port", expect);
  return guardCommand([
    `printf '\\nLINKR_VERIFY:subject=port:%s\\n' '${value}'`,
    `t=`,
    `if command -v ss >/dev/null 2>&1; then t=ss; elif command -v netstat >/dev/null 2>&1; then t=netstat; fi`,
    `[ -n "$t" ] || { printf 'LINKR_VERIFY:unsupported=no-listener-tool\\nLINKR_VERIFY:done\\n'; exit 0; }`,
    `printf 'LINKR_VERIFY:listener-tool=%s\\n' "$t"`,
    `printf 'LINKR_VERIFY:listeners-begin\\n'`,
    `$t -ltn 2>/dev/null | head -n ${MAX_LISTENER_ROWS}`,
    `printf '\\nLINKR_VERIFY:listeners-end\\n'`,
    `printf 'LINKR_VERIFY:done\\n'`,
  ].join('; '));
}

/* The expectation is validated here rather than in the tool layer so that an
 * unsupported value can never silently become "no check". `port` arrives as a
 * number, so it is stringified before the set lookup. */
export function normalizeExpectation(kind, value) {
  const allowed = SERVICE_EXPECTATIONS[kind];
  if (!allowed) throw new Error(`Unknown service kind: ${kind}`);
  if (value === undefined || value === null || value === "") return allowed[0];
  const text = String(value);
  if (!allowed.includes(text)) throw new Error(`Expected ${kind} state must be one of ${allowed.join(", ")}.`);
  return text;
}

function findMarker(lines, pattern) {
  for (const line of lines) {
    const match = pattern.exec(line);
    if (match) return match;
  }
  return null;
}

/* Collect the lines between a pair of block markers. An unterminated block is
 * returned with `closed: false`, which is how a table cut off mid-stream is
 * distinguished from an empty one. */
function blockLines(lines, begin, end) {
  const start = lines.indexOf(begin);
  if (start < 0) return { rows: [], closed: false, seen: false };
  const stop = lines.indexOf(end, start + 1);
  const rows = lines.slice(start + 1, stop < 0 ? lines.length : stop).filter((line) => line !== "");
  return { rows, closed: stop >= 0, seen: true };
}

/* Read back verifyFileCommand output.
 *
 * Pass `{ path, bytes, sha256 }` to get a verdict; `path` alone is enough to
 * make the observation attributable, and `bytes`/`sha256` are what turn a
 * measurement into a verification. Everything else is reported either way.
 */
export function parseVerifyResult(text, expected = {}) {
  const lines = markerLines(text);
  const wantPath = expected.path === undefined || expected.path === null ? null : String(expected.path);
  const wantBytes = normalizeExpectedBytes(expected.bytes);
  const wantSha = normalizeExpectedSha256(expected.sha256);
  const asked = wantBytes !== null || Boolean(wantSha);

  const result = {
    status: "indeterminate",
    found: null,
    complete: false,
    path: wantPath,
    bytes: null,
    sha256: "",
    sha256Unavailable: false,
    expectedBytes: wantBytes,
    expectedSha256: wantSha,
    checks: { bytes: "not-requested", sha256: "not-requested" },
    evidence: [],
    reason: "",
  };
  const settle = (status, reason, extra = {}) => {
    const { extraEvidence = [], ...rest } = extra;
    /* Keep the lines that carried the finding, bounded, so the model can cite
     * them without re-reading the console. */
    return { ...result, ...rest, status, reason,
      evidence: [...lines.filter((line) => line.startsWith("LINKR_VERIFY:")), ...extraEvidence].slice(0, 12) };
  };

  const done = lines.includes("LINKR_VERIFY:done");
  result.complete = done;

  /* The echo has to match before any finding is trusted: a window of the shared
   * journal can still hold an earlier verify of a different path. */
  const pathMarker = findMarker(lines, PATH_MARKER);
  if (!pathMarker) {
    return settle("indeterminate", done
      ? "The output does not name the path that was checked, so nothing can be attributed to this request."
      : "No output named the path and no completion marker arrived: the check either never ran or its output was lost. Monitor the same execution again instead of sending a second verify.");
  }
  if (wantPath !== null && pathMarker[1] !== wantPath) {
    return settle("indeterminate", `This output belongs to ${pathMarker[1]}, not ${wantPath}.`);
  }
  if (!done) return settle("indeterminate", "The check did not finish: the command printed no completion marker. Monitor the same execution again instead of sending a second verify.");

  const finding = Object.keys(FILE_FINDINGS).find((name) => lines.includes(`LINKR_VERIFY:${name}`));
  if (finding) {
    const entry = FILE_FINDINGS[finding];
    result.found = entry.label;
    if (!asked) return settle("observed", `${entry.text} No expectation was supplied, so this is a measurement, not a verification.`);
    // "The path is not what I expected" still refutes specific content, but a
    // refusal to read claims nothing: the bytes were never seen.
    if (entry.content) return settle("mismatch", `${entry.text} The expected content therefore cannot be present.`);
    return settle("indeterminate", entry.text);
  }

  if (!lines.includes("LINKR_VERIFY:file")) {
    return settle("indeterminate", "The output reports no file state, so the target's answer is unusable.");
  }
  const bytes = findMarker(lines, BYTES_MARKER);
  if (!bytes) return settle("indeterminate", "The target reported a regular file but no size, so its content cannot be compared.");
  result.found = "file";
  result.bytes = Number(bytes[1]);

  const sha = findMarker(lines, SHA_MARKER);
  if (sha) {
    if (sha[1] === "unavailable") result.sha256Unavailable = true;
    else result.sha256 = sha[1];
  }

  if (wantBytes !== null) result.checks.bytes = result.bytes === wantBytes ? "match" : "mismatch";
  if (wantSha) {
    if (result.sha256Unavailable) result.checks.sha256 = "unknown";
    else if (!result.sha256) result.checks.sha256 = "unknown";
    else result.checks.sha256 = result.sha256 === wantSha ? "match" : "mismatch";
  }

  if (Object.values(result.checks).includes("mismatch")) {
    const detail = [];
    if (result.checks.bytes === "mismatch") detail.push(`the target holds ${result.bytes} bytes, not ${wantBytes}`);
    if (result.checks.sha256 === "mismatch") detail.push(`the target's sha256 is ${result.sha256}, not ${wantSha}`);
    return settle("mismatch", `The target file is not the expected content: ${detail.join("; ")}.`);
  }
  if (result.checks.sha256 === "unknown") {
    return settle("indeterminate", result.sha256Unavailable
      ? "The target has neither sha256sum nor shasum, so the expected digest could not be checked."
      : "The output carries no digest, so the expected digest could not be checked.");
  }
  if (!asked) {
    /* A requested digest that the target could not produce is part of the
     * measurement, and saying "measured" without saying that would leave the
     * caller to infer a digest that never existed. */
    return settle("observed", result.sha256Unavailable
      ? "The target file was measured, but the target has neither sha256sum nor shasum, so no digest was produced. No expectation was supplied, so this is a measurement, not a verification."
      : "The target file was measured. No expectation was supplied, so this is a measurement, not a verification.");
  }
  return settle("match", "The target file matches every expectation that was supplied.");
}

/* Read back verifyServiceCommand output.
 *
 * Pass `{ unit }` / `{ process }` / `{ port }` and, when the caller needs
 * something other than the common case, `expect`. The subject is echoed by the
 * command and checked here for the same attribution reason as files.
 */
export function parseServiceResult(text, expected = {}) {
  const lines = markerLines(text);
  const kind = expected.unit !== undefined && expected.unit !== null && expected.unit !== ""
    ? "unit"
    : expected.process !== undefined && expected.process !== null && expected.process !== ""
      ? "process"
      : expected.port !== undefined && expected.port !== null && expected.port !== ""
        ? "port" : null;
  if (!kind) throw new Error("Name exactly one of unit, process or port in the expectation.");

  const subject = kind === "port" ? String(verifyPort(Number(expected.port))) : String(expected[kind]);
  const expectation = normalizeExpectation(kind, expected.expect);
  const result = {
    status: "indeterminate", kind, subject, expected: expectation, complete: false,
    unsupported: "", observed: {}, evidence: [], reason: "",
  };
  const settle = (status, reason, extra = {}) => {
    const { extraEvidence = [], ...rest } = extra;
    /* Block contents (ActiveState=…, a pid, a socket row) are not markers, but
     * they are the evidence the verdict rests on, so they travel with it. */
    return { ...result, ...rest, status, reason,
      evidence: [...lines.filter((line) => line.startsWith("LINKR_VERIFY:")), ...extraEvidence].slice(0, 12) };
  };

  result.complete = lines.includes("LINKR_VERIFY:done");
  const subjectMarker = findMarker(lines, SUBJECT_MARKER);
  if (!subjectMarker) {
    return settle("indeterminate", result.complete
      ? "The output does not name the claim that was checked, so nothing can be attributed to this request."
      : "No output named the claim and no completion marker arrived: the check either never ran or its output was lost. Monitor the same execution again instead of sending a second verify.");
  }
  if (subjectMarker[1] !== kind || subjectMarker[2] !== subject) {
    return settle("indeterminate", `This output belongs to ${subjectMarker[1]} ${subjectMarker[2]}, not ${kind} ${subject}.`);
  }
  const unsupported = findMarker(lines, UNSUPPORTED_MARKER);
  if (unsupported) {
    return settle("indeterminate", UNSUPPORTED_TEXT[unsupported[1]] || `The target cannot report this: ${unsupported[1]}.`,
      { unsupported: unsupported[1] });
  }
  if (!result.complete) {
    return settle("indeterminate", "The check did not finish: the command printed no completion marker. Monitor the same execution again instead of sending a second verify.");
  }

  if (kind === "unit") {
    const block = blockLines(lines, "LINKR_VERIFY:unit-begin", "LINKR_VERIFY:unit-end");
    if (!block.seen || !block.closed) return settle("indeterminate", "The target's unit state arrived incomplete, so its state cannot be read.", { extraEvidence: block.rows });
    const settings = {};
    for (const row of block.rows) {
      const match = SETTING_LINE.exec(row);
      if (match) settings[match[1]] = match[2];
    }
    if (!settings.ActiveState) {
      return settle("indeterminate", "The target reported no unit state, so there is nothing to compare.", { extraEvidence: block.rows });
    }
    /* A unit that does not exist reports no ActiveState of its own; treating
     * "not-found" as "inactive" would turn a typo into a confident all-clear. */
    if (settings.LoadState === "not-found") {
      return settle("indeterminate", `${subject} is not a unit on this target, which is not the same as a stopped service.`,
        { observed: { load: settings.LoadState }, extraEvidence: block.rows });
    }
    result.observed = { load: settings.LoadState || "", active: settings.ActiveState,
      sub: settings.SubState || "", mainPid: Number(settings.MainPID || 0), exitStatus: Number(settings.ExecMainStatus || 0) };
    const agrees = settings.ActiveState === expectation;
    const detail = `ActiveState=${settings.ActiveState}${settings.SubState ? `, SubState=${settings.SubState}` : ""}${result.observed.exitStatus ? `, exit status ${result.observed.exitStatus}` : ""}`;
    return settle(agrees ? "match" : "mismatch",
      agrees ? `${subject} is ${expectation} (${detail}).` : `${subject} is not ${expectation}: ${detail}.`,
      { extraEvidence: block.rows });
  }

  if (kind === "process") {
    const block = blockLines(lines, "LINKR_VERIFY:process-begin", "LINKR_VERIFY:process-end");
    if (!block.seen || !block.closed) return settle("indeterminate", "The target's process list arrived incomplete, so it cannot be read.", { extraEvidence: block.rows });
    const pids = block.rows.filter((row) => /^\d+$/.test(row));
    result.observed = { pids, count: pids.length, truncated: pids.length >= MAX_PROCESS_PIDS };
    const running = pids.length > 0;
    const agrees = running === (expectation === "running");
    const count = `matched ${pids.length}${result.observed.truncated ? " or more" : ""} process${pids.length === 1 ? "" : "es"}`;
    return settle(agrees ? "match" : "mismatch",
      agrees ? `The target ${expectation === "running" ? "has" : "has no"} matching process (${count}).`
        : `The target ${expectation === "running" ? "has no" : "still has"} matching process, but ${expectation} was expected (${count}).`,
      { extraEvidence: pids.slice(0, 10) });
  }

  const tool = findMarker(lines, /^LINKR_VERIFY:listener-tool=(ss|netstat)$/);
  const block = blockLines(lines, "LINKR_VERIFY:listeners-begin", "LINKR_VERIFY:listeners-end");
  if (!block.seen || !block.closed) return settle("indeterminate", "The target's listening-socket table arrived incomplete, so it cannot be read.", { extraEvidence: block.rows });
  if (!tool) return settle("indeterminate", "The output does not say which tool produced the socket table, so its rows cannot be interpreted.", { extraEvidence: block.rows });
  /* Only rows whose local address ends in this port count. A listening socket's
   * peer column is always unspecified, so a port number in a row belongs to the
   * local side. */
  const rows = block.rows.filter((row) => row.split(/\s+/).some((field) => {
    const match = LISTEN_ADDRESS.exec(field);
    return match && Number(match[1]) === Number(subject);
  }));
  const listening = rows.length > 0;
  const truncated = block.rows.length >= MAX_LISTENER_ROWS;
  result.observed = { tool: tool[1], listening, rows: rows.slice(0, 8), truncated };
  /* A truncated table proves nothing by absence. The rows are cut at a bound,
   * so "no row mentions the port" may only mean the row was never printed --
   * reporting that as "nothing listens" would be a confident wrong answer, which
   * is the one thing this module exists to prevent. A truncated table where the
   * port WAS found still proves presence, so only the negative case is refused.
   * The process list is the mirror image and needs no such guard: its bound is
   * only reached when entries were found, which is what "running" claims. */
  if (!listening && truncated) {
    return settle("indeterminate", `${tool[1]} printed the first ${MAX_LISTENER_ROWS} listening sockets and stopped, so this table cannot show that nothing listens on port ${subject}. Report the uncertainty instead of reporting the port as closed.`, { extraEvidence: block.rows.slice(-8) });
  }
  const agrees = listening === (expectation === "listening");
  return settle(agrees ? "match" : "mismatch",
    listening
      ? `${tool[1]} reports something listening on port ${subject}: ${rows[0].trim()}`
      : `${tool[1]} reports nothing listening on port ${subject}.`,
    { extraEvidence: rows.slice(0, 8) });
}
