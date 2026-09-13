import { test } from "node:test";
import assert from "node:assert/strict";
import { createAssistantMessageEventStream } from "@earendil-works/pi-ai";
import { createSerialAgent } from "../src/pi-agent.mjs";
import { createDeviceExecutor } from "../../web/device_executor.js";
import { SerialJournal } from "../../web/serial_journal.js";

const tool = (name, args = {}) => ({ type: "toolCall", id: crypto.randomUUID(), name, arguments: args });
const write = () => tool("send_serial_input", { text: "touch /tmp/recovery-example", appendEnter: true });
const done = () => [{ type: "text", text: "Mock response complete." }];
const blocked = context => context.messages.some(message => message.role === "toolResult" &&
  message.isError && JSON.stringify(message.content).includes("Recovered history is unverified"));

function fixture(source, respond) {
  const journal = new SerialJournal();
  journal.append(new TextEncoder().encode("root@board:~# "));
  let revision = 0, round = 0;
  const sent = [];
  const device = createDeviceExecutor({
    getStatus: () => ({ connected: true, sessionId: 1, inputRevision: revision, inputPending: false }),
    readLog: options => journal.read(options),
    prepareInput: ({ text, appendEnter }) => text + (appendEnter ? "\r" : ""),
    sendInput: async text => { sent.push(text); return { inputRevision: ++revision }; },
  });
  device.setMode("full-auto");
  const agent = createSerialAgent({
    config: { endpoint: "https://agent.test/v1", model: "test-model", apiKey: "test-key" },
    device,
    restoredMessages: source === "chat" ? [{ role: "user", content: "Repair the service.", timestamp: 1 }] : null,
    stream: (model, context) => {
      const content = respond(++round, context, sent);
      const message = {
        role: "assistant", api: model.api, provider: model.provider, model: model.id, timestamp: Date.now(),
        content, stopReason: content.some(part => part.type === "toolCall") ? "toolUse" : "stop",
        usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0,
          cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } },
      };
      const stream = createAssistantMessageEventStream();
      stream.push({ type: "done", reason: message.stopReason, message });
      return stream;
    },
  });
  const recovery = source === "task" ? { goal: "Repair the service", historical: true } : null;
  return { agent, device, sent, recovery };
}

for (const source of ["chat", "task"]) {
  test(`${source} recovery blocks writes and keeps the gate across questions`, async () => {
    const f = fixture(source, (round, context) => {
      if (round % 2 === 1) return [write()];
      assert.equal(blocked(context), true);
      return done();
    });
    try {
      await f.agent.prompt("Continue the repair.", { recovery: f.recovery });
      assert.deepEqual(f.sent, []);
      // A second question must not silently clear an unfinished recovery.
      await f.agent.prompt("Continue.");
      assert.deepEqual(f.sent, []);
    } finally { f.device.setMode("auto"); }
  });

  test(`${source} recovery releases writes only after both reads reach the model`, async () => {
    const f = fixture(source, (round, context, sent) => {
      if (round === 1) return [tool("get_device_status")];
      if (round === 2) return [write()];
      if (round === 3) {
        assert.equal(blocked(context), true);
        assert.deepEqual(sent, []);
        // Even the second read cannot authorize a write in its own batch.
        return [tool("read_serial_log"), write()];
      }
      if (round === 4) {
        assert.deepEqual(sent, []);
        const results = context.messages.filter(message => message.role === "toolResult");
        assert.ok(results.some(result => result.toolName === "read_serial_log" && !result.isError));
        assert.ok(results.some(result => result.toolName === "get_device_status" && !result.isError));
        return [write()];
      }
      return done();
    });
    try {
      await f.agent.prompt("Continue the repair.", { recovery: f.recovery });
      assert.deepEqual(f.sent, ["touch /tmp/recovery-example\r"]);
    } finally { f.device.setMode("auto"); }
  });
}

test("a new conversation does not inherit a recovery gate", async () => {
  const f = fixture(null, round => round === 1 ? [write()] : done());
  try {
    await f.agent.prompt("Create the example file.");
    assert.deepEqual(f.sent, ["touch /tmp/recovery-example\r"]);
  } finally { f.device.setMode("auto"); }
});
