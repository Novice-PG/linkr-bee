import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import path from "node:path";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

const transportSource = await readFile(
  path.resolve(import.meta.dirname, "../../web/ble_transport.js"),
  "utf8",
);

const nativeBootstrapSource = ts.transpileModule(
  await readFile(path.resolve(import.meta.dirname, "../src/native-bootstrap.ts"), "utf8"),
  {
    compilerOptions: {
      module: ts.ModuleKind.CommonJS,
      target: ts.ScriptTarget.ES2022,
    },
  },
).outputText;

/* Values produced inside a vm context carry that realm's Object/Array
 * prototypes, which strict deep equality rejects. Normalize before comparing. */
function plain(value) {
  return JSON.parse(JSON.stringify(value));
}

function installTransport(window = {}) {
  window.navigator ??= {};
  const context = vm.createContext({
    window,
    ArrayBuffer,
    DataView,
    Map,
    Promise,
    TypeError,
    Uint8Array,
  });
  vm.runInContext(transportSource, context, {
    filename: "web/ble_transport.js",
  });
  return window.LinkrBleTransport;
}

function installFakeTimers() {
  let nextId = 1;
  const pending = new Map();
  return {
    setTimeout(callback, delay) {
      const id = nextId;
      nextId += 1;
      pending.set(id, { callback, delay });
      return id;
    },
    clearTimeout(id) {
      pending.delete(id);
    },
    delays() {
      return [...pending.values()].map((timer) => timer.delay);
    },
    flush() {
      const timers = [...pending.values()];
      pending.clear();
      for (const timer of timers) timer.callback();
    },
  };
}

/* Let promise chains inside the sandbox run up to the point where the scan
 * has started and the fake timer has been registered. */
async function waitFor(predicate) {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    if (predicate()) return;
    await new Promise((resolve) => setImmediate(resolve));
  }
  assert.fail("timed out waiting for the scan to start");
}

function scanResult({ id, localName, deviceName, rssi } = {}) {
  return {
    device: id === undefined ? undefined : { deviceId: id, name: deviceName },
    localName,
    rssi,
  };
}

async function nativeBackend({ platform = "android", requestLEScan, stopLEScan } = {}) {
  const calls = [];
  const scan = { options: null, callback: null };
  const timers = installFakeTimers();
  const BleClient = {
    initialize: async () => {
      calls.push(["initialize"]);
    },
    requestLEScan:
      requestLEScan ||
      (async (options, callback) => {
        calls.push(["requestLEScan"]);
        scan.options = options;
        scan.callback = callback;
      }),
    stopLEScan:
      stopLEScan ||
      (async () => {
        calls.push(["stopLEScan"]);
      }),
  };
  const window = {};
  const context = vm.createContext({
    window,
    exports: {},
    require: (name) => {
      if (name === "@capacitor/core") {
        return {
          Capacitor: {
            getPlatform: () => platform,
            isNativePlatform: () => true,
          },
        };
      }
      // The MCP transport is a separate native surface; only the BLE backend
      // is under test here.
      if (name === "./mcp-native") {
        return { installLinkrMcpBridge: () => {} };
      }
      return { BleClient };
    },
    setTimeout: timers.setTimeout,
    clearTimeout: timers.clearTimeout,
  });
  vm.runInContext(nativeBootstrapSource, context, {
    filename: "src/native-bootstrap.ts",
  });
  await window.LinkrNativeBleReady;
  return { ble: window.LinkrNativeBle, calls, scan, timers };
}

test("transport delegates scanDevices to the native backend and forwards options", async () => {
  const calls = [];
  const devices = [{ id: "bee-1", name: "Linkr BLE UART-1", rssi: -42 }];
  const transport = installTransport({
    LinkrNativeBle: {
      platform: "android",
      scanDevices: async (options) => {
        calls.push(options);
        return devices;
      },
    },
  });

  const options = { services: ["0000ffe0-0000-1000-8000-00805f9b34fb"], durationMs: 1200 };
  const result = await transport.scanDevices(options);

  assert.deepEqual(result, devices);
  assert.deepEqual(plain(calls), [options]);
  // The wrapper must pass the same object through, not a rebuilt copy, so the
  // backend alone owns the duration clamp and service filtering.
  assert.equal(calls[0], options);
});

