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
  while (true) {
    signal?.throwIfAborted();
    check();
    const output = readLog({ after });
    const time = now();
    if (cursor !== output.latestCursor) { cursor = output.latestCursor; changed = time; }
    const hasNewOutput = cursor > after;
    const quietForMs = Math.max(0, Math.floor(time - changed));
    if ((hasNewOutput && quietForMs >= settleMs) || time - started >= timeoutMs) {
      return { ...output, hasMore: output.cursor < output.latestCursor, hasNewOutput, quietForMs,
        waitStatus: hasNewOutput && quietForMs >= settleMs ? "settled" : hasNewOutput ? "streaming" : "no-output",
        timedOut: time - started >= timeoutMs };
    }
    await sleep(Math.min(50, timeoutMs - (time - started)), signal);
  }
}
