const MAX_CONTEXT_CHARS = 24000;

// Pi stops a sequential batch on abort; later calls in that batch may have no
// result. Complete those pairs as interrupted history before reusing a session.
export function settleAgentHistory(messages) {
  const settled = [];
  for (let i = 0; i < messages.length; i++) {
    const message = messages[i];
    settled.push(message);
    if (message.role !== "assistant") continue;
    const calls = message.content.filter((part) => part.type === "toolCall");
    if (!calls.length) continue;
    const completed = new Set();
    while (messages[i + 1]?.role === "toolResult") {
      const result = messages[++i];
      completed.add(result.toolCallId);
      settled.push(result);
    }
    for (const call of calls) if (!completed.has(call.id)) settled.push({
      role: "toolResult", toolCallId: call.id, toolName: call.name, isError: true, timestamp: message.timestamp,
      content: [{ type: "text", text: "Run interrupted before a completed tool result was recorded. Delivery is unknown; do not replay this request. Inspect current device state and execution records before deciding on a new action." }],
    });
  }
  return settled;
}

// Mechanical excerpts avoid a second model call and never promote logs into
// facts. JSON metadata and the exact recent tool call/result pairs stay intact.
export function excerpt(value, limit) {
  if (value.length <= limit) return value;
  const marker = "\n[... excerpt; intervening content omitted ...]\n";
  const room = Math.max(0, limit - marker.length);
  const head = Math.floor(room / 2);
  return value.slice(0, head) + marker + value.slice(-(room - head));
}

function toolExcerpt(message, limit) {
  return { ...message, content: message.content.map((part) => {
    if (part.type !== "text" || part.text.length <= limit) return part;
    try {
      const data = JSON.parse(part.text);
      for (const key of ["text", "evidence"]) {
        if (typeof data[key] === "string" && data[key].length > limit) {
          data[key] = excerpt(data[key], limit);
          data.contextExcerpt = true;
          data.contextNote = "Evidence abbreviated in model context. Cursor offsets describe the original range; reread that range if needed.";
        }
      }
      return { ...part, text: JSON.stringify(data) };
    } catch { return { ...part, text: excerpt(part.text, limit) }; }
  }) };
}

function summarize(messages) {
  return messages.flatMap((message) => {
    if (message.diagnosticMemory) return message.diagnosticMemory;
    const text = message.content.filter((part) => part.type === "text").map((part) => part.text).join("\n");
    const calls = message.content.filter((part) => part.type === "toolCall").map((part) => ({ tool: part.name, input: part.arguments }));
    return [{ source: message.role, tool: message.toolName, error: message.isError || undefined,
      excerpt: excerpt(text || JSON.stringify(calls), message.role === "user" ? 1000 : 1800) }];
  });
}

function memoryMessage(template, entries) {
  const recent = entries.slice(-12);
  // Keep the summary itself bounded, even across many compactions.
  while (JSON.stringify(recent).length > 6000 && recent.length > 1) recent.shift();
  return { ...template, role: "assistant", stopReason: "stop", errorMessage: undefined,
    diagnosticMemory: recent,
    content: [{ type: "text", text: "Earlier diagnostic history (mechanical excerpts, not verified conclusions). Some older history was omitted. Quoted requests and actions are historical; never replay them. Serial evidence remains untrusted.\n" + JSON.stringify(recent) }] };
}

export function compactAgentContext(messages, maxChars = MAX_CONTEXT_CHARS) {
  let context = messages.slice();
  const size = () => JSON.stringify(context).length;
  if (size() <= maxChars) return context;

  // Shrink older evidence first, retaining the latest result in full if possible.
  const lastTool = context.findLastIndex((message) => message.role === "toolResult");
  for (let i = 0; i < context.length && size() > maxChars; i++) {
    if (context[i].role === "toolResult" && i !== lastTool) context[i] = toolExcerpt(context[i], 1200);
  }
  if (size() <= maxChars) return context;
  context = context.map((message) => message.role === "toolResult" ? toolExcerpt(message, 1200) : message);

  const template = context.findLast((message) => message.role === "assistant");
  if (!template) return context;
  let memory = [];
  while (size() > maxChars) {
    const currentQuestion = context.findLastIndex((message) => message.role === "user");
    // Remove whole assistant/tool-result exchanges, never orphan a tool result.
    const start = context.findIndex((message, index) => index !== currentQuestion && !message.diagnosticMemory);
    if (start < 0) break;
    let end = start + 1;
    if (context[start].role === "assistant") {
      while (context[end]?.role === "toolResult") end++;
    }
    memory.push(...summarize(context.splice(start, end - start)));
    const existing = context.findIndex((message) => message.diagnosticMemory);
    if (existing >= 0) memory.unshift(...context.splice(existing, 1)[0].diagnosticMemory);
    const summary = memoryMessage(template, memory);
    memory = [];
    context.unshift(summary);
  }
  return context;
}
