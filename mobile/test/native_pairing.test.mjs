import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

const source = ts.transpileModule(readFileSync(new URL("../src/native-bootstrap.ts", import.meta.url), "utf8"), {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
}).outputText;

async function backend(platform, bonded, fail = false) {
  const calls = [];
  const BleClient = {
    initialize: async () => {},
    connect: async () => calls.push("connect"),
    isBonded: async () => { calls.push("isBonded"); return bonded; },
    createBond: async () => { calls.push("createBond"); if (fail) throw new Error("pairing denied"); },
    disconnect: async () => calls.push("disconnect"),
    read: async () => { calls.push("encrypted read"); return new DataView(new ArrayBuffer(10)); },
  };
  const window = {};
  vm.runInNewContext(source, { window, exports: {}, require: name => name === "@capacitor/core"
    ? { Capacitor: { getPlatform: () => platform, isNativePlatform: () => true } } : { BleClient } });
  await window.LinkrNativeBleReady;
  return { ble: window.LinkrNativeBle, calls };
}

test("Android creates a missing bond but preserves an existing one", async () => {
  for (const bonded of [false, true]) {
    const { ble, calls } = await backend("android", bonded);
    await ble.connect("bee");
    assert.deepEqual(calls, bonded ? ["connect", "isBonded"] : ["connect", "isBonded", "createBond"]);
    assert.equal(ble.isConnected(), true);
  }
});

test("a denied Android pairing disconnects and never reads the protected service", async () => {
  const { ble, calls } = await backend("android", false, true);
  await assert.rejects(ble.connect("bee"), /pairing denied/);
  assert.deepEqual(calls, ["connect", "isBonded", "createBond", "disconnect"]);
  assert.equal(ble.isConnected(), false);
});

test("iOS leaves pairing to its encrypted characteristic read", async () => {
  const { ble, calls } = await backend("ios", false);
  await ble.connect("bee");
  await ble.read("bee", "management", "protocol");
  assert.deepEqual(calls, ["connect", "encrypted read"]);
});
