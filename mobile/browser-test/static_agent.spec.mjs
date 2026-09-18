import { test, expect } from "@playwright/test";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";

/* The documented desktop path serves web/ as plain static files (no bundler),
 * so the assistant comes from the vendored runtime bundle. These tests fail if
 * that path ever loses the assistant again, which is the regression that kept
 * desktop Chrome without it. */
const staticBase = "http://127.0.0.1:8766";

/* This spec owns its static server rather than leaving it to a second
 * `webServer` entry in playwright.config.mjs.
 *
 * Playwright's own spawn of that entry did not come up here: the three tests
 * below failed with `net::ERR_CONNECTION_REFUSED at http://127.0.0.1:8766/`
 * while every other spec passed, and the same three passed as soon as the
 * server was started outside Playwright and the config reused it. Starting it
 * in the runner's own process tree is what the passing case did, so that is
 * what the spec does now.
 *
 * It stays loud rather than flaky: a server that cannot be reached inside the
 * timeout fails the file with the reason, and a server this machine already
 * serves on that port is reused so a developer running one by hand does not
 * collide with the suite. */
const serverScript = fileURLToPath(new URL("./static-server.mjs", import.meta.url));
const webRoot = fileURLToPath(new URL("../../web", import.meta.url));
const readyTimeoutMs = 15000;

async function reachable() {
  try {
    const response = await fetch(`${staticBase}/`, { signal: AbortSignal.timeout(1000) });
    return response.ok;
  } catch { return false; }
}

let server = null;

test.beforeAll(async () => {
  if (await reachable()) return;
  server = spawn(process.execPath, [serverScript, "8766", webRoot], { stdio: "ignore" });
  const deadline = Date.now() + readyTimeoutMs;
  let exitCode = null;
  while (Date.now() < deadline) {
    if (await reachable()) return;
    if (server.exitCode !== null) { exitCode = server.exitCode; break; }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  server.kill();
  server = null;
  throw new Error(`The plain-hosting server never answered on ${staticBase} (exit code ${exitCode ?? "still running"}). Start it by hand with: node browser-test/static-server.mjs 8766 ../web`);
});

test.afterAll(() => { server?.kill(); });

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
