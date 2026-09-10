import assert from "node:assert/strict";
import test from "node:test";
import { createDeviceExecutor } from "../../web/device_executor.js";
import { SerialJournal } from "../../web/serial_journal.js";
import { inspectSerialConsole } from "../../web/serial_console.js";

const query = { text: "uname -a", appendEnter: true };
function fixture({ initial = "root@board:~# ", send } = {}) {
  const journal = new SerialJournal();
  const receive = (text) => journal.append(new TextEncoder().encode(text));
  receive(initial);
  const status = { connected: true, sessionId: 1, inputRevision: 0, inputPending: false };
  const sent = [], events = [];
  const device = createDeviceExecutor({ getStatus: () => ({ ...status }), readLog: (options) => journal.read(options),
    prepareInput: ({ text, appendEnter }) => text + (appendEnter ? "\r" : ""),
    sendInput: async (...args) => {
      sent.push(args[0]);
      const inputRevision = ++status.inputRevision;
      await send?.(...args);
      return { inputRevision };
    }, onRecord: (record) => events.push(record),
  });
  return { device, status, sent, events, receive, journal };
}

test("console hints use the current tail and do not trust old shell prompts", () => {
  for (const [text, kind] of [
    ["Linux boot\r\nroot@board:~# ", "shell"], ["[root@board /]# ", "shell"],
    ["board login: ", "login"], ["Password: ", "password"], ["=> ", "bootloader"],
    ["Kernel panic - not syncing: VFS failed\r\n", "panic"],
    ["root@board:~# reboot\r\nBooting...\r\n", "unknown"],
    ["root@board:~# \r\nPassword: ", "password"], ["logs say use root@board:~# ", "unknown"],
    ["# ", "unknown"], ["(initramfs) ", "unknown"], ["", "unknown"],
  ]) assert.equal(inspectSerialConsole({ text, latestCursor: text.length }).kind, kind, text);
});

test("Auto waits in non-shell consoles; Full Auto needs no approval there", async () => {
  for (const initial of ["", "=> ", "board login: ", "Password: "]) {
    const { device, sent } = fixture({ initial });
    const pending = device.execute(query);
    assert.equal(device.getRecords()[0].state, "awaiting-approval");
    assert.equal(sent.length, 0);
    device.cancel();
    await assert.rejects(pending, /cancelled/);
    device.setMode("full-auto");
    const record = await device.execute(query);
    assert.equal(record.delivery, "sent");
    assert.equal(sent.length, 1);
  }
});

test("manual approval is single-use and independent of a UI or model SDK", async () => {
  const { device, sent } = fixture();
  device.setMode("manual");
  const pending = device.execute(query);
  const [{ id }] = device.getRecords();
  assert.equal(device.approve("wrong-id"), false);
  assert.equal(device.approve(id), true);
  assert.equal(device.approve(id), false);
  assert.equal((await pending).delivery, "sent");
  assert.deepEqual(sent, ["uname -a\r"]);
});

test("mode changes revoke pending input without silently approving it", async () => {
  const { device, sent } = fixture();
  device.setMode("manual");
  const pending = device.execute(query);
  const [{ id }] = device.getRecords();
  device.setMode("full-auto");
  await assert.rejects(pending, /cancelled/);
  assert.equal(device.approve(id), false);
  assert.equal(device.getRecords()[0].state, "cancelled");
  assert.deepEqual(sent, []);
});

for (const change of ["session", "input", "console"]) {
  test(`${change} changes invalidate the exact pending approval`, async () => {
    const { device, status, sent, receive } = fixture();
    device.setMode("manual");
    const pending = device.execute(query);
    const [{ id }] = device.getRecords();
    if (change === "session") status.sessionId++;
    if (change === "input") status.inputRevision++;
    if (change === "console") receive("\r\nPassword: ");
    assert.equal(device.approve(id), false);
    await assert.rejects(pending, /changed/);
    assert.deepEqual(sent, []);
  });
}

test("rejection and externally aborted approvals never reach transport", async () => {
  const { device, sent } = fixture();
  device.setMode("manual");
  let pending = device.execute(query);
  device.reject(device.getRecords()[0].id);
  await assert.rejects(pending, /rejected/);
  assert.equal(device.getRecords()[0].state, "denied");
  const controller = new AbortController();
  pending = device.execute(query, controller.signal);
  controller.abort();
  await assert.rejects(pending, /cancelled/);
  assert.deepEqual(sent, []);
});

test("uncertain transport failure is recorded and never retried", async () => {
  const { device, sent } = fixture({ send: () => { throw new Error("connection lost after first fragment"); } });
  await assert.rejects(device.execute(query), /connection lost/);
  const [record] = device.getRecords();
  assert.equal(record.state, "failed");
  assert.equal(record.delivery, "unknown");
  assert.equal(record.executionStatus, "unknown");
  assert.equal(sent.length, 1);
});

