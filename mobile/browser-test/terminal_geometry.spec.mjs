import { test, expect } from "@playwright/test";

const prompt = "root@target:~# ";

test.beforeEach(async ({ page }) => {
  await page.setViewportSize({ width: 1200, height: 800 });
  await page.route("**/app.js?*", async (route) => {
    const response = await route.fetch();
    await route.fulfill({ response, body: await response.text() +
      "\nwindow.__geometry = { state, setConnected, handleIncomingBytes, onTerminalData };" });
  });
  await page.goto("/");
  await page.waitForFunction(() => window.__geometry?.state.term?.buffer.active.cursorY > 0);
  await page.evaluate(async () => {
    const { state, setConnected } = window.__geometry;
    window.sent = [];
    state.mode = "ws";
    state.ws = { readyState: 1, send: bytes => window.sent.push(new TextDecoder().decode(bytes)) };
    setConnected(true);
    await new Promise(resolve => state.term.write("\x1bc", resolve));
  });
});

async function receive(page, text) {
  await page.evaluate(text => window.__geometry.handleIncomingBytes(new TextEncoder().encode(text)), text);
  // Cover xterm's asynchronous parser and the 180 ms debounce. The margin is
  // generous because a loaded runner can delay both, and the negative
  // assertions below rely on this wait being long enough to prove absence.
  await page.waitForTimeout(600);
}

async function syncCommands(page) {
  return page.evaluate(() => window.sent.filter(text => text.startsWith("stty ")));
}

/* A sync that is expected to happen is polled: the debounce plus xterm's parser
 * can take longer than the fixed wait above when the suite runs in parallel. */
async function expectSyncCount(page, count) {
  await expect.poll(() => syncCommands(page)).toHaveLength(count);
}

for (const viewport of [{ width: 1552, height: 1221 }, { width: 390, height: 844 }]) {
  test.describe(`terminal viewport ${viewport.width}`, () => {
  test.use({ isMobile: viewport.width < 600, hasTouch: viewport.width < 600 });
  test(`last terminal row fits above keyboard with Agent at ${viewport.width}px`, async ({ page }, testInfo) => {
    await page.setViewportSize(viewport);
    await page.locator("#agentButton").click();
    await receive(page, ("test output 0123456789\r\n").repeat(150) + "LAST ROW");
    const measure = () => page.evaluate(() => {
      const screen = document.querySelector(".xterm-screen").getBoundingClientRect();
      const output = document.querySelector("#terminalOutput").getBoundingClientRect();
      const keyboard = document.querySelector(".terminal-input-bar").getBoundingClientRect();
      return { bottom: screen.bottom, right: screen.right, outputBottom: output.bottom,
        outputRight: output.right, keyboardTop: keyboard.top };
    });
    await expect.poll(async () => {
      const r = await measure();
      return r.bottom <= Math.min(r.outputBottom, r.keyboardTop) && r.right <= r.outputRight;
    }).toBe(true);
    await page.screenshot({ path: testInfo.outputPath(`terminal-${viewport.width}.png`) });
    await page.evaluate(() => document.documentElement.classList.add("soft-keyboard-open"));
    await expect.poll(async () => {
      const r = await measure();
      return r.bottom <= Math.min(r.outputBottom, r.keyboardTop) && r.right <= r.outputRight;
    }).toBe(true);
  });
  });
}

test("geometry synchronizes once at an empty prompt", async ({ page }) => {
  await receive(page, prompt);
  await expectSyncCount(page, 1);
  await receive(page, `stty\r\n${prompt}`);
  expect(await syncCommands(page)).toHaveLength(1);
});

for (const locallyTyped of [true, false]) {
  test(`Ctrl-A preserves the editable command (local input: ${locallyTyped})`, async ({ page }) => {
    if (locallyTyped) await page.evaluate(() => window.__geometry.onTerminalData("echo KEEP"));
    await receive(page, prompt + "echo KEEP");
    if (locallyTyped) await page.evaluate(() => window.__geometry.onTerminalData("\x01"));
    await receive(page, `\r\x1b[${prompt.length}C`);
    expect(await syncCommands(page)).toEqual([]);
    // Cancelling the input and receiving a fresh prompt permits synchronization.
    await page.evaluate(() => window.__geometry.onTerminalData("\x03"));
    await receive(page, `^C\r\n${prompt}`);
    await expectSyncCount(page, 1);
  });
}

test("hidden pending input and alternate-screen prompts never trigger geometry writes", async ({ page }) => {
  await page.evaluate(() => window.__geometry.onTerminalData("echo hidden"));
  await receive(page, prompt);
  expect(await syncCommands(page)).toEqual([]);
  await page.evaluate(() => window.__geometry.onTerminalData("\x03"));
  await receive(page, `\x1b[?1049h${prompt}`);
  expect(await syncCommands(page)).toEqual([]);
});

for (const coalesced of [false, true]) {
  test(`same-size login resynchronizes without a BLE reconnect (coalesced: ${coalesced})`, async ({ page }) => {
    await receive(page, prompt);
    await expectSyncCount(page, 1);
    if (coalesced) {
      await receive(page, `\r\ntarget login: root\r\n${prompt}`);
    } else {
      await receive(page, "\r\ntarget log");
      await receive(page, "in: ");
      expect(await syncCommands(page)).toHaveLength(1);
      await receive(page, `root\r\n${prompt}`);
    }
    await expectSyncCount(page, 2);
  });
}
