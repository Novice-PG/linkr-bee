import assert from "node:assert/strict";
import test from "node:test";
import { browserHostLabel, parseBondNames } from "../../web/bond_ui.js";

test("names are joined to the bond ids the BOND lines use", () => {
  const names = parseBondNames('OK bonds names\n'
    + 'NAME 0/AA:BB:CC:DD:EE:01 "longlong-mbp"\n'
    + 'NAME 1/AA:BB:CC:DD:EE:02 "longlong 的 iPhone"');
  assert.equal(names.get("0/AA:BB:CC:DD:EE:01"), "longlong-mbp");
  assert.equal(names.get("1/AA:BB:CC:DD:EE:02"), "longlong 的 iPhone");
  assert.equal(names.size, 2);
});

test("a bond without a name simply is not in the map", () => {
  const names = parseBondNames('OK bonds names\nNAME 0/AA:BB:CC:DD:EE:01 "mbp"');
  assert.equal(names.has("1/AA:BB:CC:DD:EE:02"), false);
});

test("firmware that predates the command leaves the addresses alone", () => {
  for (const reply of ["ERR unknown command", "ERR bonds encrypted BLE required", "", "OK bonds?\nBOND 0/AA:BB:CC:DD:EE:01 self=1"]) {
    assert.equal(parseBondNames(reply).size, 0, reply);
  }
});

test("a malformed line is skipped instead of failing the whole list", () => {
  const names = parseBondNames('OK bonds names\n'
    + "NAME nonsense\n"
    + 'NAME 0/AA:BB:CC:DD:EE:01 "ok"');
  assert.equal(names.size, 1);
  assert.equal(names.get("0/AA:BB:CC:DD:EE:01"), "ok");
});

test("the browser reports what it can see, and nothing when it cannot", () => {
  const cases = [
    ["Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36", "Chrome · macOS"],
    ["Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Safari/605.1.15", "Safari · macOS"],
    ["Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1", "Safari · iOS"],
    ["Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Mobile Safari/537.36", "Chrome · Android"],
    ["Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36 Edg/120.0.0.0", "Edge · Windows"],
    ["Mozilla/5.0 (X11; Linux x86_64; rv:121.0) Gecko/20100101 Firefox/121.0", "Firefox · Linux"],
    ["something odd", ""],
    ["", ""],
  ];
  for (const [userAgent, expected] of cases) {
    assert.equal(browserHostLabel({ userAgent }), expected, userAgent);
  }
  assert.equal(browserHostLabel(undefined), "");
});

// Read-only peers may label themselves, never another host or delete pairings.
import { isReadonlyManagementCommand } from "../../web/serial_control.js";
test("bond names do not require console ownership but deletion still does", () => {
  for (const cmd of ["@linkr bonds?", "@linkr bonds names?", "@linkr bonds name Chrome · Linux"]) {
    assert.equal(isReadonlyManagementCommand(cmd), true, cmd);
  }
  for (const cmd of ["@linkr bonds remove 0/AA:BB:CC:DD:EE:01", "@linkr bonds names bad", "@linkr bonds name a\n@linkr bonds remove all"]) {
    assert.equal(isReadonlyManagementCommand(cmd), false, cmd);
  }
});