test("transport rejects headless scanning on a platform without it", async () => {
  const transport = installTransport({});
  await assert.rejects(transport.scanDevices(), {
    name: "Error",
    message: "BLE scanning is not available on this platform",
  });
});

test("native scanDevices filters, maps, dedupes and always stops the scan", async () => {
  const { ble, calls, scan, timers } = await nativeBackend();
  const services = ["0000ffe0-0000-1000-8000-00805f9b34fb"];

  const pending = ble.scanDevices({ services, durationMs: 2000 });
  await waitFor(() => timers.delays().length === 1);
  assert.deepEqual(plain(calls), [["initialize"], ["requestLEScan"]]);
  assert.deepEqual(plain(scan.options), {
    allowDuplicates: false,
    services,
  });
  assert.deepEqual(timers.delays(), [2000]);

  scan.callback(scanResult({ id: "be:e0:01", localName: "Bee One", deviceName: "ignored", rssi: -51 }));
  // No localName: fall back to device.name.
  scan.callback(scanResult({ id: "be:e0:02", deviceName: "Bee Two", rssi: undefined }));
  // No name at all: empty string rather than undefined.
  scan.callback(scanResult({ id: "be:e0:03" }));
  // No device id: not connectable, so it is not part of the result.
  scan.callback(scanResult({ localName: "Ghost" }));
  scan.callback({ localName: "Ghost" });
  // A duplicate device updates its slot without changing insertion order.
  scan.callback(scanResult({ id: "be:e0:01", localName: "Bee One", rssi: -42 }));

  timers.flush();
  const devices = await pending;

  assert.deepEqual(plain(devices), [
    { id: "be:e0:01", name: "Bee One", rssi: -42 },
    { id: "be:e0:02", name: "Bee Two", rssi: null },
    { id: "be:e0:03", name: "", rssi: null },
  ]);
  assert.deepEqual(plain(calls), [["initialize"], ["requestLEScan"], ["stopLEScan"]]);
});

test("native scanDevices clamps the scan window and omits an empty service filter", async () => {
  const cases = [
    { options: {}, delay: 6000 },
    { options: undefined, delay: 6000 },
    { options: { durationMs: 10 }, delay: 500 },
    { options: { durationMs: 60000 }, delay: 15000 },
    { options: { durationMs: Number.NaN }, delay: 6000 },
    { options: { services: [], durationMs: 500 }, delay: 500 },
  ];

  for (const { options, delay } of cases) {
    const { ble, scan, timers } = await nativeBackend();
    const pending = ble.scanDevices(options);
    await waitFor(() => timers.delays().length === 1);

    assert.deepEqual(timers.delays(), [delay], `duration for ${JSON.stringify(options)}`);
    assert.deepEqual(plain(scan.options), { allowDuplicates: false });
    assert.equal(Object.hasOwn(plain(scan.options), "services"), false);

    timers.flush();
    assert.deepEqual(plain(await pending), []);
  }
});

test("native scanDevices caps the returned list at 50 devices", async () => {
  const { ble, scan, timers } = await nativeBackend();
  const pending = ble.scanDevices({ durationMs: 500 });
  await waitFor(() => timers.delays().length === 1);

  for (let index = 0; index < 55; index += 1) {
    scan.callback(scanResult({ id: `device-${index}`, localName: `Bee ${index}` }));
  }

  timers.flush();
  const devices = await pending;

  assert.equal(devices.length, 50);
  assert.equal(devices[0].id, "device-0");
  assert.equal(devices[49].id, "device-49");
});

test("native scanDevices stops the scan when the scan itself fails", async () => {
  const { ble, calls } = await nativeBackend({
    requestLEScan: async () => {
      throw new Error("scan denied");
    },
  });

  await assert.rejects(ble.scanDevices({ durationMs: 500 }), /scan denied/);
  assert.deepEqual(plain(calls), [["initialize"], ["stopLEScan"]]);
});
