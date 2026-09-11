import assert from "node:assert/strict";
import test from "node:test";

import {
  MGMT_FLAG_ERROR,
  MGMT_FRAME_ERRORS,
  RELIABLE_FRAME_ERRORS,
  ManagementResponseTracker,
  createFragmentReassembler,
  parseManagementHeader,
  parseReliableHeader,
  parseWifiEvent,
  wifiProvisioningComplete,
  isIpv4Address,
} from "../../web/management_protocol.js";

test("management request resolves only for its matching response", async () => {
  const tracker = new ManagementResponseTracker(100);
  const response = tracker.wait(42);

  assert.equal(
    tracker.settle({ type: 3, requestId: 42, flags: 0, text: "event" }),
    false,
  );
  assert.equal(
    tracker.settle({ type: 2, requestId: 7, flags: 0, text: "OK other" }),
    false,
  );
  assert.equal(
    tracker.settle({ type: 2, requestId: 42, flags: 1, text: "OK uart" }),
    true,
  );
  assert.equal((await response).requestId, 42);
});

test("management error response rejects the request", async () => {
  const tracker = new ManagementResponseTracker(100);
  const response = tracker.wait(3);

  tracker.settle({
    type: 2,
    requestId: 3,
    flags: MGMT_FLAG_ERROR,
    text: "ERR invalid UART",
  });

  await assert.rejects(response, /ERR invalid UART/);
});

test("management requests reject on timeout and disconnect", async () => {
  const tracker = new ManagementResponseTracker(5);
  await assert.rejects(tracker.wait(9), /timed out/);

  const disconnected = tracker.wait(10);
  tracker.rejectAll(new Error("Disconnected"));
  await assert.rejects(disconnected, /Disconnected/);
});

test("WiFi lifecycle events parse into the documented fields", () => {
  assert.deepEqual(
    parseWifiEvent("@event wifi operation=42 phase=ready result=0 state=connected ip=10.0.0.5 error=0"),
    {
      operation: 42,
      phase: "ready",
      result: 0,
      state: "connected",
      ip: "10.0.0.5",
      error: "0",
      isFinal: true,
      failed: false,
    },
  );

  const failed = parseWifiEvent("@event wifi operation=42 phase=failed result=-16 state=off ip=down error=-16");
  assert.equal(failed.failed, true);
  assert.equal(failed.isFinal, true);
  assert.equal(failed.error, "-16");

  const queued = parseWifiEvent("@event wifi operation=42 phase=queued result=0 state=off ip=down error=0");
  assert.equal(queued.isFinal, false);
  assert.equal(queued.failed, false);

  // Tolerates the CRLF the bridge appends, and rejects everything else.
  assert.equal(parseWifiEvent("@event wifi operation=1 phase=off result=0 state=off ip=down error=0\r\n").isFinal, true);
  for (const line of [
    "OK wifi=connected,ssid=ap,ip=10.0.0.5",
    "@event uart operation=1 phase=ready result=0",
    "@event wifi operation=x phase=ready result=0",
    "@event wifi phase=ready",
    "",
    null,
    undefined,
  ]) {
    assert.equal(parseWifiEvent(line), null, String(line));
  }
});

test("provisioning completes only on ready with a usable address", () => {
  const ready = (ip) => parseWifiEvent(`@event wifi operation=1 phase=ready result=0 state=connected ip=${ip} error=0`);

  // The API is explicit: the ready phase without an address is not completion.
  assert.equal(wifiProvisioningComplete(ready("10.0.0.5")), true);
  assert.equal(wifiProvisioningComplete(ready("ready")), false);
  assert.equal(wifiProvisioningComplete(ready("down")), false);
  assert.equal(wifiProvisioningComplete(parseWifiEvent("@event wifi operation=1 phase=dhcp result=0 state=connected ip=down error=0")), false);
  assert.equal(wifiProvisioningComplete(parseWifiEvent("@event wifi operation=1 phase=failed result=-1 state=off ip=down error=-1")), false);
  assert.equal(wifiProvisioningComplete(null), false);

  for (const good of ["0.0.0.0", "192.168.1.50", "255.255.255.255"]) {
    assert.equal(isIpv4Address(good), true, good);
  }
  for (const bad of ["", "ready", "down", "10.0.0", "10.0.0.0.0", "10.0.0.5/24", null]) {
    assert.equal(isIpv4Address(bad), false, String(bad));
  }
});

// ---------------------------------------------------------------------------
// Fragmented-frame reassembly: the first fragment carries the whole header and
// continuations carry payload only (docs/LINKR_BLE_API.zh-CN.md sections 4 and 7).
// ---------------------------------------------------------------------------

function mgmtFrame({ requestId = 42, type = 2, flags = 0, payload }) {
  const frame = new Uint8Array(12 + payload.length);
  const view = new DataView(frame.buffer);
  frame[0] = 0x4c; frame[1] = 0x4b; frame[2] = 1; frame[3] = type;
  view.setUint32(4, requestId, true);
  view.setUint16(8, payload.length, true);
  view.setUint16(10, flags, true);
  frame.set(payload, 12);
  return frame;
}

function reliableFrame({ sequence = 1, payload }) {
  const frame = new Uint8Array(12 + payload.length);
  const view = new DataView(frame.buffer);
  frame[0] = 0x4c; frame[1] = 0x52; frame[2] = 1; frame[3] = 0;
  view.setUint32(4, sequence, true);
  view.setUint16(8, payload.length, true);
  frame.set(payload, 12);
  return frame;
}

