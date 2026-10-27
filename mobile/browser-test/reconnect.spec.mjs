import { test, expect } from "@playwright/test";

/* Automatic reconnect after an unexpected link loss. The LAN transport is the
 * easiest to drive headlessly, and the state machine is transport-independent:
 * the BLE path goes through the same onDisconnected({unexpected:true}) entry. */

test.beforeEach(async ({ page }) => {
  await page.route("**/app.js?*", async (route) => {
    const response = await route.fetch();
    await route.fulfill({
      response,
      body: await response.text() +
        "\nwindow.__test = { state, connect, disconnect, onDisconnected, setConnected, " +
        "cancelAutoReconnect, autoReconnect, elements, renderAutoReconnectStatus };" ,
    });
  });
  await page.addInitScript(() => {
    /* A WebSocket the spec can fail or drop, standing in for the bridge. */
    window.__ws = { instances: [], mode: "ok" };
    class FakeWebSocket {
      constructor(url) {
        this.url = url;
        this.readyState = 0;
        this.closed = false;
        window.__ws.instances.push(this);
        /* One macrotask, so connectWs() has installed its handlers. */
        setTimeout(() => {
          if (this.closed) return;
          this.readyState = 1;
          this.onopen?.();
          if (window.__ws.mode === "ok") {
            this.onmessage?.({ data: "@ws auth=none" });
          } else if (window.__ws.mode === "fail") {
            this.onclose?.();
          }
        }, 0);
      }
      send(data) {
        this.lastSent = data;
      }
      close() {
        if (this.closed) return;
        this.closed = true;
        this.readyState = 3;
        setTimeout(() => this.onclose?.(), 0);
      }
    }
    window.WebSocket = FakeWebSocket;
  });
  await page.goto("/");
  await page.waitForFunction(() => window.__test?.state.term);
  await page.evaluate(() => {
    /* Fast, deterministic schedule: three attempts at 10/20/30 ms. */
    window.__linkrReconnectDelays = [10, 20, 30];
    window.__test.state.mode = "ws";
    window.__test.state.ws = null;
    document.querySelector("#wsHostInput").value = "127.0.0.1:9999";
  });
});

async function connectLan(page) {
  await page.evaluate(async () => {
    await window.__test.connect();
  });
  await expect
    .poll(() => page.evaluate(() => window.__test.state.connected))
    .toBe(true);
}

async function dropLan(page) {
  await page.evaluate(() => {
    window.__ws.instances.at(-1).close();
  });
}

test("an unexpected LAN drop reconnects automatically and keeps the log", async ({ page }) => {
  await connectLan(page);
  const before = await page.evaluate(() => window.__ws.instances.length);

  await dropLan(page);
  await expect
    .poll(() => page.evaluate(() => window.__ws.instances.length), { timeout: 5000 })
    .toBe(before + 1);
  await expect
    .poll(() => page.evaluate(() => window.__test.state.connected))
    .toBe(true);
  await expect
    .poll(() => page.evaluate(() => window.__test.autoReconnect.active))
    .toBe(false);
  const text = await page.locator("#terminalOutput").innerText();
  expect(text).toContain("[reconnect]");
  expect(text).toContain("restored");
  /* The journal is not cleared by the reconnect. */
  expect(text).toContain("[connect]");
});

test("auto-reconnect gives up after the configured attempts and offers the manual button", async ({ page }) => {
  await connectLan(page);
  await page.evaluate(() => {
    window.__ws.mode = "fail";
  });
  const before = await page.evaluate(() => window.__ws.instances.length);
  await dropLan(page);
  await expect
    .poll(() => page.evaluate(() => window.__test.autoReconnect.active), { timeout: 5000 })
    .toBe(false);
  const after = await page.evaluate(() => window.__ws.instances.length);
  /* The drop's own reconnect plus one dial per configured attempt. */
  expect(after).toBe(before + 3);
  await expect(page.locator("#connectButton .btn-label")).toHaveText(/Connect/i);
  const text = await page.locator("#terminalOutput").innerText();
  expect(text).toContain("Automatic reconnect failed");
});

test("clicking the button while reconnecting cancels the schedule", async ({ page }) => {
  await connectLan(page);
  const before = await page.evaluate(() => window.__ws.instances.length);
  await dropLan(page);
  await page.evaluate(() => {
    document.querySelector("#connectButton").click();
  });
  await expect
    .poll(() => page.evaluate(() => window.__test.autoReconnect.active))
    .toBe(false);
  await page.waitForTimeout(200);
  expect(await page.evaluate(() => window.__ws.instances.length)).toBe(before);
  const text = await page.locator("#terminalOutput").innerText();
  expect(text).toContain("cancelled");
});

test("a manual disconnect is not treated as a lost link", async ({ page }) => {
  await connectLan(page);
  const before = await page.evaluate(() => window.__ws.instances.length);
  await page.evaluate(async () => {
    await window.__test.disconnect();
  });
  await expect
    .poll(() => page.evaluate(() => window.__test.state.connected))
    .toBe(false);
  await page.waitForTimeout(200);
  expect(await page.evaluate(() => window.__ws.instances.length)).toBe(before);
  const text = await page.locator("#terminalOutput").innerText();
  expect(text).not.toContain("[reconnect]");
});

test("calling onDisconnected without the unexpected flag never schedules a retry", async ({ page }) => {
  await connectLan(page);
  const before = await page.evaluate(() => window.__ws.instances.length);
  await page.evaluate(() => {
    window.__test.onDisconnected();
  });
  await page.waitForTimeout(200);
  expect(await page.evaluate(() => window.__test.autoReconnect.active)).toBe(false);
  expect(await page.evaluate(() => window.__ws.instances.length)).toBe(before);
});

test("BLE uses the restored device as the reconnect target and shows a cancel button", async ({ page }) => {
  await page.evaluate(() => {
    window.__linkrReconnectDelays = [5000];
    const { state, setConnected, onDisconnected } = window.__test;
    state.mode = "ble";
    state.device = { id: "AA:BB:CC:DD:EE:FF", name: "Linkr Bee 36E8" };
    state.nusReady = true;
    state.mgmtReady = true;
    setConnected(true);
    onDisconnected({ unexpected: true });
  });
  await expect
    .poll(() => page.evaluate(() => window.__test.autoReconnect.active))
    .toBe(true);
  await expect(page.locator("#connectButton .btn-label")).toHaveText(/Cancel reconnect/i);
  await page.evaluate(() => {
    window.__test.cancelAutoReconnect("cancelled by test");
  });
  expect(await page.evaluate(() => window.__test.autoReconnect.active)).toBe(false);
});
