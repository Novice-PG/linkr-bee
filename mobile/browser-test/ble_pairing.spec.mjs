import { test, expect } from "@playwright/test";

test("a refused encrypted read shows GPIO1 recovery and sends no UART data", async ({ page }) => {
  await page.route("**/app.js?*", async (route) => {
    const response = await route.fetch();
    await route.fulfill({ response, body: await response.text() +
      "\nwindow.__pairing = { state, connect, bleTransport };" });
  });
  await page.goto("/");
  await page.waitForFunction(() => window.__pairing?.state.term);
  const result = await page.evaluate(async () => {
    const { state, connect, bleTransport } = window.__pairing;
    const calls = [];
    Object.assign(bleTransport, {
      isAvailable: () => true, initialize: async () => {},
      requestDevice: async () => ({ id: "new-bee", name: "Linkr BLE UART-3" }),
      connect: async () => calls.push("connect"),
      read: async () => { calls.push("read"); throw new Error("Insufficient authentication"); },
      disconnect: async () => calls.push("disconnect"),
      write: async () => calls.push("write"),
    });
    state.device = null;
    await connect();
    return { calls, connected: state.connected };
  });
  expect(result).toEqual({ calls: ["connect", "read", "disconnect"], connected: false });
  await expect(page.locator(".toast").filter({ hasText: "GPIO1" })).toBeVisible();
  await expect(page.locator("#connectButton")).toBeEnabled();
});

test("overlapping reconnects allow only one attempt and unlock after failure", async ({ page }) => {
  await page.route("**/app.js?*", async route => {
    const response = await route.fetch();
    await route.fulfill({ response, body: await response.text() +
      "\nwindow.__pairing = { state, connect, bleTransport };" });
  });
  await page.goto("/");
  await page.waitForFunction(() => window.__pairing?.state.term);
  const result = await page.evaluate(async () => {
    const { state, connect, bleTransport } = window.__pairing;
    let attempts = 0, rejectConnect;
    Object.assign(bleTransport, {
      isAvailable: () => true, initialize: async () => {},
      connect: () => { attempts++; return new Promise((_, reject) => { rejectConnect = reject; }); },
    });
    state.device = { id: "saved-bee", name: "Bee" };
    const first = connect();
    await Promise.resolve();
    const disabled = document.querySelector("#connectButton").disabled;
    await connect();
    rejectConnect(new Error("Connection attempt failed"));
    await first;
    const unlocked = !document.querySelector("#connectButton").disabled;
    const second = connect();
    await Promise.resolve();
    rejectConnect(new Error("Connection attempt failed"));
    await second;
    return { attempts, disabled, unlocked, connected: state.connected };
  });
  expect(result).toEqual({ attempts: 2, disabled: true, unlocked: true, connected: false });
});

test("one connect action remembers the device and switching can be cancelled", async ({ page }) => {
  await page.route("**/app.js?*", async route => {
    const response = await route.fetch();
    await route.fulfill({ response, body: await response.text() +
      "\nwindow.__pairing = { state, connect, bleTransport, rememberDevice };" });
  });
  await page.goto("/");
  await page.waitForFunction(() => window.__pairing?.state.term);
  const result = await page.evaluate(async () => {
    const { state, bleTransport, rememberDevice } = window.__pairing;
    let selections = 0;
    const connections = [];
    Object.assign(bleTransport, {
      isAvailable: () => true, initialize: async () => {},
      requestDevice: async () => {
        selections++;
        if (selections === 1) throw new DOMException("Cancelled", "NotFoundError");
        return { id: "other", name: "Other Bee" };
      },
      connect: async id => { connections.push(id); throw new Error("Test unavailable"); },
    });
    rememberDevice({ id: "saved", name: "Saved Bee" });
    const label = document.querySelector("#connectButton .btn-label").textContent;
    await window.__pairing.connect();
    await window.__pairing.connect({ chooseDevice: true });
    const preserved = state.device.id;
    await window.__pairing.connect({ chooseDevice: true });
    return { label, selections, connections, preserved, selected: state.device.id };
  });
  expect(["Reconnect", "重新连接"]).toContain(result.label);
  expect(result).toMatchObject({ selections: 2, connections: ["saved", "other"], preserved: "saved", selected: "other" });
  await expect(page.locator("#reconnectButton")).toHaveCount(0);
  await expect(page.locator("#switchDeviceButton")).toBeVisible();
});
