import { test, expect } from "@playwright/test";
import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";
import { join } from "node:path";

/* The documented desktop path serves web/ as plain static files (no bundler),
 * so the assistant comes from the vendored runtime bundle. These tests fail if
 * that path ever loses the assistant again, which is the regression that kept
 * desktop Chrome without it. */
const staticBase = "http://127.0.0.1:8766";
/* The GitHub Pages variant is web/ with the LAN bridge switched off (see
 * tools/build_pages.sh). It is built here and served on its own port, so the
 * switch is exercised in a browser rather than trusted from the diff. */
const pagesBase = "http://127.0.0.1:8767";
const readyTimeoutMs = 15000;

/* This spec owns its static servers rather than leaving them to webServer
 * entries in playwright.config.mjs.
 *
 * Playwright's own spawn of such an entry did not come up here: three tests
 * failed with `net::ERR_CONNECTION_REFUSED at http://127.0.0.1:8766/` while
 * every other spec passed, and the same tests passed as soon as the server was
 * started outside Playwright and the config reused it. Starting them in the
 * runner's own process tree is what the passing case did, so that is what the
 * spec does now.
 *
 * They stay loud rather than flaky: a server that cannot be reached inside the
 * timeout fails the file with the child's exit code, and a server this machine
 * already serves on that port is reused so a developer running one by hand does
 * not collide with the suite. */
const serverScript = fileURLToPath(new URL("./static-server.mjs", import.meta.url));
const repoDir = fileURLToPath(new URL("../..", import.meta.url));
const webRoot = fileURLToPath(new URL("../../web", import.meta.url));
const pagesDir = mkdtempSync(join(tmpdir(), "linkr-pages-build-"));

async function reachable(base) {
  try {
    const response = await fetch(`${base}/`, { signal: AbortSignal.timeout(1000) });
    return response.ok;
  } catch { return false; }
}

async function ensureServer(base, port, root) {
  if (await reachable(base)) return null;
  const server = spawn(process.execPath, [serverScript, port, root], { stdio: "ignore" });
  const deadline = Date.now() + readyTimeoutMs;
  let exitCode = null;
  while (Date.now() < deadline) {
    if (await reachable(base)) return server;
    if (server.exitCode !== null) { exitCode = server.exitCode; break; }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  server.kill();
  throw new Error(`The plain-hosting server never answered on ${base} (exit code ${exitCode ?? "still running"}). Start it by hand with: node browser-test/static-server.mjs ${port} ${root}`);
}

let servers = [];

/* This file owns its servers for the whole file, so its tests must run in one
 * worker. Playwright brackets a file's beforeAll/afterAll within a single
 * worker, so a second worker that finds the port already answered reuses the
 * first worker's server and does not own it -- and then the first worker
 * finishes and kills it under the second one. That is how the last test in this
 * file kept failing with connection refused, in CI as well as locally, and it
 * is why the fix is to stop splitting the file rather than to add a retry. */
test.describe.configure({ mode: "serial" });

test.beforeAll(async () => {
  const build = spawnSync("/bin/sh", [join(repoDir, "tools", "build_pages.sh"), pagesDir], { encoding: "utf8" });
  assert.equal(build.status, 0, `tools/build_pages.sh failed: ${build.stderr}`);
  servers = [
    await ensureServer(staticBase, "8766", webRoot),
    await ensureServer(pagesBase, "8767", pagesDir),
  ].filter(Boolean);
});

test.afterAll(() => { for (const server of servers) server?.kill(); });

function sse(text) {
  const chunk = (delta, finish) => JSON.stringify({ id: "chatcmpl-static", object: "chat.completion.chunk",
    created: 1, model: "test-model", choices: [{ index: 0, delta, finish_reason: finish }] });
  return `data: ${chunk({ role: "assistant", content: text }, null)}\n\ndata: ${chunk({}, "stop")}\n\ndata: [DONE]\n\n`;
}

test("the static terminal loads the vendored assistant runtime", async ({ page }) => {
  await page.goto(`${staticBase}/`);
  await expect(page.locator("#agentButton")).toBeVisible();
  // The locally served variant keeps the LAN bridge: this is the side of the
  // switch that must not regress while the Pages side hides it.
  await expect(page.locator("#lanModeBtn")).toBeVisible();
  const runtime = await page.evaluate(async () => {
    const module = await import("/vendor/agent/agent-runtime.js");
    return { available: module.available, exportKind: typeof module.createSerialAgent };
  });
  expect(runtime).toEqual({ available: true, exportKind: "function" });
});

test("the Pages variant hides the LAN bridge and still boots the assistant", async ({ page }) => {
  await page.goto(`${pagesBase}/`);
  await expect(page.locator("#agentButton")).toBeVisible();
  await expect(page.locator("#lanModeBtn")).toBeHidden();
  await expect(page.locator("#wsHostField")).toBeHidden();
  await expect(page.locator("#bleModeBtn")).toBeVisible();
  // The switch hides a transport, not the assistant: the vendored runtime has
  // to load on this variant exactly as it does on the locally served one.
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
