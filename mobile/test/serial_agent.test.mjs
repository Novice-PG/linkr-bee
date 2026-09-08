import assert from "node:assert/strict";
import test from "node:test";
import { createAssistantMessageEventStream } from "@earendil-works/pi-ai";
import { createSerialAgent, validateAgentConfig } from "../src/pi-agent.mjs";
import { SerialJournal } from "../../web/serial_journal.js";
import { createDeviceExecutor } from "../../web/device_executor.js";

test("serial journal preserves split UTF-8 and strips terminal controls", () => {
  const journal = new SerialJournal();
  const bytes = new TextEncoder().encode("\x1b[31m启动失败\x1b[0m\r\n");
  journal.append(bytes.slice(0, 7));
  journal.append(bytes.slice(7));
  assert.equal(journal.read().text, "启动失败\r\n");
  assert.equal(journal.read({ after: journal.read().cursor }).text, "");
});

test("serial journal caps retained data and reports missing history", () => {
  const journal = new SerialJournal(8);
  journal.append(new TextEncoder().encode("0123456789"));
  assert.deepEqual(Object.fromEntries(Object.entries(journal.read({ after: 0, limit: 3 })).filter(([key]) => key !== "updatedAt")),
    { text: "234", start: 2, cursor: 5, latestCursor: 10, truncated: true });
  journal.reset();
  assert.equal(journal.read().text, "");
  assert.equal(journal.read().latestCursor, 0);
});

test("model endpoint rejects embedded credentials and ambiguous URLs", () => {
  for (const endpoint of ["file:///tmp/model", "https://secret@host/v1", "https://host/v1?key=x", "https://host/v1#fragment"]) {
    assert.throws(() => validateAgentConfig({ endpoint, model: "model" }));
  }
  assert.equal(validateAgentConfig({ endpoint: "https://host/v1/", model: " model " }).endpoint, "https://host/v1");
});

function fakeStream(respond) {
  return (model, context) => {
    const stream = createAssistantMessageEventStream();
    queueMicrotask(() => {
      const content = respond(context);
      const message = { role: "assistant", api: model.api, provider: model.provider, model: model.id,
        timestamp: Date.now(), content,
        stopReason: content.some((item) => item.type === "toolCall") ? "toolUse" : "stop",
        usage: { input: 1, output: 1, cacheRead: 0, cacheWrite: 0, totalTokens: 2,
          cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } } };
      stream.push({ type: "done", reason: message.stopReason, message });
      stream.end(message);
    });
    return stream;
  };
}
const config = { endpoint: "https://model.invalid/v1", model: "test-model", apiKey: "test" };
const status = { sessionId: 1, connected: true };
const call = (name, args) => ({ type: "toolCall", id: `call-${Math.random()}`, name, arguments: args });

// Exercise the Pi adapter separately from the SDK-independent device executor.
function makeAgent({ getStatus, readLog, sendInput, ...options }) {
  return createSerialAgent({ ...options, device: {
    mode: "auto", getStatus, readLog, execute: sendInput,
    inspectExecution: () => ({ executionStatus: "unknown" }),
  } });
}

test("real Pi loop reads target logs and supplies them to the next model turn", async () => {
  let turns = 0;
  const agent = makeAgent({ config, getStatus: () => status,
    readLog: () => ({ text: "Kernel panic: unable to mount root", cursor: 40 }),
    sendInput: () => assert.fail("read-only diagnosis must not write"),
    stream: fakeStream((context) => {
      if (++turns === 1) return [call("read_serial_log", {})];
      assert.match(context.messages.at(-1).content[0].text, /Kernel panic/);
      return [{ type: "text", text: "Root filesystem could not mount." }];
    }),
  });
  await agent.prompt("Why did boot fail?");
  assert.equal(turns, 2);
});

