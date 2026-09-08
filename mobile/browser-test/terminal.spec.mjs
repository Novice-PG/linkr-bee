import { test, expect } from "@playwright/test";

test.beforeEach(async ({ page }) => {
  // Exercise the shipped app and vendored xterm. Replace only the transport;
  // no native BLE device is needed to observe the exact outgoing UART bytes.
  await page.route("**/app.js?*", async (route) => {
    const response = await route.fetch();
    await route.fulfill({
      response,
      body: await response.text() +
        "\nwindow.__test = { state, setConnected, setTerminalFallbackFullscreen, sendControl, bleTransport, onDisconnected, enqueueBytes };",
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

for (const phase of ["queued", "fragmented"]) {
  test(`management commands do not cross device sessions when ${phase}`, async ({ page }) => {
    const result = await page.evaluate(async (phase) => {
      const { state, setConnected, sendControl, bleTransport, onDisconnected } = window.__test;
      state.mode = "ble";
      state.device = { id: "old-device" };
      state.mgmtReady = true;
      setConnected(true);
      document.querySelector("#chunkInput").value = "20";
      const writes = [];
      const reconnect = () => {
        onDisconnected();
        state.device = { id: "new-device" };
        state.mgmtReady = true;
        setConnected(true);
      };
      let release;
      if (phase === "queued") {
        state.controlWriteQueue = new Promise((resolve) => { release = resolve; });
      }
      bleTransport.write = async (deviceId, _service, _characteristic, chunk) => {
        writes.push({ deviceId, length: chunk.length });
        if (phase === "fragmented" && writes.length === 1) reconnect();
        // Permit the original implementation to settle after leaking a command
        // to the next device, instead of leaving a ten-second response timeout.
        if (deviceId === "new-device") {
          for (const id of state.mgmtResponses.pending.keys()) {
            state.mgmtResponses.settle({ requestId: id, type: 2, flags: 0 });
          }
        }
      };
      const operation = sendControl("@w=" + "s".repeat(50)).then(
        () => ({ rejected: false }), () => ({ rejected: true }),
      );
      if (phase === "queued") {
        reconnect();
        release();
      }
      return { ...await operation, writes };
    }, phase);
    expect(result.rejected).toBe(true);
    expect(result.writes).toEqual(phase === "queued" ? [] : [{ deviceId: "old-device", length: 20 }]);
  });
}

for (const failure of ["cancel-fragment", "reject-final-fragment", "reject-complete-frame"]) {
  test(`uncertain Reliable UART ${failure} blocks later input until reconnect`, async ({ page }) => {
    const result = await page.evaluate(async (failure) => {
      const { state, setConnected, enqueueBytes, bleTransport } = window.__test;
      state.mode = "ble";
      state.device = { id: "device" };
      state.nusReady = state.reliableReady = true;
      state.reliableMaxPayload = 232;
      state.reliableWriteSize = failure === "reject-complete-frame" ? 244 : 20;
      setConnected(true);
      const abort = new AbortController();
      const writes = [];
      bleTransport.write = async (_id, _service, _characteristic, chunk) => {
        // The peripheral may already have accepted these bytes before the local
        // promise rejects or the user cancels. Never infer rejection = unsent.
        writes.push([...chunk]);
        if (failure === "cancel-fragment") abort.abort();
        if (failure === "reject-complete-frame" ||
            (failure === "reject-final-fragment" && writes.length === 2)) {
          throw new Error("GATT write response timed out");
        }
      };
      const first = await enqueueBytes(new Uint8Array(28).fill(65), false, { signal: abort.signal })
        .then(() => "sent", (error) => error.message);
      const writesAfterFailure = writes.length;
      const next = await enqueueBytes(new TextEncoder().encode("pwd\r"))
        .then(() => "sent", (error) => error.message);
      const writesAfterNext = writes.length;
      // Simulate a fresh handshake, which resets the peer's frame assembler and
      // reads its current sequence before accepting more user input.
      setConnected(false);
      setConnected(true);
      bleTransport.write = async (_id, _service, _characteristic, chunk) => writes.push([...chunk]);
      await enqueueBytes(new TextEncoder().encode("pwd\r"));
      return { first, next, writesAfterFailure, writesAfterNext, writesAfterReconnect: writes.length };
    }, failure);
    expect(result.first).toMatch(/uncertain.*reconnect/i);
    expect(result.next).toMatch(/uncertain.*reconnect/i);
    expect(result.writesAfterFailure).toBe(failure === "reject-final-fragment" ? 2 : 1);
    expect(result.writesAfterNext).toBe(result.writesAfterFailure);
    expect(result.writesAfterReconnect).toBe(result.writesAfterFailure + 1);
  });
}

test("uncertain management fragment blocks queued settings until reconnect", async ({ page }) => {
  const result = await page.evaluate(async () => {
    const { state, setConnected, sendControl, bleTransport } = window.__test;
    state.mode = "ble";
    state.device = { id: "device" };
    state.mgmtReady = true;
    setConnected(true);
    document.querySelector("#chunkInput").value = "20";
    let writes = 0;
    bleTransport.write = async () => {
      writes++;
      if (writes === 2) throw new Error("GATT write response timed out");
      // Avoid waiting for responses if the regression leaks the queued frame.
      if (writes > 2) for (const id of state.mgmtResponses.pending.keys()) {
        state.mgmtResponses.settle({ requestId: id, type: 2, flags: 0 });
      }
    };
    const resultOf = (operation) => operation.then(() => "sent", (error) => error.message);
    const first = resultOf(sendControl("@w=" + "s".repeat(50)));
    const queued = resultOf(sendControl("@w?"));
    const outcomes = await Promise.all([first, queued]);
    const writesBeforeReconnect = writes;
    setConnected(false);
    setConnected(true);
    await sendControl("@w?");
    return { outcomes, writesBeforeReconnect, writesAfterReconnect: writes };
  });
  for (const outcome of result.outcomes) expect(outcome).toMatch(/uncertain.*reconnect/i);
  expect(result.writesBeforeReconnect).toBe(2);
  expect(result.writesAfterReconnect).toBe(3);
});

test("explicit ATT size rejection still falls back before a frame starts", async ({ page }) => {
  const result = await page.evaluate(async () => {
    const { state, setConnected, enqueueBytes, bleTransport } = window.__test;
    state.mode = "ble";
    state.device = { id: "device" };
    state.nusReady = state.reliableReady = true;
    state.reliableMaxPayload = 232;
    state.reliableWriteSize = 244;
    setConnected(true);
    const attempts = [];
    const accepted = [];
    bleTransport.write = async (_id, _service, _characteristic, chunk) => {
      attempts.push(chunk.length);
      if (chunk.length > 20) throw new Error("GATT Invalid Attribute Length.");
      accepted.push(...chunk);
    };
    await enqueueBytes(new Uint8Array(80).fill(65));
    return { attempts, accepted, nextSequence: state.reliableTxSequence, writeError: state.uartWriteError };
  });
  expect(result.attempts).toEqual([92, 62, 20, 20, 20, 20, 12]);
  expect(result.accepted.slice(12)).toEqual(new Array(80).fill(65));
  expect(result.nextSequence).toBe(2);
  expect(result.writeError).toBe("");
});

test("IME Escape does not exit terminal fullscreen", async ({ page }) => {
  await page.evaluate(() => window.__test.setTerminalFallbackFullscreen(true));
  for (const composition of [{ isComposing: true }, { keyCode: 229 }]) {
    await page.evaluate((composition) => {
      window.__test.state.term.textarea.dispatchEvent(new KeyboardEvent("keydown", {
        key: "Escape", bubbles: true, ...composition,
      }));
    }, composition);
    await expect(page.locator("#terminalCard")).toHaveClass(/terminal-fullscreen-fallback/);
  }
  await page.keyboard.press("Escape");
  await expect(page.locator("#terminalCard")).not.toHaveClass(/terminal-fullscreen-fallback/);
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
