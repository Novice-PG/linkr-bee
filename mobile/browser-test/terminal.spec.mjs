import { test, expect } from "@playwright/test";

test.beforeEach(async ({ page }) => {
  // Exercise the shipped app and vendored xterm. Replace only the transport;
  // no native BLE device is needed to observe the exact outgoing UART bytes.
  await page.route("**/app.js?*", async (route) => {
    const response = await route.fetch();
    await route.fulfill({
      response,
      body: await response.text() +
        "\nwindow.__test = { state, setConnected, setTerminalFallbackFullscreen };",
    });
  });
  await page.goto("/");
  await page.waitForFunction(() => window.__test?.state.term);
  await page.evaluate(() => {
    window.sent = [];
    const { state, setConnected } = window.__test;
    state.mode = "ws";
    state.ws = { readyState: 1, send: (bytes) => window.sent.push(Array.from(bytes)) };
    setConnected(true);
    state.term.focus();
  });
});

async function terminalWrite(page, text) {
  await page.evaluate((text) => new Promise((resolve) => {
    window.__test.state.term.write(text, resolve);
  }), text);
}

for (const [modifier, expected] of [["ctrl", [3]], ["shift", [67]], ["alt", [27, 99]]]) {
  test(`${modifier} survives terminal status and cursor-position responses`, async ({ page }) => {
    const button = page.locator(`[data-modifier="${modifier}"]`);
    await button.tap();
    await terminalWrite(page, "\x1b[5n\x1b[6n");
    await expect.poll(() => page.evaluate(() => window.sent.length)).toBe(2);
    await expect(button).toHaveAttribute("aria-pressed", "true");
    expect(await page.evaluate(() => window.sent[0])).toEqual([27, 91, 48, 110]);
    await page.keyboard.type("c");
    await expect.poll(() => page.evaluate(() => window.sent.at(-1))).toEqual(expected);
    await expect(button).toHaveAttribute("aria-pressed", "false");
  });
}

test("focus reports preserve Ctrl, while a physical arrow consumes it", async ({ page }) => {
  await terminalWrite(page, "\x1b[?1004h");
  await page.evaluate(() => window.__test.state.term.blur());
  await page.locator('[data-modifier="ctrl"]').tap();
  await expect.poll(() => page.evaluate(() => window.sent.at(-1))).toEqual([27, 91, 73]);
  await expect(page.locator('[data-modifier="ctrl"]')).toHaveAttribute("aria-pressed", "true");
  await page.keyboard.press("ArrowDown");
  await expect(page.locator('[data-modifier="ctrl"]')).toHaveAttribute("aria-pressed", "false");
  await page.keyboard.type("c");
  await expect.poll(() => page.evaluate(() => window.sent.at(-1))).toEqual([99]);
});

test("mobile input, IME and bracketed paste are classified as user input", async ({ page }) => {
  await page.locator('[data-modifier="ctrl"]').tap();
  // Mobile keyboards may emit input without any keydown/keypress.
  await page.evaluate(() => {
    const input = window.__test.state.term.textarea;
    input.value = "c";
    input.dispatchEvent(new InputEvent("input", {
      bubbles: true, inputType: "insertText", data: "c",
    }));
  });
  await expect.poll(() => page.evaluate(() => window.sent.at(-1))).toEqual([3]);
  await page.locator('[data-modifier="alt"]').tap();
  // xterm commits compositions asynchronously, outside the DOM event stack.
  await page.evaluate(() => {
    const input = window.__test.state.term.textarea;
    input.value = "";
    input.dispatchEvent(new CompositionEvent("compositionstart", { bubbles: true }));
    input.value = "你好";
    input.dispatchEvent(new CompositionEvent("compositionupdate", { bubbles: true, data: "你好" }));
    input.dispatchEvent(new CompositionEvent("compositionend", { bubbles: true, data: "你好" }));
  });
  await expect.poll(() => page.evaluate(() => window.sent.at(-1))).toEqual([...Buffer.from("你好")]);
  await expect(page.locator('[data-modifier="alt"]')).toHaveAttribute("aria-pressed", "false");
  await terminalWrite(page, "\x1b[?2004h");
  await page.locator('[data-modifier="shift"]').tap();
  await page.evaluate(() => window.__test.state.term.paste("hello"));
  await expect.poll(() => page.evaluate(() => window.sent.at(-1)))
    .toEqual([...Buffer.from("\x1b[200~hello\x1b[201~")]);
  await expect(page.locator('[data-modifier="shift"]')).toHaveAttribute("aria-pressed", "false");
});

for (const [device, width, height] of [["phone", 390, 844], ["tablet", 1024, 1366], ["landscape", 844, 390]]) {
  for (const mode of ["workspace", "fallback", "fullscreen"]) {
    test(`${device} ${mode}: accessory keys stay above the soft keyboard`, async ({ page }) => {
      await page.setViewportSize({ width, height });
      if (mode === "fallback") {
        await page.evaluate(() => window.__test.setTerminalFallbackFullscreen(true));
      } else if (mode === "fullscreen") {
        await page.locator("#fullscreenBtn").tap();
        await page.waitForFunction(() => document.fullscreenElement);
      }
      await page.evaluate(() => window.__test.state.term.focus());
      // Let resize/orientation/fullscreen events establish the unoccluded
      // viewport baseline before simulating the keyboard's height change.
      await page.evaluate(() => new Promise((resolve) => {
        requestAnimationFrame(() => requestAnimationFrame(resolve));
      }));
      const visibleHeight = Math.floor(height * 0.55);
      // Model resize-only WebViews: layout viewport and dvh stay unchanged.
      await page.evaluate((visibleHeight) => {
        Object.defineProperty(visualViewport, "height", {
          configurable: true, get: () => visibleHeight,
        });
        visualViewport.dispatchEvent(new Event("resize"));
      }, visibleHeight);
      await expect(page.locator("html")).toHaveClass(/soft-keyboard-open/);
      await expect.poll(() => page.locator("#terminalKeyBar").evaluate((bar) => {
        const rect = bar.getBoundingClientRect();
        return rect.top >= 0 && rect.bottom <= visualViewport.height;
      })).toBe(true);
      await page.locator('[data-terminal-key="Tab"]').tap();
      await expect.poll(() => page.evaluate(() => window.sent.at(-1))).toEqual([9]);
      await page.evaluate(() => {
        delete visualViewport.height;
        visualViewport.dispatchEvent(new Event("resize"));
      });
      await expect(page.locator("html")).not.toHaveClass(/soft-keyboard-open/);
    });
  }
}