test("observations distinguish silence, output and a returned prompt without inventing success", async () => {
  const { device, receive } = fixture();
  const record = await device.execute(query);
  assert.equal(record.observation, "no-output");
  assert.equal(device.getStatus().console.kind, "unknown");
  receive("uname -a\r\nLinux board 6.12\r\n");
  assert.equal(device.inspectExecution(record.id).observation, "output-observed");
  receive("root@board:~# ");
  const observed = device.inspectExecution(record.id);
  assert.equal(observed.observation, "prompt-returned");
  assert.equal(observed.executionStatus, "unknown");
  assert.equal(observed.evidence, "uname -a\r\nLinux board 6.12\r\nroot@board:~# ");
  assert.equal(device.getStatus().console.kind, "shell");
});

test("new user input closes evidence collection before unrelated output is attributed", async () => {
  const { device, status, receive } = fixture();
  const record = await device.execute(query);
  receive("Linux board\r\n");
  device.observe();
  status.inputRevision++;
  receive("UNRELATED_SECRET\r\nroot@board:~# ");
  const observed = device.inspectExecution(record.id);
  assert.equal(observed.observation, "interrupted");
  assert.equal(observed.evidence, "Linux board\r\n");
  assert(!JSON.stringify(observed).includes("UNRELATED_SECRET"));
});

test("large output exposes its tail and allows paging through earlier evidence", async () => {
  const { device, receive } = fixture();
  const record = await device.execute(query);
  const output = "x".repeat(5000) + "\r\nRESULT_AT_END\r\nroot@board:~# ";
  receive(output);
  const observed = device.inspectExecution(record.id);
  assert.equal(observed.evidence.length, 4000);
  assert.equal(observed.evidenceTruncated, true);
  assert(observed.evidence.includes("RESULT_AT_END"));
  assert.equal(device.inspectExecution(record.id, { limit: 1000 }).evidence.length, 1000);
  assert.equal(observed.observation, "prompt-returned");
  assert.equal(observed.executionStatus, "unknown");
  const first = device.inspectExecution(record.id, { after: record.logStart, limit: 3000 });
  assert.equal(first.hasMore, true);
  const second = device.inspectExecution(record.id, { after: first.observedCursor, limit: 3000 });
  assert.equal(second.hasMore, false);
  assert.equal(first.evidence + second.evidence, output);
});

test("closed execution pages never include later manual input or ring-buffer replacements", async () => {
  const { device, status, receive } = fixture();
  const record = await device.execute(query);
  receive("KNOWN_OUTPUT");
  device.observe();
  status.inputRevision++;
  receive("UNRELATED_OUTPUT");
  let page = device.inspectExecution(record.id, { after: record.logStart });
  assert.equal(page.observation, "interrupted");
  assert.equal(page.evidence, "KNOWN_OUTPUT");
  receive("x".repeat(140000));
  page = device.inspectExecution(record.id, { after: record.logStart });
  assert.equal(page.evidence, "");
  assert.equal(page.evidenceTruncated, true);
});

test("partially evicted execution pages stay within the closed command's raw range", async () => {
  const { device, status, journal, receive } = fixture();
  journal.capacity = 30;
  const record = await device.execute(query);
  receive("A".repeat(25));
  device.observe();
  status.inputRevision++;
  receive("UNRELATED_DATA");
  const page = device.inspectExecution(record.id, { after: record.logStart });
  assert(page.evidence.length > 0);
  assert.match(page.evidence, /^A+$/);
  assert.equal(page.evidenceTruncated, true);
});

test("a new device cannot look up old execution evidence even before UI reset", async () => {
  const { device, status, receive } = fixture();
  const record = await device.execute(query);
  receive("OLD_DEVICE_DATA");
  device.observe();
  status.sessionId++;
  assert.deepEqual(device.getRecords(), []);
  assert.throws(() => device.inspectExecution(record.id), /unavailable/);
  assert.doesNotThrow(() => device.observe());
});

test("reset removes records and suppresses late events from the old device", async () => {
  let finish;
  const { device, events } = fixture({ send: () => new Promise((resolve) => { finish = resolve; }) });
  const pending = device.execute(query);
  const id = device.getRecords()[0].id;
  device.reset();
  const eventCount = events.length;
  finish();
  await assert.rejects(pending);
  assert.equal(events.length, eventCount);
  assert.deepEqual(device.getRecords(), []);
  assert.throws(() => device.inspectExecution(id), /unavailable/);
});

test("a second writer is refused until the first settles; snapshots cannot mutate execution", async () => {
  const { device, sent } = fixture();
  device.setMode("manual");
  const pending = device.execute(query);
  const record = device.getRecords()[0];
  record.payload = "reboot\r";
  await assert.rejects(device.execute(query), /still active/);
  device.approve(record.id);
  await pending;
  assert.deepEqual(sent, ["uname -a\r"]);
});