test("Pi validates tool arguments before any serial write", async () => {
  let turns = 0;
  const events = [];
  const agent = makeAgent({ config, getStatus: () => status, readLog: () => ({}),
    sendInput: () => assert.fail("invalid arguments must not reach transport"),
    onEvent: (event) => events.push(event),
    stream: fakeStream(() => ++turns === 1
      ? [call("send_serial_input", { text: "uname", appendEnter: "yes" })]
      : [{ type: "text", text: "Invalid tool request." }]),
  });
  await agent.prompt("Inspect target");
  assert(events.some((event) => event.type === "tool_execution_end" && event.isError));
});

test("automatic diagnosis has a bounded number of model turns", async () => {
  let reads = 0;
  const agent = makeAgent({ config, getStatus: () => status,
    readLog: () => { reads++; return { text: "waiting" }; }, sendInput: async () => {},
    stream: fakeStream(() => [call("read_serial_log", {})]),
  });
  assert.equal((await agent.prompt("Inspect target")).limitReached, true);
  assert.equal(reads, 8);
});

test("exhausting the tool budget reports an interrupted run and resets for the next question", async () => {
  let reads = 0, turns = 0;
  const agent = makeAgent({ config, getStatus: () => status,
    readLog: () => { reads++; return { text: "waiting" }; },
    stream: fakeStream(() => {
      turns++;
      return Array.from({ length: 4 }, () => call("read_serial_log", {}));
    }),
  });
  assert.equal((await agent.prompt("Inspect target")).limitReached, true);
  assert.equal(reads, 16);
  assert.equal(turns, 5);
  assert.equal((await agent.prompt("Continue inspecting")).limitReached, true);
  assert.equal(reads, 32);
});

test("a final answer on the last allowed model turn is a completed diagnosis", async () => {
  let turns = 0;
  const agent = makeAgent({ config, getStatus: () => status, readLog: () => ({ text: "evidence" }),
    stream: fakeStream(() => ++turns < 8 ? [call("read_serial_log", {})]
      : [{ type: "text", text: "Diagnosis complete." }]),
  });
  assert.equal((await agent.prompt("Inspect target")).limitReached, false);
  assert.equal(turns, 8);
});

test("a mixed tool batch stops at the execution budget before another model request", async () => {
  let reads = 0, turns = 0;
  const agent = makeAgent({ config, getStatus: () => status,
    readLog: () => { reads++; return { text: "evidence" }; },
    stream: fakeStream(() => { turns++; return Array.from({ length: 20 }, () => call("read_serial_log", {})); }),
  });
  assert.equal((await agent.prompt("Inspect target")).limitReached, true);
  assert.equal(reads, 16);
  assert.equal(turns, 1);
});

test("a changed connection cannot reuse a previous device's agent context", async () => {
  let sessionId = 1;
  const agent = makeAgent({ config, getStatus: () => ({ sessionId }), readLog: () => ({}),
    sendInput: () => assert.fail("must not send to a new target"),
    stream: fakeStream(() => { sessionId = 2; return [call("send_serial_input", { text: "uname", appendEnter: true })]; }),
  });
  // A changed target fails before another model request can use stale context.
  await assert.rejects(agent.prompt("Inspect target"), /session or mode changed/);
  await assert.rejects(agent.prompt("continue"), /session or mode changed/);
});

test("real Pi loop processes two large incremental log reads without context overflow", async () => {
  const journal = new SerialJournal();
  journal.append(new TextEncoder().encode("EARLY_FAILURE" + "x".repeat(11987) + "y".repeat(11987) + "LATEST_RESULT"));
  let turns = 0;
  const agent = makeAgent({ config, getStatus: () => status, readLog: (options) => journal.read(options),
    sendInput: () => assert.fail("read-only"),
    stream: fakeStream((context) => {
      assert(JSON.stringify(context.messages).length <= 24000);
      if (++turns === 1) return [call("read_serial_log", { after: 0, limit: 12000 })];
      if (turns === 2) return [call("read_serial_log", { limit: 12000 })];
      const value = JSON.parse(context.messages.at(-1).content[0].text);
      assert.equal(value.start, 12000);
      assert.equal(value.cursor, 24000);
      assert(value.text.endsWith("LATEST_RESULT"));
      assert(JSON.stringify(context.messages).includes("EARLY_FAILURE"));
      return [{ type: "text", text: "Diagnosis complete." }];
    }),
  });
  await agent.prompt("Read this long log");
  assert.equal(turns, 3);
});

