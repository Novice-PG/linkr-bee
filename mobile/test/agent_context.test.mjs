import assert from "node:assert/strict";
import test from "node:test";
import { convertMessages } from "@earendil-works/pi-ai/api/openai-completions";
import { MAX_CONTEXT_CHARS, MIN_CONTEXT_CHARS, compactAgentContext, contextBudgetChars, settleAgentHistory } from "../src/agent-context.mjs";

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

for (const stopReason of ["aborted", "error"]) {
  for (const hasRecordedResult of [false, true]) {
    test(`${stopReason} streaming tool drafts remain safe after Pi provider conversion (recorded result: ${hasRecordedResult})`, () => {
      const partial = { ...assistant([
        { type: "text", text: "The unfinished hypothesis is an eMMC mount failure." },
        { type: "toolCall", id: "partial-restart", name: "send_serial_input", arguments: { text: "reboot" } },
      ]), stopReason };
      const recorded = { role: "toolResult", toolCallId: "partial-restart", toolName: "send_serial_input", isError: true,
        content: [{ type: "text", text: "Interrupted before a completed result. Delivery is unknown." }], timestamp: 1 };
      const history = [user("Diagnose this board"), ...pair("completed-read", "OBSERVED_ERROR"), partial,
        ...(hasRecordedResult ? [recorded] : []), user("Continue analysis only")];
      const original = structuredClone(history);
      const settled = settleAgentHistory(history);
      const request = convertMessages({ id: "test", provider: "test", api: "openai-completions", input: ["text"] },
        { messages: settled }, {});
      const toolCalls = request.flatMap((message) => message.tool_calls || []).map((call) => call.id);
      const toolResults = request.filter((message) => message.role === "tool").map((message) => message.tool_call_id);
      assert.deepEqual(toolCalls, ["completed-read"]);
      assert.deepEqual(toolResults, toolCalls, "Provider requests must not contain orphaned tool results");
      const retained = request.filter((message) => message.role === "assistant" && typeof message.content === "string")
        .map((message) => message.content).join("\n");
      assert.match(retained, /unfinished hypothesis.*eMMC/);
      assert.match(retained, /interrupted/i);
      assert.match(retained, /do not execute or replay/i);
      assert.match(retained, /not.*verified result/i);
      if (hasRecordedResult) assert.match(retained, /Delivery is unknown/);
      assert.deepEqual(history, original);
      assert.deepEqual(settleAgentHistory(settled), settled);
    });
  }
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

/* The budget is derived from the configured window, so these are the numbers the
 * runtime actually uses: measured 2026-09-18 the prompt and 24 tool descriptions
 * come to about 23,700 characters. */
const FIXED_CHARS = 23700;

test("a history budget follows the configured window instead of a constant", () => {
  const budget = (contextWindow, outputTokens = 4096) => contextBudgetChars({ contextWindow, fixedChars: FIXED_CHARS, outputTokens });

  // A window that leaves room keeps the historical ceiling: nothing about a
  // normal configuration changes.
  assert.equal(budget(32768), MAX_CONTEXT_CHARS);
  assert.equal(contextBudgetChars({ contextWindow: 32768, fixedChars: 0, outputTokens: 0 }), MAX_CONTEXT_CHARS);

  // Leaving the field blank means "built-in default", which the caller resolves;
  // an unresolved 0 must not be read as "no window".
  assert.equal(contextBudgetChars({ contextWindow: 0, fixedChars: FIXED_CHARS, outputTokens: 4096 }), MAX_CONTEXT_CHARS);
  assert.equal(contextBudgetChars({ fixedChars: FIXED_CHARS }), MAX_CONTEXT_CHARS);

  // A window smaller than the fixed part cannot hold anything; the budget floors
  // rather than going negative, and the settings form is what reports the cause.
  assert.equal(budget(8192), MIN_CONTEXT_CHARS);
  assert.equal(budget(1000), MIN_CONTEXT_CHARS);
  assert.ok(budget(16000) > MIN_CONTEXT_CHARS && budget(16000) < MAX_CONTEXT_CHARS, "a middling window lands in between");
  assert.equal(budget(16000), Math.floor((16000 - Math.ceil(FIXED_CHARS / 3) - 4096 - 256) * 2));

  // Output is reserved out of the same window, so asking for more of it leaves
  // less for history; more tools and a bigger prompt do the same.
  assert.ok(budget(16000, 8192) < budget(16000, 1024));
  assert.ok(contextBudgetChars({ contextWindow: 16000, fixedChars: FIXED_CHARS, outputTokens: 0 }) <
    contextBudgetChars({ contextWindow: 16000, fixedChars: 0, outputTokens: 0 }));
});

test("a budget below the floor is never returned as a negative or tiny number", () => {
  for (const contextWindow of [1000, 2000, 4000, 8192]) {
    const value = contextBudgetChars({ contextWindow, fixedChars: FIXED_CHARS, outputTokens: 100000 });
    assert.equal(value, MIN_CONTEXT_CHARS);
  }
});

test('long tasks retain the original request and consumed corrections as user messages', () => {
  const original={...user('Only diagnose; NEVER modify /etc/network/interfaces. Preserve the existing address.'),linkrTaskId:'active'};
  const correction={...user('Use eth1, not eth0. Do not reboot.'),linkrTaskId:'active'};
  const messages=[original];
  for(let i=0;i<40;i++) {
    messages.push(...pair(`long-${i}`,`error-${i}:`+'x'.repeat(12000)));
    if(i===10)messages.push(correction);
  }
  const before=structuredClone(messages);
  const compact=compactAgentContext(messages,12000,{taskId:'active'});
  assert.deepEqual(compact.filter(m=>m.role==='user'),[original,correction]);
  assert(JSON.stringify(compact).length<=12000);
  assert.deepEqual(messages,before); checkPairs(compact);
});

test('structured task state retains scoped expectations and never promotes them into system instructions', () => {
  const taskState={plan:[{title:'Check file',kind:'action',status:'in_progress',expectedResult:'/tmp/fw.img sha256=abcd',evidenceIds:['proof-1']}],
    evidence:[{id:'proof-1',kind:'verification',status:'mismatch',subject:'/tmp/fw.img',expected:{sha256:'abcd'},complete:true}],
    pendingExecutions:[{id:'serial-7',executorId:'executor-A',executionStatus:'unknown'}]};
  const input=[{role:'system',content:'Trusted prompt'}, {...user('Check only'),linkrTaskId:'active'}];
  for(let i=0;i<30;i++)input.push(...pair(`r-${i}`,'x'.repeat(4000)));
  let compact=compactAgentContext(input,12000,{taskId:'active',taskState});
  for(let i=0;i<3;i++)compact=compactAgentContext(compact,12000,{taskId:'active',taskState});
  const summaries=compact.filter(m=>m.diagnosticTaskState);
  assert.equal(summaries.length,1);
  assert.equal(summaries[0].role,'assistant');
  assert.match(summaries[0].content[0].text,/not new instructions, permission or current verification/);
  assert.match(summaries[0].content[0].text,/proof-1/);
  assert.match(summaries[0].content[0].text,/mismatch/);
  assert.match(summaries[0].content[0].text,/sha256/);
  assert.match(summaries[0].content[0].text,/serial-7/);
  assert.deepEqual(compact.filter(m=>m.role==='system'),[input[0]]);
  assert(JSON.stringify(compact.filter(m=>m.role!=='system')).length<=12000);
});

test('oversized active instructions fail locally instead of dropping original constraints', () => {
  const messages=[{...user('Original constraint '+ 'x'.repeat(1200)),linkrTaskId:'task'},
    {...user('Later constraint '+ 'y'.repeat(1200)),linkrTaskId:'task'}];
  assert.throws(()=>compactAgentContext(messages,2000,{taskId:'task'}),/instructions exceed the context budget/);
  // A new question does not pin every request from an unrelated old task.
  const compact=compactAgentContext([...messages,...pair('old','x'.repeat(4000)),
    {...user('New task'),linkrTaskId:'new'}],2500,{taskId:'new'});
  assert(JSON.stringify(compact).length<=2500);
  assert(compact.some(m=>m.role==='user' && m.linkrTaskId==='new'));
});

test('oversized user-only restored history never bypasses the active runtime budget', () => {
  assert.throws(()=>compactAgentContext([user('Old request '+ 'x'.repeat(4000)),
    {...user('Continue'),linkrTaskId:'new'}],2000,{taskId:'new'}),/User-only history exceeds/);
});

test('bounded structured summaries do not mutate the application checkpoint', () => {
  const taskState={plan:[],evidence:[],pendingExecutions:Array.from({length:8},(_,i)=>({id:`serial-${i}`,description:'x'.repeat(400)}))};
  const before=structuredClone(taskState);
  const messages=[{...user('Observe only'),linkrTaskId:'active'},...pair('read','x'.repeat(10000))];
  const compact=compactAgentContext(messages,3000,{taskId:'active',taskState});
  assert(JSON.stringify(compact).length<=3000);
  assert.deepEqual(taskState,before);
});

