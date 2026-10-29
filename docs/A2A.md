# A2A: the resident Linkr agent

The Android app can host a resident AI assistant and expose it to other agents
over the [Agent2Agent (A2A) protocol](https://a2a-protocol.org/). This is the
agent-to-agent half of the app's automation: an external agent delegates a task
instead of driving the serial console tool by tool. The MCP server covers the
tool half; both run on the same app, port and bearer token.

> Status: implemented and covered by host-JVM, JavaScript and browser tests.
> On a real phone (2026-10-29, Android 16): the agent card was served over LAN,
> `SendMessage` showed the one approval card, the resident task ran against a
> mock OpenAI-compatible endpoint and returned `TASK_STATE_COMPLETED` with an
> artifact; `GetTask`/`ListTasks` and the MCP `start_agent_task` path were also
> exercised. The target board was not connected during that run (it advertised
> a new BLE address that needs GPIO1 pairing), so the resident agent's device
> tools were not exercised on hardware. One background-approval attempt let the
> task complete but the blocking HTTP response was lost, so callers should use
> `returnImmediately:true` plus `GetTask` for background tasks.

## Prerequisites

1. The Linkr Bee app is running with **Allow AI clients on this network**
   enabled (MCP server). The A2A endpoint shares that listener and token.
2. The app has usable AI settings. A board-stored configuration is **not** used
   automatically: the user must first load it into the app's AI settings
   (AI → board configuration → **Load from board**). Without saved settings the
   agent answers `get_agent_status` with `available:false` and refuses tasks.
3. The phone owner is reachable for the first task approval unless the access
   mode is **fully authorized**. With **every-command** or **high-risk-only**
   mode the app asks once when a task starts.

## Discovery

| What | URL |
| --- | --- |
| Agent card | `http://<phone-ip>:8712/.well-known/agent-card.json` |
| JSON-RPC endpoint | `http://<phone-ip>:8712/a2a` |
| Auth | `Authorization: Bearer <the MCP token>` on `/a2a` |

The card is public (it carries no secret). It advertises a JSON-RPC interface,
bearer auth, no streaming and no push notifications. Clients that need the token
get it from the app's MCP settings card, exactly as for MCP.

## Supported methods

The endpoint implements the A2A v1.0 JSON-RPC binding:

| Method | Result |
| --- | --- |
| `SendMessage` | `{"task": Task}`; starts a resident task (after the one approval) |
| `GetTask` | `Task`; poll while the task runs |
| `ListTasks` | `{"tasks": [...], "nextPageToken": "", "pageSize": n, "totalSize": n}` |
| `CancelTask` | `Task`; stops a queued or running task |
| `GetExtendedAgentCard` | the same agent card |

`SendStreamingMessage` and `SubscribeToTask` return an explicit
`UNSUPPORTED_OPERATION` error because the native transport answers one POST
with one body and does not hold SSE connections. The card says
`capabilities.streaming:false`, so a compliant client polls `GetTask`.

`SendMessage` accepts one or more text parts. Other part kinds (files, URLs,
structured data) are refused rather than silently dropped: the target is a
serial console, not a document store. `configuration.returnImmediately` is
honoured; the default is blocking, but because a serial task can outlive any
HTTP timeout the server waits at most 30 seconds and then returns the working
task for polling.

### Example: start and poll

```sh
TOKEN=<64-hex token from the app>

curl -s -X POST http://192.168.1.42:8712/a2a \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":1,"method":"SendMessage","params":{"message":{"role":"ROLE_USER","parts":[{"text":"Check the target kernel version and report the uptime."}],"messageId":"msg-1","contextId":"bench-1"}}}'

curl -s -X POST http://192.168.1.42:8712/a2a \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":2,"method":"GetTask","params":{"id":"<task id>"}}'
```

A completed task carries the assistant's answer as an artifact:

```json
{"jsonrpc":"2.0","id":2,"result":{
  "id":"agent-…","contextId":"bench-1",
  "status":{"state":"TASK_STATE_COMPLETED"},
  "artifacts":[{"artifactId":"artifact-…","name":"Linkr Bee resident agent answer",
                "parts":[{"text":"Kernel 6.1.0 …"}]}]
}}
```

## Approval and task execution

- `start_agent_task` / `SendMessage` is the **one consent** for a task. In
  `confirm` mode (and in `guarded` mode, where starting the resident agent is
  high risk) the phone shows one approval card; denying it starts nothing.
  `auto` mode starts without a prompt; `read` mode refuses tasks.
- Inside the approved task, the resident assistant's serial writes and
  accessory changes run without further phone prompts. It still uses the app's
  own executors, evidence rules and one-write-at-a-time gate. A second external
  write tool call while a task is active is refused with a clear error.
- One task runs at a time; up to 8 wait in the queue. There is **no hard
  wall-clock limit by default**: a slow model or a target that takes minutes to
  boot must not be killed by a timer while it is making progress. Callers that
  want a deadline can set `metadata.timeoutMs` (up to 24 hours; 0 means none);
  `CancelTask` (MCP: `cancel_agent_task`) is the normal way to stop a long task.
  The assistant panel's Stop button cancels the active task and all queued tasks.
  The panel shows when resident work owns the executor and blocks local questions
  until it settles. Turning off the MCP server also cancels resident work.
- Each queued task is bound to the device/session approved at submission. A
  target switch or reconnect before execution fails the old task; submit a new
  task for the current target. A2A approval is revoked on server shutdown,
  token/access changes or native request cancellation, just like MCP.
- Sessions: `contextId` (A2A) or `sessionId` (MCP) continues the same
  conversation. Omit it for the default session; use `newSession` (MCP) or a new
  `contextId` for a fresh one. Board-stored instructions are read once when a
  session starts, like a new conversation in the app. Changing the target
  resets that session's history because the old transcript is not evidence
  about the new console.
- Tasks live in the app process. Sending the app to the background is supported
  by its foreground service; killing the process ends the server and loses
  running tasks. Persistent task history is not implemented yet.

## Security notes

- Anyone with the MCP token can start a resident task and spend the configured
  model's tokens. Treat the token like an API key; rotate it in the app when in
  doubt.
- The approval card shows only a short request preview. Review it before
  approving: the task's later writes do not ask again.
- Assistant output is model output, not device evidence. It never includes the
  API key or custom headers; it can still be wrong. Verify device changes with
  the MCP read tools or the serial log.
- The agent card is public by design. It contains the model provider/model? No:
  it contains only names, descriptions and the interface URL; the provider/model
  are visible through the authenticated `get_agent_status` tool.
- Board-stored AI configuration is plaintext on the board. This guide does not
  change that: the app only uses such a configuration after the user explicitly
  loads it locally.

## MCP equivalent

The same resident agent is available to MCP clients without A2A:

| MCP tool | Purpose |
| --- | --- |
| `get_agent_status` | configuration summary, busy state, queue, recent tasks |
| `start_agent_task` | approve and queue a task |
| `get_agent_task` | poll one task (optional `waitMs`) |
| `list_agent_tasks` | recent tasks, optionally for one session |
| `cancel_agent_task` | stop a task |

See the [MCP server guide](MCP.md) for access modes, approval notifications and
the device tools the resident agent can use.
