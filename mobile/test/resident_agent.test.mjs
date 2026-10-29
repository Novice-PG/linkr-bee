import assert from "node:assert/strict";
import test from "node:test";
import {
  createResidentAgent,
  RESIDENT_MAX_QUEUE,
  RESIDENT_SESSION_KEY,
  validateResidentTaskArgs,
} from "../../web/resident_agent.js";

const CONFIG = {
  endpoint: "https://api.example.test/v1",
  model: "test-model",
  apiKey: "secret-key",
  provider: "openai-completions",
  reasoning: "off",
  contextWindow: 128000,
  maxTokens: 4096,
};

function fakeStorage() {
  const map = new Map();
  return {
    map,
    getItem(key) { return map.has(key) ? map.get(key) : null; },
    setItem(key, value) { map.set(key, String(value)); },
    removeItem(key) { map.delete(key); },
  };
}

function fakeBaseDevice(calls = []) {
  return {
    mode: "auto",
    getStatus: () => ({ connected: true, sessionId: 7, deviceId: "dev-1", transport: "ble", uart: "115200,8,n,1,n" }),
    readLog: () => ({ text: "", latestCursor: 0 }),
    getRecords: () => [],
    inspectExecution: () => ({ id: "serial-1", state: "sent", delivery: "sent" }),
    observe() {},
    execute: async (args, _signal, options) => {
      calls.push({ args, options });
      return { id: "serial-1", state: "sent", delivery: "sent", executionStatus: "completed", exitCode: 0 };
    },
  };
}

function fakeRunner(text, { onEvent, block, snapshot } = {}) {
  let release;
  let aborted = false;
  const gate = block ? new Promise((resolve) => { release = resolve; }) : null;
  return {
    release() { release?.(); },
    async prompt(promptText) {
      if (gate) await gate;
      if (aborted) throw new Error("aborted by test");
      onEvent?.({
        type: "message_end",
        message: {
          role: "assistant",
          content: [{ type: "text", text: `${text}: ${promptText}` }],
          usage: { input: 12, output: 7, totalTokens: 19 },
        },
      });
      return { limitReached: false, unresolvedExecutions: 0 };
    },
    abort() { aborted = true; release?.(); },
    snapshot: () => snapshot ?? [{ role: "user", content: text }, { role: "assistant", content: text }],
  };
}

function build({ config = CONFIG, busy = false, block = false, runnerSnapshot } = {}) {
  const storage = fakeStorage();
  const calls = [];
  const created = [];
  const readPrompts = [];
  const runners = [];
  const events = [];
  let runnerText = "answer";
  const agent = createResidentAgent({
    storage,
    maxQueue: RESIDENT_MAX_QUEUE,
    getConfig: () => config,
    getDevice: () => fakeBaseDevice(calls),
    readBoardPrompt: async () => {
      readPrompts.push(true);
      return "board instruction";
    },
    isHumanAgentBusy: () => busy,
    notes: null,
    createAgent: async (options) => {
      created.push(options);
      const runner = fakeRunner(runnerText, {
        onEvent: (event) => {
          options.onEvent?.(event);
          events.push(event);
        },
        block,
        snapshot: runnerSnapshot,
      });
      runners.push(runner);
      return runner;
    },
  });
  return {
    agent, storage, calls, created, readPrompts, runners, events,
    setRunnerText(value) { runnerText = value; },
    runnerOptions: (index = 0) => created[index],
  };
}

