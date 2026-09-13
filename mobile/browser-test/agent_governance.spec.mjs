import { test, expect } from "@playwright/test";

/* Governance for unattended execution: Full Auto carries a visible deadline,
 * and the user can pin commands that must always be confirmed on this device. */
test.beforeEach(async ({ page }) => {
  await page.route("**/app.js?*", async (route) => {
    const response = await route.fetch();
    await route.fulfill({ response, body: await response.text() +
      "\nwindow.__test = { state, setConnected, handleIncomingBytes };" });
  });
  await page.goto("/");
  await page.waitForFunction(() => window.__test?.state.term);
  await page.evaluate(() => {
    const { state, setConnected, handleIncomingBytes } = window.__test;
    window.sent = [];
    state.mode = "ws";
    // A host gives the session a device identity; per-device policy and notes
    // are keyed by it, and without one the panel refuses to store them.
    state.wsHost = "test-board";
    /* The injected shell prompt schedules the app's own window-size sync; it is
     * not command input, so keep it out of the command accounting. */
    state.ws = { readyState: 1, send: (bytes) => {
      const text = new TextDecoder().decode(bytes);
      if (text.startsWith("stty ")) return;
      window.sent.push(text);
    } };
    setConnected(true);
    handleIncomingBytes(new TextEncoder().encode("root@board:~# "));
  });
  await page.locator("#agentButton").click();
});

function sse({ text, tool }) {
  const chunk = (delta, finish) => JSON.stringify({ id: "chatcmpl-test", object: "chat.completion.chunk",
    created: 1, model: "test-model", choices: [{ index: 0, delta, finish_reason: finish }] });
  const delta = tool ? { role: "assistant", tool_calls: [{ index: 0, id: "call_1", type: "function",
    function: { name: tool.name, arguments: JSON.stringify(tool.args) } }] }
    : { role: "assistant", content: text };
  return `data: ${chunk(delta, null)}\n\ndata: ${chunk({}, tool ? "tool_calls" : "stop")}\n\ndata: [DONE]\n\n`;
}

async function mockModel(page, respond) {
  await page.route("https://agent.test/v1/chat/completions", async (route) => {
    const headers = { "access-control-allow-origin": "*", "access-control-allow-headers": "*" };
    if (route.request().method() === "OPTIONS") { await route.fulfill({ status: 204, headers }); return; }
    await route.fulfill({ status: 200, headers, contentType: "text/event-stream", body: sse(respond()) });
  });
}

async function configure(page) {
  await page.locator("#agentSettingsButton").click();
  await page.locator("#agentEndpoint").fill("https://agent.test/v1");
  await page.locator("#agentModel").fill("test-model");
  await page.locator("#agentApiKey").fill("test-device-key");
  await page.locator("#agentSettingsSave").click();
  await expect(page.locator("#agentSettingsStatus")).toContainText("saved");
  await page.locator("#drawerClose").click();
}

async function chooseMode(page, mode) {
  await page.locator("#agentModeButton").tap();
  await page.locator(`[name="agentMode"][value="${mode}"]`).locator("..").tap();
  await expect(page.locator("#agentModePicker")).toBeHidden();
}

test("Full Auto shows its remaining window and re-selecting it extends it", async ({ page }) => {
  await chooseMode(page, "full-auto");
  const mode = page.locator("#agentActiveMode");
  const countdown = page.locator("#agentModeCountdown");
  // The mode keeps its exact label; the deadline lives beside it.
  await expect(mode).toHaveText("Full Auto");
  await expect(countdown).toHaveText(/· \d+:\d\d/);
  const first = await page.evaluate(() => window.__test ? Date.now() : 0);
  const remaining = async () => {
    const label = await countdown.textContent();
    const [minutes, seconds] = label.split("·")[1].trim().split(":").map(Number);
    return minutes * 60 + seconds;
  };
  const before = await remaining();
  expect(before).toBeGreaterThan(800);
  expect(before).toBeLessThanOrEqual(900);
  await page.waitForTimeout(1200);
  expect(await remaining()).toBeLessThan(before);
  await chooseMode(page, "full-auto");
  expect(await remaining()).toBeGreaterThan(before - 1);
  await chooseMode(page, "auto");
  await expect(mode).toHaveText("Auto");
  await expect(countdown).toHaveText("");
  expect(first).toBeGreaterThan(0);
});

test("an always-ask entry makes Full Auto request approval for that command", async ({ page }) => {
  await configure(page);
  // Pin "reboot" for this device. Identity falls back to transport+device+UART
  // when no target is bound, which is what the panel stores it under.
  await page.locator("#agentHistory > summary").click();
  await page.locator("#agentPolicy > summary").click();
  await page.locator("#agentPolicyAsk").fill("reboot");
  await page.locator("#agentPolicySave").click();
  await expect(page.locator("#agentPolicyStatus")).toContainText(/Saved on this device|已保存到本机/);
  await page.locator("#agentPolicy > summary").click();
  await page.locator("#agentHistory > summary").click();

  await chooseMode(page, "full-auto");
  await mockModel(page, () => ({ tool: { name: "send_serial_input", args: { text: "reboot", appendEnter: true } } }));
  await page.locator("#agentQuestion").fill("Reboot the target");
  await page.locator("#agentAsk").click();
  const approve = page.locator("#agentMessages .agent-actions .btn-primary");
  await expect(approve).toBeVisible();
  expect(await page.evaluate(() => window.sent)).toEqual([]);
  await approve.click();
  await expect.poll(() => page.evaluate(() => window.sent.length)).toBe(1);
  expect(await page.evaluate(() => window.sent[0])).toBe("reboot\r");
});

test("a pre-approved command is sent unattended in Auto mode", async ({ page }) => {
  await configure(page);
  await page.locator("#agentHistory > summary").click();
  await page.locator("#agentPolicy > summary").click();
  await page.locator("#agentPolicyAllow").fill("systemctl status nginx");
  await page.locator("#agentPolicySave").click();
  await expect(page.locator("#agentPolicyStatus")).toContainText(/Saved on this device|已保存到本机/);
  await page.locator("#agentPolicy > summary").click();
  await page.locator("#agentHistory > summary").click();

  await mockModel(page, () => ({ tool: { name: "send_serial_input", args: { text: "systemctl status nginx", appendEnter: true } } }));
  await page.locator("#agentQuestion").fill("Check nginx");
  await page.locator("#agentAsk").click();
  await expect.poll(() => page.evaluate(() => window.sent.length)).toBe(1);
  expect(await page.evaluate(() => window.sent[0])).toBe("systemctl status nginx\r");
  await expect(page.locator("#agentMessages .agent-actions .btn-primary")).toHaveCount(0);
});