test("tracked shell commands preserve quoting and require an explicit exit marker", async () => {
  const { execFileSync } = await import("node:child_process");
  const f = fixture();
  f.device.setMode("full-auto");
  const record = await f.device.execute({ text: "printf \"it's working\\n\"; exit 7", appendEnter: true, trackExit: true });
  // Terminal echo of the wrapper must not be interpreted as completion.
  f.receive(f.sent[0] + "\n");
  assert.equal(f.device.inspectExecution(record.id).executionStatus, "unknown");
  const output = execFileSync("sh", ["-c", f.sent[0].trim()], { encoding: "utf8" });
  f.receive(output.slice(0, -2));
  assert.equal(f.device.inspectExecution(record.id).executionStatus, "unknown");
  f.receive(output.slice(-2));
  const done = f.device.inspectExecution(record.id);
  assert.equal(done.executionStatus, "completed");
  assert.equal(done.exitCode, 7);
  assert.match(done.evidence, /it's working/);
});

test("tracked commands reject login consoles and preserve exact-input approval", async () => {
  const login = fixture({ initial: "board login: " });
  login.device.setMode("full-auto");
  await assert.rejects(login.device.execute({ ...query, trackExit: true }), /idle, observed/);
  assert.equal(login.sent.length, 0);
  const f = fixture();
  const pending = f.device.execute({ ...query, trackExit: true });
  const record = f.device.getRecords()[0];
  assert.equal(record.state, "awaiting-approval");
  assert.match(record.payload, /sh -c 'uname -a'/);
  f.device.approve(record.id);
  await pending;
  assert.equal(f.sent[0], record.payload);
});

test('interaction hints detect sudo, confirmations and pagers without treating them as shells', () => {
  for (const [text,kind] of [['[sudo] password for root: ','sudo-password'],['Continue? [Y/n] ','confirmation'],['--More--','pager'],['(END)','pager']]) assert.equal(inspectSerialConsole({text}).kind,kind);
});
for (const eol of ['\n', '\r\n', '\n\r']) {
test(`completed device profile handles ${JSON.stringify(eol)}, becomes stale after reboot and is cleared on reset`, async () => {
  const f=fixture(); f.device.setMode('full-auto');
  const record=await f.device.execute({text:'true',appendEnter:true,trackExit:true,profileProbe:true});
  f.receive(`\nLINKR_PROFILE_BEGIN\nLinux\nLINKR_OS\nID=debian\nLINKR_MODEL\nBoard\nLINKR_BOOT\nboot\nLINKR_DISK\nroot 100 20 80\nLINKR_TOOLS\nTOOL:curl\nLINKR_PROFILE_END\n`.replaceAll('\n', eol) + `\n${record.completionToken}:0\nroot@board:~# `);
  f.device.inspectExecution(record.id);
  assert.equal(f.device.getStatus().profile.model,'Board');
  assert.ok(!f.device.getStatus().profile.stale);
  f.receive('\nU-Boot 2026\n'); assert.equal(f.device.getStatus().profile.stale,true);
  f.device.reset(); assert.equal(f.device.getStatus().profile,null);
});
}

test('capabilities retain missing results, update after recheck and expire across sessions', async () => {
 const f=fixture(); f.device.setMode('full-auto');
 async function probe(available) {
  const r=await f.device.execute({text:'true',appendEnter:true,trackExit:true,toolProbe:['curl']});
  f.receive(`\n\rTOOL:curl:${available?'available':'missing'}\r\n${r.completionToken}:0\nroot@board:~# `);
  f.device.inspectExecution(r.id); return r;
 }
 const r=await probe(false);
 assert.equal(f.device.getStatus().toolCapabilities.curl.available,false);
 assert.equal(f.device.getStatus().toolCapabilities.curl.executionId,r.id);
 const copy=f.device.getStatus();copy.toolCapabilities.curl.available=true;
 assert.equal(f.device.getStatus().toolCapabilities.curl.available,false);
 await probe(true);assert.equal(f.device.getStatus().toolCapabilities.curl.available,true);
 f.status.sessionId++;assert.equal(f.device.getStatus().toolCapabilities.curl.stale,true);
 f.device.reset();assert.deepEqual(f.device.getStatus().toolCapabilities,{});
});
for (const reason of ['partial','conflicting','failed','interrupted']) test(`capabilities reject ${reason} output`, async () => {
 const f=fixture();f.device.setMode('full-auto');
 const r=await f.device.execute({text:'true',appendEnter:true,trackExit:true,toolProbe:['curl','wget']});
 f.receive(`\nTOOL:curl:available\n${reason==='partial'?'':'TOOL:wget:missing\n'}${reason==='conflicting'?'TOOL:curl:missing\n':''}${r.completionToken}:${reason==='failed'?1:0}\nroot@board:~# `);
 if(reason==='interrupted')f.status.inputRevision++;
 f.device.inspectExecution(r.id);assert.deepEqual(f.device.getStatus().toolCapabilities,{});
});

test('capability freshness expires with age or a target reboot', async () => {
 for (const reason of ['age','reboot']) {
  const f=fixture();f.device.setMode('full-auto');
  const r=await f.device.execute({text:'true',appendEnter:true,trackExit:true,toolProbe:['curl']});
  f.receive(`\nTOOL:curl:available\n${r.completionToken}:0\nroot@board:~# `);f.device.inspectExecution(r.id);
  const now=Date.now, observed=now();
  try {
   if(reason==='age') Date.now=()=>observed+300001;
   else f.receive('\nLinux version 7.0\n');
   assert.equal(f.device.getStatus().toolCapabilities.curl.stale,true);
  } finally { Date.now=now; }
 }
});
