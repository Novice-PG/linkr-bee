import assert from "node:assert/strict";
import test from "node:test";
import { BOOT_PATTERNS, PANIC_LABELS_ZH, PANIC_PATTERNS, createSerialWatch,
  describeFindings, isBootLoopHint } from "../../web/serial_watch.js";

const encode = (text) => new TextEncoder().encode(text);
const kinds = (findings) => findings.map((finding) => finding.kind);

// internal line-buffer cap documented in web/serial_watch.js
const LINE_CAP = 1024;

test("a panic split across three chunks produces one finding with the whole line", () => {
  const watch = createSerialWatch({ now: () => 7 });
  assert.deepEqual(watch.feed("boot ok\r\nKernel pa"), []);
  assert.deepEqual(watch.feed("nic - not syncing: VFS:"), []);
  const produced = watch.feed(" unable to mount root fs\r\nnext\r\n");
  assert.equal(produced.length, 1);
  const [finding] = produced;
  assert.equal(finding.kind, "panic");
  assert.equal(finding.id, "kernel-panic");
  assert.equal(finding.count, 1);
  assert.equal(finding.at, 7);
  assert.equal(finding.line, 2);
  assert.equal(finding.evidence, "Kernel panic - not syncing: VFS: unable to mount root fs");
  assert.ok(finding.label.length > 0);
  assert.deepEqual(watch.findings(), produced);
  assert.equal(watch.snapshot().lines, 3);
});

test("no line is ever reported twice, and old chunks are not re-scanned", () => {
  const watch = createSerialWatch();
  const line = "Kernel panic - not syncing: A\n";
  const first = watch.feed(line, 1);
  assert.equal(first.length, 1);
  // Feeding the same text again is a second observation, not a second entry.
  assert.deepEqual(watch.feed(line, 2), []);
  assert.equal(watch.findings().length, 1);
  assert.equal(watch.findings()[0].count, 2);
  assert.equal(watch.findings()[0].at, 2);
});

test("repeated identical panics increment count and keep one entry with fresh time", () => {
  const watch = createSerialWatch();
  watch.feed("Kernel panic - not syncing: A\n", 10);
  watch.feed("Kernel panic - not syncing: A\n", 20);
  watch.feed("Kernel panic - not syncing: A\n", 30);
  const findings = watch.findings();
  assert.equal(findings.length, 1);
  assert.equal(findings[0].count, 3);
  assert.equal(findings[0].at, 30);
  assert.equal(findings[0].line, 1);
  // A different panic is genuinely new information: its own entry.
  watch.feed("Out of memory: Killed process 42\n", 40);
  assert.deepEqual(kinds(watch.findings()), ["panic", "panic"]);
  assert.equal(watch.findings()[1].id, "oom");
});

test("every documented built-in panic pattern is observed with a human label", () => {
  const lines = [
    ["Kernel panic - not syncing: VFS", "kernel-panic"],
    ["Oops: 3 [#1] PREEMPT SMP", "oops"],
    ["BUG: unable to handle page fault", "bug"],
    ["Out of memory: Killed process 42", "oom"],
    ["oom-kill:constraint=CONSTRAINT_NONE", "oom-kill"],
    ["watchdog: BUG: soft lockup - CPU#0 stuck", "watchdog-bug"],
    ["Unable to handle kernel NULL pointer dereference at 00000000", "null-deref"],
    ["potato[123]: segfault at 0 ip 00000000 sp 00000000 error 4", "segfault"],
    ["[Hardware Error]: System Fatal error.", "hardware-error"],
    ["WARNING: CPU: 0 PID: 1 at kernel/sched/core.c:1", "cpu-warning"],
  ];
  const watch = createSerialWatch();
  watch.feed(lines.map(([line]) => line).join("\n") + "\n", 1);
  assert.deepEqual(watch.findings().map((finding) => finding.id), lines.map(([, id]) => id));
  for (const finding of watch.findings()) {
    assert.equal(finding.kind, "panic");
    assert.equal(finding.count, 1);
    assert.ok(finding.label.length > 5, `${finding.id} needs a readable label`);
    assert.equal(finding.evidence, lines.find(([, id]) => id === finding.id)[0]);
  }
  assert.equal(PANIC_PATTERNS.length, lines.length);
  for (const pattern of PANIC_PATTERNS) {
    assert.match(pattern.text, /[A-Za-z]/);
    assert.ok(PANIC_LABELS_ZH[pattern.id], `${pattern.id} needs a Chinese label`);
  }
});