test("omitted log cursors read only new data, while recent explicitly rereads the tail", async () => {
  const journal = new SerialJournal();
  journal.append(new TextEncoder().encode("A"));
  let turns = 0;
  const agent = makeAgent({ config, getStatus: () => status, readLog: (options) => journal.read(options),
    stream: fakeStream((context) => {
      if (++turns === 1) return [call("read_serial_log", {})];
      const value = JSON.parse(context.messages.at(-1).content[0].text);
      assert.equal(value.text, turns === 2 ? "A" : turns === 3 ? "B" : "AB");
      if (turns === 2) { journal.append(new TextEncoder().encode("B")); return [call("read_serial_log", {})]; }
      if (turns === 3) return [call("read_serial_log", { recent: true })];
      return [{ type: "text", text: "Done" }];
    }),
  });
  await agent.prompt("Watch new output");
});

test("default log reads continue from an explicit historical page after reading the tail", async () => {
  const journal = new SerialJournal();
  journal.append(new TextEncoder().encode("ABCDEFGHIJ"));
  const pages = [];
  const requests = [{ limit: 2 }, { after: 0, limit: 2 }, { limit: 2 }];
  let turns = 0;
  const agent = makeAgent({ config, getStatus: () => status, readLog: (options) => journal.read(options),
    stream: fakeStream((context) => {
      if (turns > 0) pages.push(JSON.parse(context.messages.at(-1).content[0].text));
      if (turns < requests.length) return [call("read_serial_log", requests[turns++])];
      return [{ type: "text", text: "Historical paging continued without skipping output." }];
    }),
  });
  await agent.prompt("Read the tail, then page through the earlier output");
  assert.deepEqual(pages.map(({ text, start, cursor, hasMore }) => ({ text, start, cursor, hasMore })), [
    { text: "IJ", start: 8, cursor: 10, hasMore: false },
    { text: "AB", start: 0, cursor: 2, hasMore: true },
    { text: "CD", start: 2, cursor: 4, hasMore: true },
  ]);
});

test("default log reads continue from the last page returned by waiting for output", async () => {
  const journal = new SerialJournal();
  journal.append(new TextEncoder().encode("A".repeat(12000) + "NEXT_PAGE" + "Z".repeat(1000)));
  const pages = [];
  let turns = 0;
  const agent = makeAgent({ config, getStatus: () => status, readLog: (options) => journal.read(options),
    stream: fakeStream((context) => {
      if (++turns === 1) return [call("read_serial_log", { limit: 2 })];
      pages.push(JSON.parse(context.messages.at(-1).content[0].text));
      if (turns === 2) return [call("wait_for_serial_output", { after: 0, timeoutMs: 100, settleMs: 100 })];
      if (turns === 3) return [call("read_serial_log", { limit: 9 })];
      return [{ type: "text", text: "Continued from the observed page." }];
    }),
  });
  await agent.prompt("Read the tail, then observe and page through earlier output");
  assert.equal(pages[0].text, "ZZ");
  assert.equal(pages[1].cursor, 12000);
  assert.equal(pages[1].hasMore, true);
  assert.equal(pages[2].text, "NEXT_PAGE");
  assert.equal(pages[2].start, 12000);
});

function executableDevice({ onRecord, output = "DONE\r\nroot@board:~# " } = {}) {
  const journal = new SerialJournal();
  const sent = [];
  let inputRevision = 0;
  const device = createDeviceExecutor({ getStatus: () => ({ ...status, inputRevision, inputPending: false }),
    readLog: (options) => journal.read(options), prepareInput: ({ text, appendEnter }) => text + (appendEnter ? "\r" : ""),
    sendInput: async (payload) => { sent.push(payload); journal.append(new TextEncoder().encode(output)); return { inputRevision: ++inputRevision }; }, onRecord });
  return { device, sent };
}

