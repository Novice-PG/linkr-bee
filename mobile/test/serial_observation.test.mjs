import assert from "node:assert/strict";
import test from "node:test";
import { waitForSerialOutput } from "../../web/serial_observation.js";

function clocked(read) {
  let time = 0;
  return { now: () => time, sleep: async (ms) => { time += ms; },
    readLog: () => { const text = read(time); return { text, start: 0, cursor: text.length, latestCursor: text.length }; } };
}

test("serial observation collects delayed fragments until the quiet interval", async () => {
  const clock = clocked((time) => time >= 350 ? "ABC" : time >= 150 ? "AB" : "A");
  const output = await waitForSerialOutput({ ...clock, after: 0, timeoutMs: 1000, settleMs: 200 });
  assert.equal(output.text, "ABC");
  assert.equal(output.waitStatus, "settled");
  assert.equal(output.quietForMs, 200);
  assert.equal(output.timedOut, false);
  assert.equal(clock.now(), 550);
});

test("silence and continuous output end at the deadline with different states", async () => {
  for (const [read, expected] of [[() => "", "no-output"], [(time) => "x".repeat(time / 50 + 1), "streaming"]]) {
    const clock = clocked(read);
    const output = await waitForSerialOutput({ ...clock, after: 0, timeoutMs: 300, settleMs: 200 });
    assert.equal(output.waitStatus, expected);
    assert.equal(output.timedOut, true);
    assert.equal(clock.now(), 300);
  }
});

test("abort and session changes interrupt output observation", async () => {
  const controller = new AbortController();
  const clock = clocked(() => "");
  await assert.rejects(waitForSerialOutput({ ...clock, after: 0, signal: controller.signal,
    sleep: async () => controller.abort() }), { name: "AbortError" });
  let checks = 0;
  await assert.rejects(waitForSerialOutput({ ...clock, after: 0,
    check: () => { if (++checks > 1) throw new Error("Session changed"); } }), /Session changed/);
});
