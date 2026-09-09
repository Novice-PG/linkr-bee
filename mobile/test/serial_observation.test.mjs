import { monitorSerialExecution } from "../../web/serial_observation.js";
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

test("execution monitoring waits through silence and returns explicit completion", async () => {
  const { monitorSerialExecution } = await import("../../web/serial_observation.js");
  let time = 0;
  const result = await monitorSerialExecution({ timeoutMs: 30000, now: () => time,
    sleep: async ms => { time += ms; },
    inspect: () => ({ delivery: "sent", executionStatus: time >= 12000 ? "completed" : "unknown" }) });
  assert.equal(time, 12000);
  assert.equal(result.timedOut, false);
});

test("monitor deadline stays unresolved and cancellation does not send input", async () => {
  const { monitorSerialExecution } = await import("../../web/serial_observation.js");
  let time = 0;
  const options = { timeoutMs: 1000, now: () => time, sleep: async ms => { time += ms; },
    inspect: () => ({ delivery: "sent", executionStatus: "unknown" }) };
  assert.equal((await monitorSerialExecution(options)).timedOut, true);
  const abort = new AbortController();
  await assert.rejects(monitorSerialExecution({ ...options, signal: abort.signal,
    sleep: async () => abort.abort(new Error("Stopped")) }), /Stopped/);
});

test('monitor returns immediately for interactive input, but completion takes precedence', async () => {
  const inspect=()=>({delivery:'sent',waitingFor:'sudo-password'});
  const result=await monitorSerialExecution({inspect,sleep:()=>{throw new Error('must not wait');}});
  assert.equal(result.waitStatus,'awaiting-input');
  const done=await monitorSerialExecution({inspect:()=>({...inspect(),executionStatus:'completed'})});
  assert.equal(done.waitStatus,undefined);
});