test("boot banners are counted without producing a finding below the threshold", () => {
  const watch = createSerialWatch({ now: () => 0 });
  assert.deepEqual(watch.feed("U-Boot 2026.01 (Jan 01 2026)\n", 0), []);
  assert.deepEqual(watch.feed("Linux version 6.12.0 (gcc 14)\n", 30000), []);
  const state = watch.snapshot();
  assert.equal(state.bootCount, 2);
  assert.equal(state.findings.length, 0);
  assert.equal(isBootLoopHint(state), false);
  assert.equal(BOOT_PATTERNS.length, 4);
});

test("three banners inside the window yield one boot-loop finding with count 3", () => {
  const watch = createSerialWatch({ now: () => 0 });
  watch.feed("U-Boot 2026.01\n", 0);
  watch.feed("Linux version 6.12.0\n", 30000);
  const produced = watch.feed("U-Boot 2026.01\n", 60000);
  assert.equal(produced.length, 1);
  const [finding] = produced;
  assert.equal(finding.kind, "boot-loop");
  assert.equal(finding.id, "u-boot");
  assert.equal(finding.count, 3);
  assert.equal(finding.at, 60000);
  assert.equal(finding.line, 3);
  // Evidence is the actual banner lines that justify the finding, newest first.
  const evidence = finding.evidence.split(" | ");
  assert.equal(evidence.length, 3);
  assert.deepEqual(evidence, [
    "U-Boot 2026.01 (line 3)",
    "Linux version 6.12.0 (line 2)",
    "U-Boot 2026.01 (line 1)",
  ]);
  // The burst is reported once: a fourth banner in the same burst adds nothing.
  assert.deepEqual(watch.feed("U-Boot 2026.01\n", 70000), []);
  assert.equal(watch.findings().length, 1);
  assert.equal(watch.findings()[0].count, 3);
  assert.equal(isBootLoopHint(watch.snapshot()), true);
});
test("banners spread outside the window never form a boot-loop finding", () => {
  const watch = createSerialWatch({ now: () => 0 });
  watch.feed("U-Boot 2026.01\n", 0);
  watch.feed("U-Boot 2026.01\n", 200000);
  watch.feed("U-Boot 2026.01\n", 400000);
  assert.deepEqual(watch.findings(), []);
  assert.equal(watch.snapshot().bootCount, 3);
  // Still nothing once a fourth spaced-out banner arrives.
  watch.feed("U-Boot 2026.01\n", 600000);
  assert.deepEqual(watch.findings(), []);
});

test("boot-loop threshold and window are configurable and clamped", () => {
  const watch = createSerialWatch({ now: () => 0, bootLoop: { threshold: 2, windowMs: 5000 } });
  watch.feed("Starting kernel ...\n", 0);
  const produced = watch.feed("Starting kernel ...\n", 4000);
  assert.equal(produced.length, 1);
  assert.equal(produced[0].kind, "boot-loop");
  assert.equal(produced[0].count, 2);
  // A higher threshold with the same in-window banners reports nothing.
  const explicit = createSerialWatch({ bootLoop: { threshold: 4 } });
  explicit.feed("Booting Linux\n", 0);
  explicit.feed("Booting Linux\n", 1000);
  explicit.feed("Booting Linux\n", 2000);
  assert.deepEqual(explicit.findings(), []);
  assert.equal(explicit.snapshot().bootCount, 3);
  assert.equal(explicit.feed("Booting Linux\n", 3000)[0].count, 4);
});

test("a panic between banners prevents counting them as one boot burst", () => {
  const watch = createSerialWatch({ now: () => 0 });
  watch.feed("U-Boot 2026.01\n", 0);
  watch.feed("Kernel panic - not syncing: bad rootfs\n", 1000);
  watch.feed("U-Boot 2026.01\n", 2000);
  assert.deepEqual(watch.findings().map((finding) => finding.kind), ["panic"]);
  // Two banners after the crash are needed again to reach the threshold of 3.
  watch.feed("U-Boot 2026.01\n", 3000);
  const produced = watch.feed("U-Boot 2026.01\n", 4000);
  assert.equal(produced.length, 1);
  assert.equal(produced[0].kind, "boot-loop");
  assert.equal(produced[0].count, 3);
});

