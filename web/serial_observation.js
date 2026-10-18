// Waiting for a quiet interval is an observation, never command completion.
function pause(ms, signal) {
  return new Promise((resolve, reject) => {
    signal?.throwIfAborted();
    const finish = () => { signal?.removeEventListener("abort", abort); resolve(); };
    const timer = setTimeout(finish, ms);
    const abort = () => { clearTimeout(timer); signal.removeEventListener("abort", abort); reject(signal.reason); };
    signal?.addEventListener("abort", abort, { once: true });
  });
}

export async function waitForSerialOutput({ readLog, after, timeoutMs = 5000, settleMs = 400,
  signal, check = () => {}, now = () => performance.now(), sleep = pause }) {
  timeoutMs = Math.max(100, Math.min(5000, timeoutMs));
  settleMs = Math.max(100, Math.min(1000, settleMs));
  const started = now();
  let changed = started;
  let cursor;
  /* Without a caller-supplied cursor the baseline is the log's end on the first
   * read: an undefined `after` would turn every comparison into NaN, so the
   * wait would always report "no output" and run to the deadline. */
  let baseline = Number.isFinite(after) ? after : null;
  while (true) {
    signal?.throwIfAborted();
    check();
    const output = readLog({ after });
    const time = now();
    if (cursor !== output.latestCursor) { cursor = output.latestCursor; changed = time; }
    if (baseline === null) baseline = output.latestCursor;
    const hasNewOutput = cursor > baseline;
    const quietForMs = Math.max(0, Math.floor(time - changed));
    if ((hasNewOutput && quietForMs >= settleMs) || time - started >= timeoutMs) {
      return { ...output, hasMore: output.cursor < output.latestCursor, hasNewOutput, quietForMs,
        waitStatus: hasNewOutput && quietForMs >= settleMs ? "settled" : hasNewOutput ? "streaming" : "no-output",
        timedOut: time - started >= timeoutMs };
    }
    await sleep(Math.min(50, timeoutMs - (time - started)), signal);
  }
}

// Long-running commands can be silent between progress updates. Do not stop
// on a quiet interval; only report a completion marker, interruption or deadline.
export async function monitorSerialExecution({ inspect, timeoutMs = 30000, signal,
  check = () => {}, now = () => performance.now(), sleep = pause }) {
  const deadline = now() + Math.max(100, Math.min(60000, timeoutMs));
  while (true) {
    signal?.throwIfAborted();
    check();
    const record = inspect();
    if (record.executionStatus === "completed" || record.observationClosed || record.delivery !== "sent") {
      return { ...record, timedOut: false };
    }
    if (record.waitingFor) return {...record,timedOut:false,waitStatus:"awaiting-input",next:"Target is waiting for interaction. Report the prompt; passwords must be entered by the user directly in the terminal."};
    if (now() >= deadline) return { ...record, timedOut: true,
      next: "Execution is unresolved. Monitor this same id again; do not resend the command." };
    await sleep(Math.min(500, deadline - now()), signal);
  }
}
