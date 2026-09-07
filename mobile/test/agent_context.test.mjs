import assert from "node:assert/strict";
import test from "node:test";
import { compactAgentContext } from "../src/agent-context.mjs";

const user = (text) => ({ role: "user", content: [{ type: "text", text }], timestamp: 1 });
const assistant = (content) => ({ role: "assistant", content, api: "openai-completions", provider: "test", model: "test", timestamp: 1, stopReason: "toolUse" });
const pair = (id, value) => [
  assistant([{ type: "toolCall", name: "read_serial_log", id, arguments: { after: 0 } }]),
  { role: "toolResult", toolName: "read_serial_log", toolCallId: id, content: [{ type: "text", text: JSON.stringify({ text: value, start: 0, cursor: value.length, latestCursor: value.length }) }], timestamp: 1, isError: false },
];

function checkPairs(messages) {
  const calls = new Set(), results = new Set();
  for (const message of messages) {
    for (const part of message.content) if (part.type === "toolCall") calls.add(part.id);
    if (message.role === "toolResult") { assert(calls.has(message.toolCallId)); results.add(message.toolCallId); }
  }
  assert.deepEqual(calls, results);
}

test("two large log reads fit without losing current evidence or changing original messages", () => {
  const messages = [user("Explain boot failure"), ...pair("first", "EARLY_ERROR" + "x".repeat(12000)), ...pair("second", "y".repeat(12000) + "LATEST_ERROR")];
  const original = structuredClone(messages);
  const compact = compactAgentContext(messages);
  assert(JSON.stringify(compact).length <= 24000);
  assert(JSON.stringify(compact).includes("EARLY_ERROR"));
  assert(JSON.stringify(compact).includes("LATEST_ERROR"));
  assert(JSON.stringify(compact).includes("contextExcerpt"));
  assert.deepEqual(messages, original);
  checkPairs(compact);
});

test("long single-question investigations compact complete tool exchanges", () => {
  const messages = [user("CURRENT_GOAL")];
  for (let i = 0; i < 40; i++) messages.push(...pair(`read-${i}`, `evidence-${i}:` + "x".repeat(12000)));
  const compact = compactAgentContext(messages);
  assert(JSON.stringify(compact).length <= 24000);
  assert(compact.some((message) => message.role === "user" && message.content[0].text === "CURRENT_GOAL"));
  assert(JSON.stringify(compact).includes("evidence-39"));
  assert(compact.some((message) => message.diagnosticMemory));
  checkPairs(compact);
});

test("repeated compaction retains the latest question and marks earlier excerpts as history", () => {
  let messages = [];
  for (let i = 0; i < 50; i++) {
    messages.push(user(`Question ${i}`), ...pair(`call-${i}`, "z".repeat(16000)));
    messages = compactAgentContext(messages);
    assert(JSON.stringify(messages).length <= 24000);
    checkPairs(messages);
  }
  assert(messages.some((message) => message.role === "user" && message.content[0].text === "Question 49"));
  assert(JSON.stringify(messages).includes("never replay"));
});