test("custom patterns match literal substrings, case-insensitively", () => {
  const watch = createSerialWatch({ now: () => 0, patterns: [
    { id: "ready", text: "system ready", label: "App reported ready" },
  ] });
  const produced = watch.feed("2026 SYSTEM READY in 3s\n", 1);
  assert.equal(produced.length, 1);
  assert.equal(produced[0].kind, "pattern");
  assert.equal(produced[0].id, "ready");
  assert.equal(produced[0].label, "App reported ready");
  assert.equal(produced[0].evidence, "2026 SYSTEM READY in 3s");
  // Without an explicit label the pattern text is the label.
  const bare = createSerialWatch({ patterns: [{ id: "bare", text: "watchdog tripped" }] });
  assert.equal(bare.feed("WATCHDOG TRIPPED\n", 2)[0].label, "watchdog tripped");
});

test("a regex-looking pattern is treated literally, never compiled", () => {
  const watch = createSerialWatch({ now: () => 0, patterns: [
    { id: "literal", text: "boot\\.\\.\\./^a+$" },
  ] });
  const produced = watch.feed("prefix BOOT\\.\\.\\./^A+$ suffix\n", 1);
  assert.equal(produced.length, 1);
  assert.equal(produced[0].id, "literal");
  // The literal text does not appear here, so a regex engine must not match it.
  assert.deepEqual(watch.feed("bootaaa\nBOOT\n", 2), []);
  assert.equal(watch.findings().length, 1);
});

test("a pattern cannot be duplicated by a built-in id or an empty text", () => {
  const watch = createSerialWatch({ now: () => 0, patterns: [
    { id: "kernel-panic", text: "custom text" },
    { id: "empty", text: "" },
  ] });
  watch.feed("Kernel panic - not syncing: A\ncustom text here\n", 1);
  const findings = watch.findings();
  assert.equal(findings.length, 1);
  assert.equal(findings[0].kind, "panic");
  assert.equal(findings[0].id, "kernel-panic");
  assert.equal(findings[0].label, "Kernel panic message");
});

test("feedBytes decodes UTF-8 split across chunk boundaries", () => {
  const watch = createSerialWatch({ now: () => 0 });
  const bytes = encode("正常启动\n内核乱码 Kernel panic - not syncing: 内存不足\n");
  for (const byte of bytes) watch.feedBytes(Uint8Array.of(byte), 5);
  const findings = watch.findings();
  assert.equal(findings.length, 1);
  assert.equal(findings[0].kind, "panic");
  assert.equal(findings[0].evidence, "内核乱码 Kernel panic - not syncing: 内存不足");
  assert.ok(!findings[0].evidence.includes("\uFFFD"), "split characters must not become replacement chars");
  assert.equal(watch.snapshot().lines, 2);

  // A single call with the same bytes observes the same thing.
  const whole = createSerialWatch();
  whole.feedBytes(bytes, 5);
  assert.deepEqual(whole.findings(), findings);
});

test("feedBytes tolerates arbitrary chunk shapes", () => {
  const watch = createSerialWatch();
  const bytes = encode("你好 Out of memory: Killed process 7\n");
  assert.deepEqual(watch.feedBytes(bytes.slice(0, 2), 1), []);
  assert.deepEqual(watch.feedBytes(bytes.slice(2, 5), 2), []);
  const produced = watch.feedBytes(bytes.slice(5), 3);
  assert.equal(produced.length, 1);
  assert.equal(produced[0].evidence, "你好 Out of memory: Killed process 7");
  assert.deepEqual(watch.feedBytes(new Uint8Array(0), 4), []);
  assert.deepEqual(watch.feedBytes(null, 5), []);
});