test("Full Auto must receive inspection results in a later model turn before another send", async () => {
  const { device, sent } = executableDevice();
  device.setMode("full-auto");
  let turns = 0;
  const errors = [];
  const send = () => call("send_serial_input", { text: "pwd", appendEnter: true });
  const agent = createSerialAgent({ config, device, onEvent: (event) => {
    if (event.type === "tool_execution_end" && event.isError) errors.push(event.result);
  }, stream: fakeStream((context) => {
    if (++turns === 1) return [send(), send()];
    if (turns === 2) { assert.equal(sent.length, 1); return [call("inspect_serial_execution", { id: "serial-1" }), send()]; }
    if (turns === 3) {
      assert.equal(sent.length, 1);
      const inspected = context.messages.filter((m) => m.role === "toolResult" && m.toolName === "inspect_serial_execution").at(-1);
      assert.equal(JSON.parse(inspected.content[0].text).waitStatus, "settled");
      return [send()];
    }
    return [{ type: "text", text: "Verified first step; second input sent." }];
  }) });
  await agent.prompt("Inspect the device");
  assert.equal(sent.length, 2);
  assert.equal(errors.length, 2);
});

test("changing mode retains prior dialogue and closes interrupted tool batches without replay", async () => {
  let agent, cancelNext = false, turns = 0;
  const { device, sent } = executableDevice({ onRecord: (record) => {
    if (cancelNext && record.state === "awaiting-approval") {
      cancelNext = false;
      queueMicrotask(() => { agent.abort(); device.setMode("full-auto"); });
    }
  } });
  agent = createSerialAgent({ config, device, stream: fakeStream((context) => {
    if (++turns === 1) return [{ type: "text", text: "Remember: this board has a mount failure." }];
    if (turns === 2) return [call("send_serial_input", { text: "reboot", appendEnter: true }), call("send_serial_input", { text: "pwd", appendEnter: true })];
    assert(context.systemPrompt.includes("Execution mode: Full Auto"));
    assert(JSON.stringify(context.messages).includes("mount failure"));
    const requests = context.messages.flatMap((m) => m.content.filter((c) => c.type === "toolCall").map((c) => c.id));
    const results = context.messages.filter((m) => m.role === "toolResult").map((m) => m.toolCallId);
    assert.deepEqual(new Set(requests), new Set(results));
    assert(JSON.stringify(context.messages).includes("interrupted"));
    return [{ type: "text", text: "Continue diagnosing the mount failure." }];
  }) });
  await agent.prompt("Diagnose this board");
  cancelNext = true;
  await agent.prompt("Propose a restart").catch(() => {});
  assert.equal(sent.length, 0);
  await agent.prompt("Continue analysis only");
  assert.equal(sent.length, 0);
});

test("input delivered just before abort still needs inspection when the dialogue resumes", async () => {
  let agent, turns = 0, interrupted = false;
  const { device, sent } = executableDevice({ onRecord: (record) => {
    if (!interrupted && record.state === "sending") {
      interrupted = true;
      queueMicrotask(() => agent.abort());
    }
  } });
  device.setMode("full-auto");
  agent = createSerialAgent({ config, device, stream: fakeStream((context) => {
    if (++turns <= 2) return [call("send_serial_input", { text: "pwd", appendEnter: true })];
    assert.equal(sent.length, 1);
    assert.match(context.messages.at(-1).content[0].text, /Inspect execution serial-1/);
    return [{ type: "text", text: "The interrupted send must be inspected first." }];
  }) });
  await agent.prompt("Inspect the board").catch(() => {});
  assert.equal(device.getRecords()[0].delivery, "sent");
  await agent.prompt("Continue");
  assert.equal(sent.length, 1);
});
