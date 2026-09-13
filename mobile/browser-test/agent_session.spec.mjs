import { test, expect } from "@playwright/test";

/* A diagnostic conversation should survive a reload, and the restored history
 * must come back marked as unverified rather than as evidence. */
test.beforeEach(async ({ page }) => {
  await page.route("**/app.js?*", async (route) => {
    const response = await route.fetch();
    await route.fulfill({ response, body: await response.text() +
      "\nwindow.__test = { state, setConnected, handleIncomingBytes };" });
  });
  await page.goto("/");
  await connect(page);
  await page.locator("#agentButton").click();
  await page.locator("#agentSettingsButton").click();
  await page.locator("#agentEndpoint").fill("https://agent.test/v1");
  await page.locator("#agentModel").fill("test-model");
  await page.locator("#agentApiKey").fill("test-device-key");
  await page.locator("#agentSettingsSave").click();
  await expect(page.locator("#agentSettingsStatus")).toContainText("saved");
  await page.locator("#drawerClose").click();
});

/* Establishes the device identity the way a reconnect does: the app takes it
 * from the live session, not from storage, so a reload alone has no device. */
async function connect(page) {
  await page.waitForFunction(() => window.__test?.state.term);
  await page.evaluate(() => {
    const { state, setConnected, handleIncomingBytes } = window.__test;
    state.mode = "ws";
    state.wsHost = "test-board";
    state.ws = { readyState: 1, send: () => {} };
    setConnected(true);
    handleIncomingBytes(new TextEncoder().encode("root@board:~# "));
  });
}

function sse(text) {
  const chunk = (delta, finish) => JSON.stringify({ id: "chatcmpl-test", object: "chat.completion.chunk",
    created: 1, model: "test-model", choices: [{ index: 0, delta, finish_reason: finish }] });
  return `data: ${chunk({ role: "assistant", content: text }, null)}\n\ndata: ${chunk({}, "stop")}\n\ndata: [DONE]\n\n`;
}

async function mockModel(page, bodies) {
  await page.route("https://agent.test/v1/chat/completions", async (route) => {
    const headers = { "access-control-allow-origin": "*", "access-control-allow-headers": "*" };
    if (route.request().method() === "OPTIONS") { await route.fulfill({ status: 204, headers }); return; }
    bodies.push(route.request().postDataJSON());
    await route.fulfill({ status: 200, headers, contentType: "text/event-stream", body: sse("The root filesystem could not mount.") });
  });
}

async function ask(page, question) {
  await page.locator("#agentQuestion").fill(question);
  await page.locator("#agentAsk").click();
  await expect(page.locator("#agentMessages")).toContainText("could not mount");
}

test("a reload restores the conversation and marks the history unverified", async ({ page }) => {
  const bodies = [];
  await mockModel(page, bodies);
  await ask(page, "Why did boot fail?");
  // The panel saves on a short debounce.
  await page.waitForTimeout(700);

  await page.reload();
  await connect(page);
  await page.locator("#agentButton").click();
  const messages = page.locator("#agentMessages");
  await expect(messages).toContainText("Why did boot fail?");
  await expect(messages).toContainText("could not mount");
  await expect(messages).toContainText(/unverified|未经核实/);

  // The model receives the earlier turns, not just the on-screen copy.
  await ask(page, "And what should I check first?");
  const resent = bodies.at(-1).messages;
  expect(JSON.stringify(resent)).toContain("Why did boot fail?");
});

test("a new conversation clears the stored history", async ({ page }) => {
  const bodies = [];
  await mockModel(page, bodies);
  await ask(page, "Why did boot fail?");
  await page.waitForTimeout(700);
  expect(await page.evaluate(() => JSON.stringify(localStorage.getItem("linkr-agent-session-v1") || ""))).toContain("Why did boot fail");

  await page.locator("#agentNew").click();
  await expect(page.locator("#agentMessages")).not.toContainText("Why did boot fail?");
  await page.reload();
  await connect(page);
  await page.locator("#agentButton").click();
  await expect(page.locator("#agentMessages")).not.toContainText("Why did boot fail?");
});
