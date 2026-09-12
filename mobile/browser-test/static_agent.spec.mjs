import { test, expect } from "@playwright/test";

/* The documented desktop path serves web/ as plain static files (no bundler),
 * so the assistant comes from the vendored runtime bundle. These tests fail if
 * that path ever loses the assistant again, which is the regression that kept
 * desktop Chrome without it. */
const staticBase = "http://127.0.0.1:8766";

function sse(text) {
  const chunk = (delta, finish) => JSON.stringify({ id: "chatcmpl-static", object: "chat.completion.chunk",
    created: 1, model: "test-model", choices: [{ index: 0, delta, finish_reason: finish }] });
  return `data: ${chunk({ role: "assistant", content: text }, null)}\n\ndata: ${chunk({}, "stop")}\n\ndata: [DONE]\n\n`;
}

test("the static terminal loads the vendored assistant runtime", async ({ page }) => {
  await page.goto(`${staticBase}/`);
  await expect(page.locator("#agentButton")).toBeVisible();
  const runtime = await page.evaluate(async () => {
    const module = await import("/vendor/agent/agent-runtime.js");
    return { available: module.available, exportKind: typeof module.createSerialAgent };
  });
  expect(runtime).toEqual({ available: true, exportKind: "function" });
});

test("the static terminal answers a question end to end", async ({ page }) => {
  await page.goto(`${staticBase}/`);
  await expect(page.locator("#agentButton")).toBeVisible();
  await page.locator("#agentButton").click();
  await page.locator("#agentSettingsButton").click();
  await page.locator("#agentEndpoint").fill("https://agent.test/v1");
  await page.locator("#agentModel").fill("test-model");
  await page.locator("#agentApiKey").fill("test-device-key");
  await page.locator("#agentSettingsSave").click();
  await expect(page.locator("#agentSettingsStatus")).toContainText("saved");
  await page.locator("#drawerClose").click();

  await page.route("https://agent.test/v1/chat/completions", async (route) => {
    const headers = { "access-control-allow-origin": "*", "access-control-allow-headers": "*" };
    if (route.request().method() === "OPTIONS") {
      await route.fulfill({ status: 204, headers });
      return;
    }
    await route.fulfill({ status: 200, headers, contentType: "text/event-stream", body: sse("Static path answer.") });
  });
  await page.locator("#agentQuestion").fill("Is the assistant available here?");
  await page.locator("#agentAsk").click();
  await expect(page.locator("#agentMessages")).toContainText("Static path answer.");
});

test("an unreachable endpoint explains the browser-side conditions", async ({ page }) => {
  await page.goto(`${staticBase}/`);
  await expect(page.locator("#agentButton")).toBeVisible();
  await page.locator("#agentButton").click();
  await page.locator("#agentSettingsButton").click();
  await page.locator("#agentEndpoint").fill("https://agent.test/v1");
  await page.locator("#agentModel").fill("test-model");
  await page.locator("#agentApiKey").fill("test-device-key");
  await page.locator("#agentSettingsSave").click();
  await expect(page.locator("#agentSettingsStatus")).toContainText("saved");
  await page.locator("#drawerClose").click();

  // What a blocked cross-origin request looks like before any HTTP response.
  await page.route("https://agent.test/v1/chat/completions", (route) => route.abort("failed"));
  await page.locator("#agentQuestion").fill("Is the assistant available here?");
  await page.locator("#agentAsk").click();
  await expect(page.locator("#agentMessages")).toContainText(/did not answer a browser request|没有响应浏览器请求/);
});