test("the findings list is capped and drops the oldest entries", () => {
  const watch = createSerialWatch({ now: () => 0, maxFindings: 3 });
  const lines = ["Kernel panic - not syncing: A", "Oops: 1 [#1] SMP", "BUG: soft lockup",
    "Out of memory: Killed process 1", "segfault at 0", "Hardware Error: x"];
  for (const [index, line] of lines.entries()) watch.feed(`${line}\n`, index);
  const retained = watch.findings();
  assert.equal(retained.length, 3);
  assert.deepEqual(retained.map((finding) => finding.id), ["oom", "segfault", "hardware-error"]);
  assert.deepEqual(retained.map((finding) => finding.count), [1, 1, 1]);
  // A repeat of a dropped id is new information again, not a silent increment.
  const produced = watch.feed("Kernel panic - not syncing: A\n", 9);
  assert.equal(produced.length, 1);
  assert.equal(produced[0].count, 1);
  assert.equal(watch.findings().length, 3);
  // The hard cap bounds a caller that asks for more.
  const greedy = createSerialWatch({ maxFindings: 100000 });
  for (let index = 0; index < 260; index++) greedy.feed(`BUG: report ${index}\n`, index);
  assert.equal(greedy.findings().length, 200);
});

test("the line buffer stays bounded under a flood of partial lines", () => {
  const watch = createSerialWatch();
  const chunk = "x".repeat(997) + "\n";
  for (let index = 0; index < 500; index++) watch.feed(chunk, index);
  assert.equal(watch.snapshot().buffered, 0);
  assert.equal(watch.snapshot().lines, 500);
  for (let index = 0; index < 200; index++) watch.feed("y".repeat(4096), index);
  const flooded = watch.snapshot();
  assert.equal(flooded.buffered, LINE_CAP);
  assert.ok(flooded.dropped >= 200 * (4096 - LINE_CAP));
  assert.equal(flooded.lines, 500);
  // Bounded even when every chunk ends inside a multi-byte character.
  const bytes = encode("内".repeat(4096));
  for (let index = 0; index < 50; index++) watch.feedBytes(bytes, index);
  assert.equal(watch.snapshot().buffered, LINE_CAP);
  // The buffered tail is discarded when its line terminates.
  const produced = watch.feed("\n", 1);
  assert.deepEqual(produced, []);
  assert.equal(watch.snapshot().buffered, 0);
});

test("one line that is longer than the cap still reports its terminated tail", () => {
  const watch = createSerialWatch();
  watch.feed("A".repeat(5000) + " Kernel panic - not syncing: late\n", 1);
  assert.equal(watch.snapshot().buffered, 0);
  const findings = watch.findings();
  assert.equal(findings.length, 1);
  assert.equal(findings[0].id, "kernel-panic");
  assert.equal(findings[0].evidence.length, 240);
  assert.ok(findings[0].evidence.endsWith("Kernel panic - not syncing: late"));
});

test("clear and reset drop state without touching the configured rules", () => {
  const watch = createSerialWatch({ patterns: [{ id: "ready", text: "ready" }], bootLoop: { threshold: 2 } });
  watch.feed("Kernel panic - not syncing: A\nready now\nU-Boot 1\n", 5);
  assert.equal(watch.snapshot().bootCount, 1);
  const cleared = watch.clear();
  assert.deepEqual(cleared.findings, []);
  // Counters describe the whole session, so clear() keeps them; reset() does not.
  assert.equal(cleared.bootCount, 1);
  assert.equal(cleared.lines, 3);
  assert.equal(cleared.buffered, 0);
  // Rules and counters survive clear(): the panic and the pattern are observed
  // again, and the two U-Boot lines after them form their own boot burst.
  assert.equal(watch.feed("ready now\n", 6).length, 1);
  assert.equal(watch.feed("Kernel panic - not syncing: A\n", 7).length, 1);
  assert.equal(watch.snapshot().bootCount, 1);
  const reset = watch.reset();
  assert.deepEqual(reset, { findings: [], lines: 0, bootCount: 0, buffered: 0, dropped: 0 });
  // A reset watch keeps the caller's patterns and boot-loop settings.
  assert.deepEqual(watch.feed("U-Boot 1\n", 8), []);
  assert.equal(watch.feed("U-Boot 1\n", 9)[0].kind, "boot-loop");
  assert.equal(watch.feed("ready now\n", 10).length, 1);
  assert.equal(watch.snapshot().bootCount, 2);
});