async function waitForState(agent, taskId, states, timeoutMs = 2000) {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const task = await agent.getTask(taskId);
    if (states.includes(task.state)) return task;
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${states.join("/")}: ${task.state}`);
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
}

test("arguments and readiness are validated before a task is queued", async () => {
  for (const args of [null, {}, { request: "   " }, { request: "x".repeat(12001) },
    { request: "ok", sessionId: "bad id" }, { request: "ok", maxTurns: 0 },
    { request: "ok", maxTools: 999 }]) {
    assert.throws(() => validateResidentTaskArgs(args), /object|request|sessionId|maxTurns|maxTools/);
  }
  const noConfig = createResidentAgent({
    storage: fakeStorage(),
    getConfig: () => null,
    getDevice: () => fakeBaseDevice(),
    createAgent: async () => { throw new Error("must not run"); },
  });
  assert.equal(noConfig.getStatus().available, false);
  assert.throws(() => noConfig.startTask({ request: "hello" }), /AI configuration/);
});

test("a task runs with an already-approved device and records result, tools and usage", async () => {
  const harness = build();
  const task = harness.agent.startTask({ request: "check the target", sessionId: "s1" });
  assert.ok(["queued", "running"].includes(task.state));
  const done = await waitForState(harness.agent, task.taskId, ["completed"]);
  assert.match(done.result, /answer: check the target/);
  assert.equal(done.usage.input, 12);
  assert.equal(done.usage.output, 7);
  assert.equal(done.usage.totalTokens, 19);
  assert.equal(done.toolCalls.length, 0);

  const options = harness.runnerOptions(0);
  assert.equal(options.customInstructions, "board instruction");
  assert.equal(options.device.mode, "full-auto");
  await options.device.execute({ text: "uname -a", appendEnter: true });
  assert.equal(harness.calls[0].options.userApproved, true);
  assert.deepEqual(harness.calls[0].args, { text: "uname -a", appendEnter: true });
  /* board instructions are read once per session. */
  assert.equal(harness.readPrompts.length, 1);
});

test("tasks are serialized and queued", async () => {
  const harness = build({ block: true });
  const first = harness.agent.startTask({ request: "first" });
  const second = harness.agent.startTask({ request: "second" });
  assert.equal((await harness.agent.getTask(first.taskId)).state, "running");
  assert.equal((await harness.agent.getTask(second.taskId)).state, "queued");
  assert.equal(harness.created.length, 1);
  const list = harness.agent.listTasks();
  assert.equal(list.length, 2);
  assert.equal(list[0].taskId, second.taskId);

  const canceled = await harness.agent.cancelTask(second.taskId);
  assert.equal(canceled.state, "canceled");
  /* Canceling the active task releases the runner; the canceled queued task
     must not start afterwards. */
  await harness.agent.cancelTask(first.taskId);
  await waitForState(harness.agent, first.taskId, ["canceled"]);
  await new Promise((resolve) => setTimeout(resolve, 30));
  assert.equal(harness.created.length, 1);
  harness.agent.dispose();
});

test("a session is continued with its messages and newSession starts clean", async () => {
  const snapshot = [{ role: "user", content: "old" }, { role: "assistant", content: "old answer" }];
  const harness = build({ runnerSnapshot: snapshot });
  const first = harness.agent.startTask({ request: "first", sessionId: "shared" });
  await waitForState(harness.agent, first.taskId, ["completed"]);
  const second = harness.agent.startTask({ request: "second", sessionId: "shared" });
  await waitForState(harness.agent, second.taskId, ["completed"]);
  assert.equal(harness.created.length, 2);
  assert.deepEqual(harness.runnerOptions(0).restoredMessages, null);
  assert.deepEqual(harness.runnerOptions(1).restoredMessages, snapshot);

  const fresh = harness.agent.startTask({ request: "third", newSession: true });
  await waitForState(harness.agent, fresh.taskId, ["completed"]);
  assert.notEqual(fresh.sessionId, "shared");
  assert.equal(harness.runnerOptions(2).restoredMessages, null);
  const stored = JSON.parse(harness.storage.getItem(RESIDENT_SESSION_KEY));
  assert.ok(stored.sessions.shared);
});

test("canceling a queued task removes it before it starts", async () => {
  const harness = build({ block: true });
  const running = harness.agent.startTask({ request: "running" });
  const queued = harness.agent.startTask({ request: "queued" });
  await new Promise((resolve) => setTimeout(resolve, 20));
  const canceled = await harness.agent.cancelTask(queued.taskId);
  assert.equal(canceled.state, "canceled");
  assert.equal((await harness.agent.getTask(running.taskId)).state, "running");
  await harness.agent.cancelTask(running.taskId);
  await waitForState(harness.agent, running.taskId, ["canceled"]);
  assert.equal(harness.created.length, 1);
  harness.agent.dispose();
});

test("a busy human conversation or disabled agent blocks new tasks", async () => {
  const busy = build({ busy: true });
  assert.throws(() => busy.agent.startTask({ request: "hello" }), /in-app assistant is busy/);

  const disabled = build({ config: null });
  assert.throws(() => disabled.agent.startTask({ request: "hello" }), /AI configuration/);
});

test("status exposes model details but never the key or headers", () => {
  const harness = build();
  const status = harness.agent.getStatus();
  assert.equal(status.available, true);
  assert.equal(status.model.model, "test-model");
  assert.equal(status.model.endpoint, "https://api.example.test");
  assert.equal(JSON.stringify(status).includes("secret-key"), false);
  assert.equal(JSON.stringify(status).toLowerCase().includes("apikey"), false);
});