const mgmtReassembler = (maxPayload = 512) => createFragmentReassembler({
  headerSize: 12, maxPayload, parseHeader: parseManagementHeader,
});
const reliableReassembler = (maxPayload = 232) => createFragmentReassembler({
  headerSize: 12, maxPayload, parseHeader: (bytes) => parseReliableHeader(bytes, maxPayload),
});

test("a whole management response in one indication completes immediately", () => {
  const pending = mgmtReassembler();
  const payload = new TextEncoder().encode("OK uart=115200,8,N,1,none\r\n");
  const result = pending.push(mgmtFrame({ requestId: 7, payload }));
  assert.equal(result.status, "complete");
  assert.equal(result.meta.requestId, 7);
  assert.equal(result.meta.type, 2);
  assert.equal(result.meta.flags, 0);
  assert.deepEqual([...result.payload], [...payload]);
  assert.equal(pending.pending, false);
});

test("headers on the first fragment only, payload reassembled in order", () => {
  const pending = mgmtReassembler();
  const payload = Uint8Array.from({ length: 300 }, (_, i) => i % 251);
  const frame = mgmtFrame({ payload });
  // First fragment: header + part of the payload. Continuations: bare payload.
  assert.equal(pending.push(frame.slice(0, 100)).status, "incomplete");
  assert.equal(pending.pending, true);
  assert.equal(pending.push(frame.slice(100, 200)).status, "incomplete");
  const done = pending.push(frame.slice(200));
  assert.equal(done.status, "complete");
  assert.deepEqual([...done.payload], [...payload]);
});

test("sequence numbers survive a whole and a split reliable frame", () => {
  const payload = new TextEncoder().encode("boot log line\n");
  const whole = reliableReassembler().push(reliableFrame({ sequence: 9, payload }));
  assert.equal(whole.status, "complete");
  assert.equal(whole.meta.sequence, 9);
  assert.deepEqual([...whole.payload], [...payload]);

  const pending = reliableReassembler();
  const frame = reliableFrame({ sequence: 0xffffffff, payload });
  assert.equal(pending.push(frame.slice(0, 14)).status, "incomplete");
  const split = pending.push(frame.slice(14));
  assert.equal(split.status, "complete");
  assert.equal(split.meta.sequence, 0xffffffff);
});

test("unusable frames are rejected and never leave stale state behind", () => {
  const cases = [
    ["short-header", new Uint8Array(4)],
    ["bad-magic", mgmtFrame({ payload: new Uint8Array(2) }).map((b, i) => (i === 1 ? 0x58 : b))],
    ["bad-length", mgmtFrame({ payload: new Uint8Array(0) })],
  ];
  for (const [reason, bytes] of cases) {
    const pending = mgmtReassembler();
    assert.equal(pending.push(Uint8Array.from(bytes)).reason, reason, reason);
    assert.equal(pending.pending, false, reason);
  }

  // Wrong version and wrong message type are both rejected.
  const wrongVersion = mgmtFrame({ payload: new Uint8Array(2) });
  wrongVersion[2] = 2;
  assert.equal(mgmtReassembler().push(wrongVersion).reason, "bad-version");
  const wrongType = mgmtFrame({ payload: new Uint8Array(2) });
  wrongType[3] = 1;
  assert.equal(mgmtReassembler().push(wrongType).reason, "bad-type");

  // A payload longer than the advertised limit is refused up front.
  assert.equal(mgmtReassembler(8).push(mgmtFrame({ payload: new Uint8Array(9) })).reason, "bad-length");

  const reliable = reliableReassembler();
  assert.equal(reliable.push(reliableFrame({ sequence: 0, payload: new Uint8Array(2) })).reason, "bad-length");
  const wrongReliableVersion = reliableFrame({ payload: new Uint8Array(2) });
  wrongReliableVersion[2] = 9;
  assert.equal(reliableReassembler().push(wrongReliableVersion).reason, "bad-version");
});

test("extra continuation bytes are rejected instead of overflowing the frame", () => {
  const pending = mgmtReassembler();
  const frame = mgmtFrame({ payload: Uint8Array.from([1, 2, 3, 4]) });
  assert.equal(pending.push(frame.slice(0, 14)).status, "incomplete");
  // More payload than the header promised: the stream is no longer trustworthy.
  assert.equal(pending.push(Uint8Array.from([5, 6, 7, 8, 9])).reason, "oversized");
  assert.equal(pending.pending, false);
});

test("an orphaned continuation after a reset is rejected, not merged", () => {
  const pending = mgmtReassembler();
  const frame = mgmtFrame({ payload: Uint8Array.from([1, 2, 3, 4]) });
  assert.equal(pending.push(frame.slice(0, 14)).status, "incomplete");
  pending.reset();
  // A bare continuation no longer has a header, so it cannot be accepted.
  assert.equal(pending.push(Uint8Array.from([3, 4])).reason, "short-header");
  assert.equal(pending.pending, false);
});

test("the max payload limit is read per frame, not captured at construction", () => {
  let limit = 4;
  const pending = createFragmentReassembler({
    headerSize: 12, maxPayload: () => limit, parseHeader: parseManagementHeader,
  });
  assert.equal(pending.push(mgmtFrame({ payload: new Uint8Array(5) })).reason, "bad-length");
  limit = 8;
  assert.equal(pending.push(mgmtFrame({ payload: new Uint8Array(5) })).status, "complete");
});

test("every reject reason has a user-visible message", () => {
  for (const reason of ["short-header", "bad-magic", "bad-version", "bad-type", "bad-length", "oversized"]) {
    assert.equal(typeof MGMT_FRAME_ERRORS[reason], "string", reason);
  }
  for (const reason of ["short-header", "bad-magic", "bad-version", "bad-length", "oversized"]) {
    assert.equal(typeof RELIABLE_FRAME_ERRORS[reason], "string", reason);
  }
});
