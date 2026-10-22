import { test, expect } from "@playwright/test";

async function openTerminal(page, { android = false } = {}) {
  if (android) {
    await page.addInitScript(() => {
      // The Capacitor bootstrap only installs LinkrNativeBle on a native
      // platform; this is the same surface app.js reads for layout decisions.
      window.LinkrNativeBle = { platform: "android" };
    });
  }
  await page.route("**/app.js?*", async (route) => {
    const response = await route.fetch();
    await route.fulfill({
      response,
      body: (await response.text()) +
        "\nwindow.__terminalTest = { state, setConnected, handleIncomingBytes };" +
        "\nwindow.__terminalCopied = null;" +
        "\nObject.defineProperty(navigator.clipboard, 'writeText', {" +
        "\n  configurable: true," +
        "\n  value: async (text) => { window.__terminalCopied = text; }," +
        "\n});",
    });
  });
  await page.goto("/");
  await page.waitForFunction(() => window.__terminalTest?.state.term);
  await page.evaluate(async () => {
    const { state, setConnected } = window.__terminalTest;
    window.sent = [];
    state.mode = "ws";
    state.ws = {
      readyState: 1,
      send: (bytes) => window.sent.push(new TextDecoder().decode(bytes)),
    };
    setConnected(true);
    await new Promise((resolve) => state.term.write(
      Array.from({ length: 120 }, (_, i) =>
        `line ${String(i).padStart(3, "0")} abcdefghijklmnopqrstuvwxyz\r\n`).join(""),
      resolve,
    ));
    state.term.blur();
  });
}

async function touchDrag(context, page, { startY, deltaY }) {
  const box = await page.locator(".xterm-screen").boundingBox();
  const x = box.x + box.width / 2;
  const y = startY ?? box.y + 40;
  const client = await context.newCDPSession(page);
  await client.send("Input.dispatchTouchEvent", {
    type: "touchStart",
    touchPoints: [{ x, y, radiusX: 8, radiusY: 8, force: 1, id: 1 }],
  });
  for (let i = 1; i <= 10; i += 1) {
    await client.send("Input.dispatchTouchEvent", {
      type: "touchMove",
      touchPoints: [{
        x,
        y: y + (deltaY * i) / 10,
        radiusX: 8,
        radiusY: 8,
        force: 1,
        id: 1,
      }],
    });
  }
  await client.send("Input.dispatchTouchEvent", {
    type: "touchEnd",
    touchPoints: [],
  });
  await page.waitForTimeout(200);
}

test("touch drag scrolls the xterm history and turns auto-scroll off", async ({ page, context }) => {
  await openTerminal(page);
  const before = await page.evaluate(() => {
    const buffer = window.__terminalTest.state.term.buffer.active;
    return { viewportY: buffer.viewportY, baseY: buffer.baseY, auto: window.__terminalTest.state.autoScroll };
  });
  await touchDrag(context, page, { deltaY: 240 });
  const after = await page.evaluate(() => {
    const buffer = window.__terminalTest.state.term.buffer.active;
    return {
      viewportY: buffer.viewportY,
      auto: window.__terminalTest.state.autoScroll,
      focused: document.activeElement?.className || "",
    };
  });
  expect(after.viewportY).toBeLessThan(before.viewportY);
  expect(after.viewportY).toBeLessThan(before.baseY);
  expect(after.auto).toBe(false);
  // Dragging history must not pop the soft keyboard.
  expect(after.focused).not.toContain("xterm-helper-textarea");
});

test("long press selects a line and the phone copy button copies it", async ({ page, context }) => {
  await openTerminal(page);
  const box = await page.locator(".xterm-screen").boundingBox();
  const client = await context.newCDPSession(page);
  const x = box.x + box.width / 2;
  const y = box.y + box.height - 50;
  await client.send("Input.dispatchTouchEvent", {
    type: "touchStart",
    touchPoints: [{ x, y, radiusX: 8, radiusY: 8, force: 1, id: 2 }],
  });
  await page.waitForTimeout(900);
  const selected = await page.evaluate(() => window.__terminalTest.state.term.getSelection());
  await client.send("Input.dispatchTouchEvent", { type: "touchEnd", touchPoints: [] });
  expect(selected).toMatch(/^line \d{3} /);
  await expect(page.locator("#copyBtn")).toBeVisible();
  await page.locator("#copyBtn").click();
  await expect.poll(() => page.evaluate(() => window.__terminalCopied)).toBe(selected);
});

test("copy with no selection falls back to the whole transcript", async ({ page }) => {
  await openTerminal(page);
  await page.evaluate(() => window.__terminalTest.state.term.clearSelection());
  await page.locator("#copyBtn").click();
  const copied = await page.evaluate(() => window.__terminalCopied);
  expect(copied).toContain("line 000 abcdefghijklmnopqrstuvwxyz");
  expect(copied).toContain("line 119 abcdefghijklmnopqrstuvwxyz");
});

test("native Android keeps keyboard resizes local instead of sending stty", async ({ page }) => {
  await openTerminal(page, { android: true });
  await page.evaluate(() => {
    const { handleIncomingBytes } = window.__terminalTest;
    handleIncomingBytes(new TextEncoder().encode("root@target:~# "));
  });
  await page.waitForTimeout(700);
  expect(await page.evaluate(() => window.sent.filter((text) => text.startsWith("stty ")))).toEqual([]);

  await page.evaluate(() => {
    Object.defineProperty(visualViewport, "height", {
      configurable: true,
      get: () => 420,
    });
    visualViewport.dispatchEvent(new Event("resize"));
    window.__terminalTest.state.term.focus();
  });
  await expect(page.locator("html")).toHaveClass(/soft-keyboard-open/);
  await expect.poll(() => page.evaluate(() => window.__terminalTest.state.term.rows)).toBeLessThan(40);
  expect(await page.evaluate(() => window.sent.filter((text) => text.startsWith("stty ")))).toEqual([]);
});
