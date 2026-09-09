import { requiresInputApproval } from "./agent_execution_policy.js";
import { parseDeviceProfile } from "./device_profile.js";
import { inspectSerialConsole } from "./serial_console.js";

const cancelled = () => new Error("Operation cancelled. Do not retry automatically.");

// Owns device permissions and evidence. No DOM, model SDK, or automatic retries.
export function createDeviceExecutor({ getStatus, readLog, prepareInput, sendInput, onRecord }) {
  let mode = "auto";
  let nextId = 0;
  let active = null;
  const records = [];
  let profile = null;
  const snapshot = (record) => structuredClone(record);
  const publish = (record) => { if (records.includes(record)) onRecord?.(snapshot(record)); };
  function consoleState() {
    const log = readLog({ limit: 4000 });
    const state = inspectSerialConsole(log);
    const last = records.findLast((record) => record.delivery !== "not-sent");
    if (last && last.sessionId === getStatus().sessionId && log.latestCursor <= last.logStart) {
      return { kind: "unknown", evidence: "", cursor: log.latestCursor, source: "awaiting-new-output" };
    }
    return state;
  }
  function check(record, signal) {
    signal.throwIfAborted();
    const status = getStatus();
    if (!status.connected || status.sessionId !== record.sessionId) throw new Error("Device session changed; input was not sent.");
    if (status.inputRevision !== record.inputRevision) throw new Error("Terminal input changed; request a new command before sending.");
    if (consoleState().kind !== record.console.kind) throw new Error("Console state changed; review a new command before sending.");
  }
  function cancel() {
    if (!active) return;
    active.controller.abort();
    active.rejectApproval?.(cancelled());
  }
  function executionPage(record, { after, limit = 4000 } = {}) {
    const end = record.observedEnd ?? record.logStart;
    limit = Math.max(1, Math.min(16000, Math.trunc(limit) || 4000));
    const start = Math.min(end, Math.max(record.logStart,
      Number.isFinite(after) ? Math.trunc(after) : end - limit));
    let output = start < end ? readLog({ after: start, limit: Math.min(limit, end - start) }) : null;
    const missingHistory = Boolean(output?.truncated);
    if (output && output.start < end && output.cursor > end) {
      // Ring-buffer eviction can move the start forward; re-bound the raw read
      // before exposing text so it never crosses a closed observation's end.
      output = readLog({ after: output.start, limit: end - output.start });
    }
    // A retained log may have advanced beyond this closed execution's range.
    const available = output && output.start < end;
    const cursor = available ? Math.min(end, output.cursor) : end;
    return { evidence: available ? output.text : "", evidenceStart: available ? output.start : start,
      evidenceTruncated: Boolean(missingHistory || output?.truncated || (output && !available) ||
        (after === undefined ? start > record.logStart : false) || cursor < end),
      observedCursor: cursor, latestCursor: end, hasMore: cursor < end };
  }
  function inspectExecution(id, options = {}) {
    const paged = options.after !== undefined || options.limit !== undefined;
    const record = records.find((item) => item.id === id);
    if (!record || record.sessionId !== getStatus().sessionId) throw new Error("Execution record is unavailable for this device session.");
    if (record.delivery !== "sent") return snapshot(record);
    if (record.observationClosed) return snapshot(paged ? { ...record, ...executionPage(record, options) } : record);
    const status = getStatus();
    if (!status.connected || status.sessionId !== record.sessionId || status.inputRevision !== record.sentRevision) {
      record.observation = "interrupted";
      record.observationClosed = true;
    } else {
      record.observedEnd = readLog({ limit: 1 }).latestCursor;
      Object.assign(record, executionPage(record));
      record.observation = !record.evidence ? "no-output"
        : inspectSerialConsole({ text: record.evidence, latestCursor: record.observedEnd }).kind === "shell" ? "prompt-returned" : "output-observed";
      const hint = inspectSerialConsole({text:record.evidence,latestCursor:record.observedEnd});
      record.waitingFor = ["login", "password", "sudo-password", "confirmation", "pager", "bootloader"].includes(hint.kind) ? hint.kind : null;
      if (record.download) {
        const partial = record.evidence.match(/^LINKR_PART:(.+)\r?$/m);
        if (partial) record.download.partialPath = partial[1].trim();
        const hash = record.evidence.match(/^LINKR_SHA256:([a-fA-F0-9]{64})\r?$/m);
        const bytes = record.evidence.match(/^LINKR_BYTES:\s*(\d+)\r?$/m);
        if (hash) record.download.sha256 = hash[1].toLowerCase();
        if (bytes) record.download.bytes = Number(bytes[1]);
      }
      if (record.completionToken) {
        const match = record.evidence.match(new RegExp(`(?:^|\\n)${record.completionToken}:([0-9]{1,3})\\r?\\n`));
        if (match && Number(match[1]) <= 255) {
          record.exitCode = Number(match[1]);
          record.executionStatus = "completed";
          record.observationClosed = true;
          if (record.download) record.download.status = record.exitCode === 0 && record.download.sha256 && Number.isFinite(record.download.bytes) ? "saved" : "failed-or-unverified";
          if (record.profileProbe && record.exitCode === 0) {
            const data = parseDeviceProfile(record.evidence);
            if (data) profile = {...data, sessionId:record.sessionId, cursor:record.observedEnd};
          }
          record.completedAt = new Date().toISOString();
        }
      }
      // Exit status describes shell completion, never verification of the goal.
    }
    publish(record);
    return snapshot(paged ? { ...record, ...executionPage(record, options) } : record);
  }
  function observe() {
    for (const record of records) {
      if (record.delivery !== "sent" || record.observationClosed) continue;
      if (record.sessionId === getStatus().sessionId) inspectExecution(record.id);
      else {
        record.observation = "interrupted";
        record.observationClosed = true;
        publish(record);
      }
    }
  }
  return {
    get mode() { return mode; },
    setMode(value) {
      if (!["auto", "manual", "full-auto"].includes(value)) throw new Error("Unknown execution mode.");
      if (value !== mode) { cancel(); mode = value; }
    },
    cancel,
    forgetProfile() { profile = null; },
    reset() { cancel(); records.length = 0; profile = null; },
    getStatus() {
      const status = getStatus();
      if (profile) {
        const recent = readLog({after:profile.cursor,limit:4000});
        profile.stale ||= !status.connected || status.sessionId !== profile.sessionId || Date.now()-profile.observedAt > 900000 ||
          /(?:^|\n)(?:Linux version |U-Boot |.* login:)/.test(recent.text || '') || recent.truncated;
        profile.cursor = recent.cursor ?? profile.cursor;
      }
      return { ...status, executionMode: mode, console: consoleState(), profile };
    },
    readLog(options) { return readLog(options); },
    getRecords() { return records.filter((record) => record.sessionId === getStatus().sessionId).map(snapshot); },
    inspectExecution,
    observe,
    approve(id) {
      if (active?.record.id !== id || active.record.state !== "awaiting-approval") return false;
      try { check(active.record, active.controller.signal); }
      catch (error) { active.rejectApproval(error); return false; }
      active.resolveApproval();
      return true;
    },
    reject(id) {
      if (active?.record.id !== id || active.record.state !== "awaiting-approval") return false;
      active.record.state = "denied";
      active.rejectApproval(new Error("User rejected this input. Do not request it again."));
      return true;
    },
    async execute(args, signal, { userApproved = false } = {}) {
      signal?.throwIfAborted();
      if (active) throw new Error("Another serial action is still active.");
      if (typeof args?.text !== "string" || !args.text.length || args.text.length > 2048 || typeof args.appendEnter !== "boolean") {
        throw new Error("Invalid serial input arguments.");
      }
      const status = getStatus();
      if (!status.connected) throw new Error("Device is disconnected.");
      const download = args.download;
      const profileProbe = args.profileProbe;
      let completionToken;
      if (args.trackExit) {
        if (status.inputPending || consoleState().kind !== "shell" || !args.appendEnter) {
          throw new Error("Tracked commands require an idle, observed POSIX shell prompt.");
        }
        completionToken = `LINKR_EXIT_${crypto.randomUUID().replaceAll("-", "")}`;
        const quoted = "'" + args.text.replaceAll("'", "'\\''") + "'";
        args = { text: `sh -c ${quoted}; printf '\\n%s:%s\\n' '${completionToken}' "$?"`, appendEnter: true };
        if (args.text.length > 2048) throw new Error("Tracked command is too long after shell quoting.");
      }
      const record = { profileProbe, download, completionToken, id: `serial-${++nextId}`, sessionId: status.sessionId, inputRevision: status.inputRevision,
        payload: prepareInput(args), mode, console: consoleState(), state: "proposed", delivery: "not-sent",
        executionStatus: "unknown", createdAt: new Date().toISOString(), evidence: "", observation: "no-output" };
      const controller = new AbortController();
      const operation = { record, controller };
      active = operation;
      const abort = () => { controller.abort(); operation.rejectApproval?.(cancelled()); };
      signal?.addEventListener("abort", abort, { once: true });
      records.push(record);
      if (records.length > 50) records.shift();
      try {
        if (!userApproved && (requiresInputApproval(mode, args, record.payload, status.inputPending) || (mode === "auto" && record.console.kind !== "shell"))) {
          record.state = "awaiting-approval";
          await new Promise((resolve, reject) => {
            let settled = false;
            const finish = (error) => {
              if (settled) return;
              settled = true;
              // Disable repeat approvals synchronously, before yielding to the loop.
              if (!error) record.state = "approved";
              publish(record);
              error ? reject(error) : resolve();
            };
            operation.resolveApproval = () => finish();
            operation.rejectApproval = finish;
            publish(record);
          });
        }
        check(record, controller.signal);
        // Close older observations before another command can contribute output.
        observe();
        for (const previous of records) if (previous !== record) previous.observationClosed = true;
        record.logStart = readLog({ limit: 1 }).latestCursor;
        record.observedEnd = record.logStart;
        record.state = "sending";
        record.delivery = "unknown";
        publish(record);
        const receipt = await sendInput(record.payload, record.sessionId, controller.signal, record.inputRevision);
        record.delivery = "sent";
        record.sentRevision = receipt?.inputRevision ?? getStatus().inputRevision;
        record.sentAt = new Date().toISOString();
        controller.signal.throwIfAborted();
        if (getStatus().sessionId !== record.sessionId) throw cancelled();
        record.state = "sent";
        publish(record);
        return inspectExecution(record.id);
      } catch (error) {
        if (record.state !== "denied") record.state = controller.signal.aborted ? "cancelled" : "failed";
        record.error = error.message;
        record.observationClosed = true;
        publish(record);
        throw error;
      } finally {
        signal?.removeEventListener("abort", abort);
        if (active === operation) active = null;
      }
    },
  };
}