test("the engine never reads an ambient clock", () => {
  let calls = 0;
  const watch = createSerialWatch({ now: () => { calls += 1; return 42; } });
  watch.feed("Kernel panic - not syncing: A\n");
  assert.equal(calls, 1);
  assert.equal(watch.findings()[0].at, 42);
  // An explicit timestamp must not call the clock at all.
  const fixed = createSerialWatch({ now: () => { throw new Error("clock used"); } });
  fixed.feed("Kernel panic - not syncing: A\n", 3);
  fixed.feedBytes(encode("Oops: 1 [#1] SMP\n"), 4);
  assert.deepEqual(fixed.findings().map((finding) => finding.at), [3, 4]);
});

test("findings and snapshot return copies a caller cannot use to corrupt state", () => {
  const watch = createSerialWatch();
  watch.feed("Kernel panic - not syncing: A\n", 1);
  const [finding] = watch.findings();
  finding.count = 999;
  finding.evidence = "tampered";
  watch.snapshot().findings[0].label = "tampered";
  const fresh = watch.findings()[0];
  assert.equal(fresh.count, 1);
  assert.equal(fresh.evidence, "Kernel panic - not syncing: A");
  assert.equal(fresh.label, "Kernel panic message");
});

test("describeFindings writes one readable sentence per finding in both languages", () => {
  const watch = createSerialWatch({ now: () => 0, patterns: [{ id: "ready", text: "ready", label: "Ready marker" }] });
  watch.feed("Kernel panic - not syncing: VFS\nready now\n", 1000);
  const findings = [...watch.findings()];
  const bootWatch = createSerialWatch({ now: () => 0 });
  bootWatch.feed("U-Boot 1\n", 2000);
  bootWatch.feed("U-Boot 1\n", 3000);
  bootWatch.feed("U-Boot 1\n", 4000);
  findings.push(...bootWatch.findings());
  assert.deepEqual(kinds(findings), ["panic", "pattern", "boot-loop"]);

  const english = describeFindings(findings);
  assert.equal(english.length, 3);
  assert.equal(english[0], "Observed Kernel panic message at line 1: Kernel panic - not syncing: VFS");
  assert.equal(english[1], "Observed Ready marker at line 2: ready now");
  assert.equal(english[2], "Observed Bootloader banner 3 times within the watch window; " +
    "the target may be restarting before it settles: U-Boot 1 (line 3) | U-Boot 1 (line 2) | " +
    "U-Boot 1 (line 1)");
  for (const sentence of english) {
    assert.equal(typeof sentence, "string");
    assert.ok(sentence.length > 10);
    assert.ok(!sentence.includes("\n"));
  }

  const chinese = describeFindings(findings, "zh-CN");
  assert.equal(chinese.length, 3);
  assert.equal(chinese[0], "第 1 行出现内核崩溃信息：Kernel panic - not syncing: VFS");
  assert.equal(chinese[1], "第 2 行出现Ready marker：ready now");
  assert.ok(chinese[2].includes("观察窗口内出现 3 次引导程序启动横幅"));
  assert.ok(chinese[2].includes("U-Boot 1 (line 3)"));
  for (const sentence of chinese) assert.ok(/[\u4e00-\u9fff]/.test(sentence));
  // Unknown languages fall back to English, and junk input never throws.
  assert.deepEqual(describeFindings(findings, "fr"), english);
  assert.deepEqual(describeFindings(null), []);
  assert.deepEqual(describeFindings([null, 7]), ["", ""]);
});

test("isBootLoopHint only reports a boot-loop observation", () => {
  const watch = createSerialWatch({ bootLoop: { threshold: 2 } });
  assert.equal(isBootLoopHint(watch.snapshot()), false);
  assert.equal(isBootLoopHint(watch.findings()), false);
  assert.equal(isBootLoopHint(undefined), false);
  watch.feed("Linux version 6.12.0\nU-Boot 2026.01\n", 0);
  assert.equal(isBootLoopHint(watch.findings()), true);
  assert.equal(isBootLoopHint(watch.snapshot()), true);
  // A panic is not a boot-loop hint.
  const panicOnly = createSerialWatch();
  panicOnly.feed("Kernel panic - not syncing: A\n", 0);
  assert.equal(isBootLoopHint(panicOnly.findings()), false);
});
