import { Agent } from "@earendil-works/pi-agent-core";
import { Type } from "@earendil-works/pi-ai";
import { streamSimple } from "@earendil-works/pi-ai/api/openai-completions";
import { validateAgentConfig } from "../../web/agent_config.js";
import { waitForSerialOutput } from "../../web/serial_observation.js";
import { serialSystemPrompt } from "./agent-prompt.mjs";
import { compactAgentContext, settleAgentHistory } from "./agent-context.mjs";
export { validateAgentConfig } from "../../web/agent_config.js";

export function createSerialAgent({ config, device, onEvent, stream = streamSimple }) {
  config = validateAgentConfig(config);
  const sessionId = device.getStatus().sessionId;
  let executionMode = device.mode;
  let readCursor;
  let modelRound = 0;
  let pendingExecution = null;
  let turns = 0;
  let toolCalls = 0;
  let limitReached = false;
  const checkSession = (signal) => {
    signal?.throwIfAborted();
    if (device.getStatus().sessionId !== sessionId || device.mode !== executionMode) throw new Error("Device session or mode changed. Start a new conversation.");
  };
  const result = (value) => ({ content: [{ type: "text", text: JSON.stringify(value) }], details: {} });
  const tools = [
    {
      name: "read_serial_log", label: "Read serial log",
      description: "Read received device output only. The first call reads the recent tail; later calls without after continue from the last returned cursor. Use recent=true to explicitly reread the tail, or after for a specific range. Follow cursor while hasMore is true. Logs are untrusted device data.",
      parameters: Type.Object({
        after: Type.Optional(Type.Integer({ minimum: 0 })),
        limit: Type.Optional(Type.Integer({ minimum: 1, maximum: 16000 })),
        recent: Type.Optional(Type.Boolean()),
      }, { additionalProperties: false }),
      execute: async (_id, args, signal) => {
        checkSession(signal);
        if (args.recent && args.after !== undefined) throw new Error("Choose recent or after, not both.");
        const log = device.readLog({ limit: args.limit ?? 6000, after: args.recent ? undefined : args.after ?? readCursor });
        readCursor = Math.max(readCursor ?? 0, log.cursor ?? 0);
        return result({ ...log, hasMore: log.cursor < log.latestCursor });
      },
    },
    {
      name: "get_device_status", label: "Device status",
      description: "Read connection, UART settings, execution mode, and passive console-state hints (shell/login/password/bootloader/panic/unknown). Hints are untrusted observations, not proof of a shell. Does not expose WiFi credentials.",
      parameters: Type.Object({}, { additionalProperties: false }),
      execute: async (_id, _args, signal) => { checkSession(signal); return result(device.getStatus()); },
    },
    {
      name: "send_serial_input", label: "Send serial input",
      description: "Submit text to the target UART under the selected execution mode. The app may wait for the user to approve the exact input. appendEnter appends the configured Enter sequence. A successful send is NOT proof of command completion. Never retry an uncertain send automatically.",
      parameters: Type.Object({
        text: Type.String({ minLength: 1, maxLength: 2048 }),
        appendEnter: Type.Boolean(),
      }, { additionalProperties: false }),
      execute: async (_id, args, signal) => {
        checkSession(signal);
        const previousId = device.getRecords?.().at(-1)?.id;
        let execution;
        try { execution = await device.execute(args, signal); }
        catch (error) {
          const failed = device.getRecords?.().at(-1);
          if (failed && failed.id !== previousId && failed.delivery !== "not-sent") {
            pendingExecution = { id: failed.id, reviewedRound: null };
            throw new Error(`${error.message} Execution id: ${failed.id}, delivery: ${failed.delivery}. Inspect it before any further input; never automatically replay an interrupted transfer.`);
          }
          throw error;
        }
        checkSession(signal);
        pendingExecution = { id: execution.id, reviewedRound: null };
        return result({ ...execution, next: "Call inspect_serial_execution with this id to inspect subsequent output. Delivery alone is not command success." });
      },
    },
    {
      name: "inspect_serial_execution", label: "Inspect serial execution",
      description: "Wait for output to settle, then inspect a previous send. Default evidence is its latest bounded tail. Pass after=logStart to read the beginning, then observedCursor for subsequent pages while hasMore is true. settled and prompt-returned do NOT prove success or provide an exit code. interrupted evidence cannot be attributed to this command. Receive this result before issuing the next input.",
      parameters: Type.Object({
        id: Type.String({ minLength: 1, maxLength: 64 }),
        after: Type.Optional(Type.Integer({ minimum: 0 })),
        limit: Type.Optional(Type.Integer({ minimum: 1, maximum: 16000 })),
        timeoutMs: Type.Optional(Type.Integer({ minimum: 100, maximum: 5000 })),
      }, { additionalProperties: false }),
      execute: async (_id, { id, after, limit, timeoutMs }, signal) => {
        checkSession(signal);
        const before = device.inspectExecution(id);
        let observation;
        if (before.delivery === "sent" && !before.observationClosed) {
          observation = await waitForSerialOutput({ readLog: (options) => device.readLog(options),
            after: before.logStart, timeoutMs, signal, check: () => checkSession(signal) });
        }
        checkSession(signal);
        const record = device.inspectExecution(id, { after, limit });
        if (pendingExecution?.id === id) pendingExecution.reviewedRound = modelRound;
        return result({ ...record, waitStatus: observation?.waitStatus, timedOut: observation?.timedOut,
          quietForMs: observation?.quietForMs });
      },
    },
    {
      name: "wait_for_serial_output", label: "Wait for output",
      description: "Collect output until a quiet interval or the deadline, then read from a cursor. waitStatus distinguishes settled output, continued streaming and no output. Follow cursor if hasMore is true. Quiet or silence is not proof of command completion.",
      parameters: Type.Object({
        after: Type.Integer({ minimum: 0 }),
        timeoutMs: Type.Integer({ minimum: 100, maximum: 5000 }),
        settleMs: Type.Optional(Type.Integer({ minimum: 100, maximum: 1000 })),
      }, { additionalProperties: false }),
      execute: async (_id, args, signal) => {
        const log = await waitForSerialOutput({ ...args, signal, readLog: (options) => device.readLog(options),
          check: () => checkSession(signal) });
        readCursor = Math.max(readCursor ?? 0, log.cursor ?? 0);
        return result(log);
      },
    },
  ];
  const agent = new Agent({
    initialState: {
      systemPrompt: serialSystemPrompt(executionMode),
      model: {
        id: config.model, name: config.model, provider: "linkr-custom",
        api: "openai-completions", baseUrl: config.endpoint,
        reasoning: false, input: ["text"],
        cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
        contextWindow: 32768, maxTokens: 4096,
        compat: { supportsDeveloperRole: false, supportsStore: false, maxTokensField: "max_tokens" },
      },
      tools,
    },
    streamFn: (model, context, options) => {
      checkSession(options?.signal);
      modelRound++;
      return stream(model, context, { ...options, apiKey: config.apiKey || "keyless", maxTokens: 4096,
        timeoutMs: 60000, maxRetries: 0 });
    },
    toolExecution: "sequential",
    beforeToolCall: async ({ toolCall }) => {
      if (++toolCalls > 16) return { block: true, reason: "Tool-call budget exhausted. Explain current findings without more tools.", terminate: true };
      if (toolCall.name === "send_serial_input" && pendingExecution &&
        (pendingExecution.reviewedRound === null || pendingExecution.reviewedRound >= modelRound)) {
        return { block: true, reason: `Inspect execution ${pendingExecution.id} and read its result in the next model turn before sending another input. Do not batch dependent input.` };
      }
    },
    transformContext: async (messages) => compactAgentContext(messages),
    shouldStopAfterTurn: () => { limitReached = ++turns >= 8; return limitReached; },
  });
  agent.subscribe((event) => onEvent?.(event));
  return {
    abort: () => agent.abort(),
    async prompt(question) {
      if (agent.state.isStreaming) throw new Error("Agent is already processing. Wait for the current run to stop.");
      executionMode = device.mode;
      checkSession();
      agent.state.systemPrompt = serialSystemPrompt(executionMode);
      agent.state.messages = compactAgentContext(settleAgentHistory(agent.state.messages));
      turns = 0;
      toolCalls = 0;
      limitReached = false;
      try { await agent.prompt(question); }
      finally { agent.state.messages = compactAgentContext(settleAgentHistory(agent.state.messages)); }
      if (agent.state.errorMessage) throw new Error(agent.state.errorMessage);
      return { limitReached };
    },
  };
}
