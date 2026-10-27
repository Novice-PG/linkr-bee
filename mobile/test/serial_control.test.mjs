import { test } from "node:test";
import assert from "node:assert/strict";
import { parseSerialControl, isReadonlyManagementCommand } from "../../web/serial_control.js";

test("control status is validated against its owner identity", () => {
  const status = "OK control self=2 owner=1 mode=readonly epoch=3 peers=2 ble_max=2 linkr_reserved=0\r\n";
  assert.deepEqual(parseSerialControl(status), { self: 2, owner: 1, mode: "readonly", epoch: 3, peers: 2, capacity: 2 });
  for (const text of [status.replace("mode=readonly", "mode=control"), status.replace("owner=1", "owner=nan"),
    status.replace("self=2", "self=0"), status.replace("epoch=3", "epoch=-1"), "ERR control=busy"]) {
    assert.equal(parseSerialControl(text), null);
  }
  assert.equal(parseSerialControl(status.replace("owner=1 mode=readonly", "owner=0 mode=available")).mode, "available");
});

test("readonly clients can query and negotiate but cannot mutate device settings", () => {
  for (const command of ["@c?", "@c claim", "@c release", "@c yield 2", "@i?", "@u?", "@linkr target?"]) {
    assert.equal(isReadonlyManagementCommand(command), true, command);
  }
  for (const command of ["@u=115200,8,n,1,n", "@w=ssid,pass", "@s?", "@s token off", "@linkr target clear", "@c yield 0"]) {
    assert.equal(isReadonlyManagementCommand(command), false, command);
  }
});
