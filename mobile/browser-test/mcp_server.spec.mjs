import { test, expect } from "@playwright/test";

/* The Android shell installs window.LinkrMcp before app.js runs. In a browser
 * there is no native bridge, so the spec injects a recording fake that follows
 * the same contract and answers exactly like the Java transport would. */
const FAKE_BRIDGE = () => {
  const state = {
    running: false, port: 8712, bind: "lan",
    token: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
    url: "http://192.168.1.20:8712/mcp", urls: ["http://127.0.0.1:8712/mcp", "http://192.168.1.20:8712/mcp"],
    foreground: true, activeRequests: 0, lastError: null,
  };
  const listeners = { httpRequest: [], stateChanged: [] };
  window.__mcp = { calls: [], state };
  window.LinkrMcp = {
    available: true,
    platform: "android",
    addListener(event, listener) {
      listeners[event]?.push(listener);
      return Promise.resolve({ remove: async () => {
        const list = listeners[event] || [];
        const at = list.indexOf(listener);
        if (at >= 0) list.splice(at, 1);
      } });
    },
    async start(config) {
      window.__mcp.calls.push({ method: "start", config });
      Object.assign(state, { running: true, port: config.port, bind: config.bind });
      for (const listener of listeners.stateChanged) listener({ ...state });
      return { ...state };
    },
    async stop() {
      window.__mcp.calls.push({ method: "stop" });
      Object.assign(state, { running: false });
      for (const listener of listeners.stateChanged) listener({ ...state });
      return { ...state };
    },
    async getState() { return { ...state }; },
    async regenerateToken() { state.token = "f".repeat(64); return { ...state }; },
    async respond(payload) { window.__mcp.calls.push({ method: "respond", payload }); },
    __emit(payload) { for (const listener of listeners.httpRequest) listener(payload); },
  };
};

const initialize = { jsonrpc: "2.0", id: 1, method: "initialize", params: { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "spec", version: "1" } } };
const toolsList = { jsonrpc: "2.0", id: 2, method: "tools/list" };

async function openMcpSettings(page) {
  await page.locator("#panelToggle").tap();
  await page.locator('[data-settings-target="ai"]').tap();
  await expect(page.locator("#controlsPanel #mcpSettings")).toBeVisible();
}

async function emitRpc(page, message) {
  const call = await page.evaluate(async (message) => {
    const before = window.__mcp.calls.length;
    window.LinkrMcp.__emit({ id: "req-1", method: "POST", path: "/mcp", remote: "127.0.0.1", userAgent: "spec", body: JSON.stringify(message) });
    // handleHttp -> bridge.respond is async; wait for the recorded call.
    for (let i = 0; i < 200 && window.__mcp.calls.length === before; i += 1) await new Promise((r) => setTimeout(r, 10));
    return window.__mcp.calls.at(-1);
  }, message);
  return call;
}

test.beforeEach(async ({ page }) => {
  await page.addInitScript(FAKE_BRIDGE);
  await page.goto("/");
  await expect(page.locator("#agentButton")).toBeVisible();
});

test("MCP settings card renders, enables the server, and exposes endpoint and token", async ({ page }) => {
  await openMcpSettings(page);
  await expect(page.locator("#mcpSettingsTitle")).toHaveText("MCP server");
  await expect(page.locator("#mcpEnabled")).not.toBeChecked();
  await expect(page.locator("#mcpEndpointBlock")).toBeHidden();
  await page.locator("#mcpEnabled").tap();
  await expect(page.locator("#mcpUrl")).toHaveValue("http://192.168.1.20:8712/mcp");
  expect(await page.locator("#mcpToken").inputValue()).toMatch(/^[0-9a-f]{64}$/);
  await expect(page.locator("#mcpToken")).toHaveAttribute("type", "password");
  await expect(page.locator("#mcpStatus")).toContainText("Running");
  const calls = await page.evaluate(() => window.__mcp.calls);
  expect(calls.some((call) => call.method === "start" && call.config.port === 8712 && call.config.bind === "lan")).toBe(true);
  const snippet = await page.locator("#mcpSnippet").textContent();
  expect(snippet).toContain('"type": "http"');
  expect(snippet).toContain("/mcp");
  expect(snippet).toContain("Bearer 0123456789abcdef");
});

test("the forwarded MCP transport answers initialize and tools/list through the native bridge", async ({ page }) => {
  await openMcpSettings(page);
  await page.locator("#mcpEnabled").tap();

  const init = await emitRpc(page, initialize);
  expect(init.method).toBe("respond");
  const initBody = JSON.parse(init.payload.body);
  expect(initBody.result.protocolVersion).toBe("2025-06-18");
  expect(initBody.result.serverInfo.name).toBe("linkr-bee");
  expect(init.payload.status).toBe(200);

  const list = await emitRpc(page, toolsList);
  const names = JSON.parse(list.payload.body).result.tools.map((tool) => tool.name);
  expect(names).toHaveLength(21);
  expect(names).toContain("send_serial_input");
  expect(names).toContain("read_serial_log");
});

test("read-only mode refuses a write tool with an isError result and never asks for approval", async ({ page }) => {
  await openMcpSettings(page);
  await page.locator("#mcpAccess").selectOption("read");
  await page.locator("#mcpEnabled").tap();
  const call = await emitRpc(page, { jsonrpc: "2.0", id: 3, method: "tools/call", params: { name: "send_serial_input", arguments: { text: "ls", appendEnter: true } } });
  const result = JSON.parse(call.payload.body).result;
  expect(result.isError).toBe(true);
  expect(result.content[0].text).toContain("read-only");
  await expect(page.locator("#mcpApproval")).toBeHidden();
});

test("a disconnected target makes read tools return an explicit error instead of a false success", async ({ page }) => {
  await openMcpSettings(page);
  await page.locator("#mcpEnabled").tap();
  const call = await emitRpc(page, { jsonrpc: "2.0", id: 4, method: "tools/call", params: { name: "get_device_status", arguments: {} } });
  const result = JSON.parse(call.payload.body).result;
  expect(result.isError).toBe(true);
  expect(result.content[0].text).toContain("not connected");
});

test("the approval banner masks secrets and resolves allow and deny without executing", async ({ page }) => {
  await page.goto("/");
  await page.evaluate(async () => {
    // The app already created its own MCP card from the injected native bridge;
    // remove it so this test drives an isolated UI instance.
    for (const node of document.querySelectorAll("#mcpApproval, #mcpSettings")) node.remove();
    const { createMcpUi } = await import("/mcp_ui.js");
    const config = { enabled: true, port: 8712, bind: "lan", access: "confirm" };
    const state = { running: false, url: "", urls: [], token: "", port: 8712, bind: "lan", access: "confirm", foreground: true, activeRequests: 0, lastError: null, audit: [] };
    const host = {
      getConfig: () => ({ ...config }), getState: () => ({ ...state }),
      refreshState: async () => ({ ...state }), getAudit: () => [],
      setConfig: async () => ({}), enable: async () => ({}), disable: async () => ({}),
    };
    window.__ui = createMcpUi({ mount: document.querySelector("#controlsPanel") || document.body, getLang: () => "en", host });
    window.__decisions = [];
    window.__request = (args) => {
      window.__ui.requestApproval({ tool: "set_wifi", args, timeoutMs: 5000 })
        .then((decision) => window.__decisions.push(decision));
    };
    window.__request({ action: "connect", ssid: "lab", password: "s3cret" });
  });
  await expect(page.locator("#mcpApproval")).toBeVisible();
  await expect(page.locator("#mcpApproval .mcp-approval-args")).toContainText("lab");
  await expect(page.locator("#mcpApproval .mcp-approval-args")).not.toContainText("s3cret");
  await page.locator('#mcpApproval [data-mcp="approvalAllow"]').tap();
  await expect.poll(async () => page.evaluate(() => window.__decisions[0]?.approved)).toBe(true);
  await expect(page.locator("#mcpApproval")).toBeHidden();

  await page.evaluate(() => window.__request({ action: "off" }));
  await expect(page.locator("#mcpApproval")).toBeVisible();
  await page.locator('#mcpApproval [data-mcp="approvalDeny"]').tap();
  await expect.poll(async () => page.evaluate(() => window.__decisions[1]?.approved)).toBe(false);
  await expect(page.locator("#mcpApproval")).toBeHidden();
});
