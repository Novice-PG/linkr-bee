// Watch engine for target UART output.
//
// Everything here is an OBSERVATION of untrusted device text, never proof: a
// matched line says the target printed something that looks like a panic, a
// reboot or a user pattern. It does not say the target is broken, that a boot
// loop is still running, or that a command failed. Callers must show the raw
// evidence and let the user (or the assistant) judge it. No function here is
// named "detect" or "verify" for that reason.
//
// Pure by construction: no DOM, no timers, no ambient clock, no I/O. The caller
// feeds text or bytes and supplies the timestamp, so the engine is importable
// from Node and its behaviour is deterministic in tests.
//
// Input shaping: UART output arrives in arbitrary chunks. Only complete lines
// are evaluated; an unterminated tail is kept for the next call.
//
// Bounded memory (a target printing garbage at line rate cannot grow this
// engine without limit):
//   - MAX_LINE_CHARS = 1024   retained unfinished line (older characters are
//                             dropped; the buffer never exceeds this length)
//   - findings                 at most options.maxFindings (default 20, hard
//                             cap MAX_FINDINGS = 200), oldest dropped first
//   - de-duplication records   one per distinct finding, so an endless repeat
//                             of one panic line costs a single entry (bounded by
//                             the findings cap as well)
//   - MAX_PATTERNS = 64       extra literal patterns beyond the built-ins
//   - boot window              at most MAX_BOOT_HISTORY = 64 banner
//                             observations (times + evidence lines)
//   - MAX_EVIDENCE = 240      characters kept per finding's evidence line
//
// The line buffer, the per-pattern records and the findings list are the only
// structures that grow, and all three are capped.
//
// Repeated occurrences of the same thing do not append entries: they increment
// `count` and update `at`, so an endless panic storm costs nothing. A built-in
// panic rule collapses repeats of the same message; a caller pattern collapses
// repeats of its id.

const MAX_LINE_CHARS = 1024;
const MAX_FINDINGS = 200;
const MAX_PATTERNS = 64;
const MAX_BOOT_HISTORY = 64;
const MAX_EVIDENCE = 240;

// De-duplication key per retained finding, kept outside the finding objects so
// their shape stays exactly { kind, id, label, line, at, count, evidence }.
const DEDUP_KEYS = new WeakMap();

const DEFAULT_BOOT_LOOP_THRESHOLD = 3;
const DEFAULT_BOOT_LOOP_WINDOW_MS = 120000;
const MAX_BOOT_LOOP_THRESHOLD = 1000;
const MAX_BOOT_LOOP_WINDOW_MS = 3600000;

// Built-in panic-like rules, most specific first. Matching is a case-insensitive
// literal substring, exactly like user patterns: these strings are never
// compiled as regular expressions, so a device can echo a hostile pattern back
// without changing how the engine behaves. Order matters because a line is
// reported under one rule only: "watchdog: BUG: ..." must not also be reported
// as a plain "BUG: ".
export const PANIC_PATTERNS = Object.freeze([
  Object.freeze({ id: "watchdog-bug", text: "watchdog: BUG", label: "Watchdog reported a bug" }),
  Object.freeze({ id: "kernel-panic", text: "Kernel panic", label: "Kernel panic message" }),
  Object.freeze({ id: "oops", text: "Oops:", label: "Kernel oops report" }),
  Object.freeze({ id: "bug", text: "BUG: ", label: "Kernel BUG warning" }),
  Object.freeze({ id: "oom", text: "Out of memory", label: "Kernel out-of-memory report" }),
  Object.freeze({ id: "oom-kill", text: "oom-kill", label: "OOM killer invoked" }),
  Object.freeze({ id: "null-deref", text: "Unable to handle kernel NULL pointer dereference",
    label: "Kernel NULL pointer dereference" }),
  Object.freeze({ id: "segfault", text: "segfault at", label: "Userspace segmentation fault" }),
  Object.freeze({ id: "hardware-error", text: "Hardware Error", label: "Hardware error report" }),
  Object.freeze({ id: "cpu-warning", text: "WARNING: CPU", label: "Kernel CPU warning" }),
]);

// Boot banners reuse the vocabulary the console/executor helpers already treat
// as "the target restarted" (U-Boot and Linux version), plus the two common
// kernel handoff lines. Boot-loop findings are built only from these.
export const BOOT_PATTERNS = Object.freeze([
  Object.freeze({ id: "u-boot", text: "U-Boot ", label: "Bootloader banner" }),
  Object.freeze({ id: "linux-version", text: "Linux version ", label: "Linux kernel banner" }),
  Object.freeze({ id: "booting-linux", text: "Booting Linux", label: "Linux boot message" }),
  Object.freeze({ id: "starting-kernel", text: "Starting kernel", label: "Kernel handoff message" }),
]);

const BUILT_IN_RULES = Object.freeze([
  ...PANIC_PATTERNS.map((rule) => Object.freeze({ ...rule, kind: "panic" })),
  ...BOOT_PATTERNS.map((rule) => Object.freeze({ ...rule, kind: "boot" })),
]);

const BUILT_IN_IDS = new Set(BUILT_IN_RULES.map((rule) => rule.id));

// Built-in labels in Chinese. User-supplied labels are shown unchanged: the
// engine does not translate text it did not write.
export const PANIC_LABELS_ZH = Object.freeze({
  "kernel-panic": "内核崩溃信息",
  oops: "内核 oops 报告",
  bug: "内核 BUG 警告",
  oom: "内核内存耗尽报告",
  "oom-kill": "OOM 终止进程",
  "watchdog-bug": "看门狗 BUG 报告",
  "null-deref": "内核空指针解引用",
  segfault: "用户态段错误",
  "hardware-error": "硬件错误报告",
  "cpu-warning": "内核 CPU 警告",
  "u-boot": "引导程序启动横幅",
  "linux-version": "Linux 内核启动横幅",
  "booting-linux": "Linux 启动信息",
  "starting-kernel": "内核交接信息",
});

const SENTENCES = {
  en: {
    panic: (f) => `Observed ${f.label} at line ${f.line}: ${f.evidence}`,
    "boot-loop": (f) => `Observed ${f.label} ${f.count} times within the watch window; ` +
      `the target may be restarting before it settles: ${f.evidence}`,
    pattern: (f) => `Observed ${f.label} at line ${f.line}: ${f.evidence}`,
  },
  "zh-CN": {
    panic: (f) => `第 ${f.line} 行出现${f.label}：${f.evidence}`,
    "boot-loop": (f) => `观察窗口内出现 ${f.count} 次${f.label}，设备可能在稳定前反复重启：${f.evidence}`,
    pattern: (f) => `第 ${f.line} 行出现${f.label}：${f.evidence}`,
  },
};

function isChinese(lang) {
  return typeof lang === "string" && /^zh\b|^zh[-_]/i.test(lang.trim());
}

function boundedInt(value, fallback, min, max) {
  const number = Math.trunc(Number(value));
  if (!Number.isFinite(number)) return fallback;
  return Math.max(min, Math.min(max, number));
}

function clip(text) {
  return text.length > MAX_EVIDENCE ? text.slice(-MAX_EVIDENCE) : text;
}

function textOf(value) {
  if (typeof value === "string") return value;
  return value == null ? "" : String(value);
}

function normalizePatterns(patterns) {
  const rules = [];
  const seen = new Set(BUILT_IN_IDS);
  if (!Array.isArray(patterns)) return rules;
  for (const pattern of patterns) {
    if (rules.length >= MAX_PATTERNS) break;
    if (!pattern || typeof pattern !== "object") continue;
    const id = textOf(pattern.id).trim();
    const text = textOf(pattern.text);
    // Never treat caller input as a regular expression, and never let an empty
    // string match every line.
    if (!id || !text || seen.has(id)) continue;
    seen.add(id);
    rules.push(Object.freeze({ id, text, label: textOf(pattern.label).trim() || text,
      kind: "pattern", lower: text.toLowerCase() }));
  }
  return rules;
}

// Boot-loop bookkeeping: one shared window of banner observations
// (`times`/`banners`, bounded by MAX_BOOT_HISTORY), plus a per-rule
// `reported` flag so each banner rule reports a burst once.
function createBootLoop() {
  return { times: [], banners: [], bursts: new Map(), failed: false, lastAt: null };
}

function burstFor(state, ruleId) {
  let burst = state.bootLoop.bursts.get(ruleId);
  if (!burst) {
    burst = { reported: false };
    state.bootLoop.bursts.set(ruleId, burst);
  }
  return burst;
}

function matchRule(rule, lowerLine) {
  return lowerLine.includes(rule.lower ?? rule.text.toLowerCase());
}

// What makes two observations "the same finding":
//   - a caller pattern is identified by its id: one line per distinct pattern,
//     so repeats of the same id collapse into `count`;
//   - a built-in panic rule keeps its id but adds the offending message, so an
//     endless repeat of the same panic costs one entry while a genuinely
//     different BUG/panic line is new information.
// The key is never stored in the finding: it only drives de-duplication.
function dedupKey(rule, line) {
  if (rule.kind === "pattern") return `pattern:${rule.id}`;
  const text = line.trim();
  return `panic:${rule.id}:${text.length > MAX_EVIDENCE ? text.slice(0, MAX_EVIDENCE) : text}`;
}

function observation(kind, rule, line, at, evidence, count = 1) {
  return { kind, id: rule.id, label: rule.label, line, at, count, evidence };
}

function evaluateLine(state, rawline, at) {
  const line = rawline.endsWith("\r") ? rawline.slice(0, -1) : rawline;
  const at_ = typeof at === "number" && Number.isFinite(at) ? at : state.now();
  const evidence = clip(line);
  const lower = line.toLowerCase();
  const found = [];
  const matched = new Set();
  // Whether this line already produced a panic/pattern finding. Banner rules do
  // not set it: they report on their own rule and cannot duplicate another
  // rule's finding for the same line.
  let reported = false;
  const lineNumber = state.lines;
  for (const rule of state.rules) {
    if (matched.has(rule.id)) continue;
    if (!matchRule(rule, lower)) continue;
    matched.add(rule.id);
    if (rule.kind === "boot") {
      state.bootCount += 1;
      const loop = observeBoot(state, rule, lineNumber, at_, evidence);
      if (loop) found.push(loop);
      continue;
    }
    // A panic line between two banners means the target did not get far enough
    // to reboot on its own: those banners do not belong to one boot burst.
    state.bootLoop.failed = true;
    // "boot" is the internal marker for banner rules; everything else is either
    // a built-in panic rule or a user pattern. At most one panic finding per
    // line: a line matching several built-in rules is reported under the first
    // (most specific) one instead of raising several notifications.
    if (reported) continue;
    // A rule whose key is already retained returns null: it updated an existing
    // finding instead of producing a new one.
    const finding = observeRule(state, rule, lineNumber, at_, evidence);
    if (finding) {
      reported = true;
      found.push(finding);
    }
  }
  return found;
}

// Panic rules and user patterns: one retained record per de-duplication key,
// refreshed in place on every later occurrence of the same thing.
function observeRule(state, rule, lineNumber, at, evidence) {
  const kind = rule.kind === "pattern" ? "pattern" : "panic";
  const key = dedupKey(rule, evidence);
  const existing = findRecord(state, key);
  if (existing) {
    existing.count += 1;
    existing.at = at;
    return null;
  }
  const finding = observation(kind, rule, lineNumber, at, evidence);
  remember(state, finding, key);
  return finding;
}

// One window of boot banners across all banner rules, newest last. The window is
// trimmed in place, so it never exceeds MAX_BOOT_HISTORY observations.
function pushBanner(loop, at, text, windowMs) {
  loop.times.push(at);
  loop.banners.push(text);
  if (loop.times.length > MAX_BOOT_HISTORY) {
    loop.times.splice(0, loop.times.length - MAX_BOOT_HISTORY);
    loop.banners.splice(0, loop.banners.length - MAX_BOOT_HISTORY);
  }
  let dropped = false;
  // The observation that just arrived is never dropped, so the caller can
  // always decide about the window that ends at `at`.
  while (loop.times.length > 1 && at - loop.times[0] > windowMs) {
    loop.times.shift();
    loop.banners.shift();
    dropped = true;
  }
  return dropped;
}

// Boot banners: a device that prints boot banners over and over is observed as a
// possible boot loop. All banner rules share one window (a real boot burst mixes
// "U-Boot", "Linux version" and "Starting kernel"), but each rule reports under
// its own id and at most once per burst, so one rule's report cannot silence
// another's. A finding carries the banner lines that justify it.
function observeBoot(state, rule, lineNumber, at, evidence) {
  const loop = state.bootLoop;
  if (loop.failed) {
    // A panic sat between these banners, so they never formed one boot burst.
    loop.times.length = 0;
    loop.banners.length = 0;
    for (const burst of loop.bursts.values()) burst.reported = false;
    loop.failed = false;
  }
  // A gap longer than the window since the last banner of any kind starts a new
  // burst: every rule gets to report about it again.
  const stale = loop.lastAt !== null && at - loop.lastAt > state.bootLoopWindowMs;
  if (stale) for (const burst of loop.bursts.values()) burst.reported = false;
  loop.lastAt = at;
  const dropped = pushBanner(loop, at, `${evidence} (line ${lineNumber})`, state.bootLoopWindowMs);
  if (dropped) {
    // Banners fell out of the window, so the remaining ones are a new burst.
    for (const burst of loop.bursts.values()) burst.reported = false;
  }
  const burst = burstFor(state, rule.id);
  if (loop.times.length < state.bootLoopThreshold) return null;
  if (burst.reported) return null;
  const evidenceText = loop.banners.slice().reverse().join(" | ");
  const existing = findRecord(state, `boot-loop:${rule.id}`);
  if (existing) {
    // Same rule observed again in a later burst: refresh the retained record
    // instead of appending a second entry for the same id.
    existing.count += 1;
    existing.at = at;
    if (evidenceText !== existing.evidence) existing.evidence = clip(evidenceText);
    burst.reported = true;
    return null;
  }
  const finding = observation("boot-loop", rule, lineNumber, at, clip(evidenceText),
    loop.times.length);
  remember(state, finding, `boot-loop:${rule.id}`);
  burst.reported = true;
  return finding;
}

// De-duplication keys live beside the findings (a WeakMap) so the finding shape
// stays exactly the documented one and snapshot() remains plain JSON.
function findRecord(state, key) {
  for (let index = state.findings.length - 1; index >= 0; index--) {
    if (DEDUP_KEYS.get(state.findings[index]) === key) return state.findings[index];
  }
  return null;
}

function remember(state, finding, key) {
  DEDUP_KEYS.set(finding, key);
  state.findings.push(finding);
  if (state.findings.length > state.maxFindings) {
    state.findings.splice(0, state.findings.length - state.maxFindings);
  }
}

function feedText(state, text, at) {
  const produced = [];
  const stamped = typeof at === "number" && Number.isFinite(at) ? at : null;
  let start = 0;
  for (let index = 0; index < text.length; index++) {
    const char = text[index];
    if (char !== "\n" && char !== "\r") continue;
    // Treat CRLF as one terminator; a lone CR still ends a line.
    if (char === "\r" && text[index + 1] === "\n") continue;
    const line = (state.line + text.slice(start, index)).slice(-MAX_LINE_CHARS);
    state.line = "";
    start = index + 1;
    state.lines += 1;
    // A repeated rule returns null: it updated a retained finding instead of
    // producing a new one, and a line is never reported twice.
    for (const finding of evaluateLine(state, line, stamped ?? state.now())) {
      if (finding) produced.push(finding);
    }
  }
  if (start < text.length) {
    const rest = state.line + text.slice(start);
    // Keep a bounded tail: a device that never sends a newline cannot grow the
    // engine, and the cap is observable through snapshot().
    if (rest.length > MAX_LINE_CHARS) state.dropped += rest.length - MAX_LINE_CHARS;
    state.line = rest.slice(-MAX_LINE_CHARS);
  }
  return produced;
}

function createState(options, now) {
  const { patterns, bootLoop, maxFindings } = options;
  const threshold = boundedInt(bootLoop?.threshold, DEFAULT_BOOT_LOOP_THRESHOLD, 1, MAX_BOOT_LOOP_THRESHOLD);
  const windowMs = boundedInt(bootLoop?.windowMs, DEFAULT_BOOT_LOOP_WINDOW_MS, 1, MAX_BOOT_LOOP_WINDOW_MS);
  return {
    now,
    rules: Object.freeze([...BUILT_IN_RULES, ...normalizePatterns(patterns)]),
    maxFindings: boundedInt(maxFindings, 20, 1, MAX_FINDINGS),
    bootLoopThreshold: threshold,
    bootLoopWindowMs: windowMs,
    findings: [],
    line: "",
    lines: 0,
    dropped: 0,
    bootCount: 0,
    bootLoop: createBootLoop(),
  };
}

// One watch instance. `feed`/`feedBytes` return only the findings produced by
// that call; `findings()` returns everything retained, oldest first.
export function createSerialWatch({ patterns = [], now = () => Date.now(), bootLoop = {},
  maxFindings = 20 } = {}) {
  const clock = typeof now === "function" ? now : () => Date.now();
  let state = createState({ patterns, bootLoop, maxFindings }, clock);
  const decoder = new TextDecoder();

  return {
    // Feed decoded text. Incomplete lines are buffered across calls; only
    // complete lines are evaluated, and a completed line is evaluated once.
    feed(text, at = clock()) {
      return feedText(state, textOf(text), at);
    },

    // Feed raw bytes with the same streaming UTF-8 handling as
    // web/serial_journal.js, so a multi-byte character split across two UART
    // notifications is not turned into replacement characters.
    feedBytes(bytes, at = clock()) {
      if (!bytes || typeof bytes.length !== "number") return [];
      const text = decoder.decode(bytes, { stream: true });
      return text ? feedText(state, text, at) : [];
    },

    // Retained findings, oldest first. Returned as copies so callers cannot
    // mutate the engine's state.
    findings() {
      return state.findings.map((finding) => ({ ...finding }));
    },

    // Drop accumulated findings and buffered input, keep the counters and the
    // configured rules (a new device session on the same watch).
    clear() {
      state.findings.length = 0;
      state.line = "";
      state.dropped = 0;
      state.bootLoop = createBootLoop();
      return this.snapshot();
    },

    // Full engine reset: findings, counters and buffered input.
    reset() {
      state = createState({ patterns, bootLoop, maxFindings }, clock);
      return this.snapshot();
    },

    // Plain JSON view for the UI. `buffered` exposes the bounded line buffer so
    // bounded memory is assertable; `dropped` counts characters discarded from
    // an over-long unfinished line.
    snapshot() {
      return {
        findings: state.findings.map((finding) => ({ ...finding })),
        lines: state.lines,
        bootCount: state.bootCount,
        buffered: state.line.length,
        dropped: state.dropped,
      };
    },
  };
}

// One short status-line sentence per finding; findings are observations, so the
// wording stays "observed" / "出现". English by default, Chinese for zh-*.
export function describeFindings(findings, lang = "en") {
  const chinese = isChinese(lang);
  const sentences = chinese ? SENTENCES["zh-CN"] : SENTENCES.en;
  if (!Array.isArray(findings)) return [];
  return findings.map((finding) => {
    if (!finding || typeof finding !== "object") return "";
    const build = sentences[finding.kind] || sentences.pattern;
    const id = textOf(finding.id);
    const label = chinese && PANIC_LABELS_ZH[id] ? PANIC_LABELS_ZH[id]
      : textOf(finding.label) || id;
    return build({
      kind: finding.kind,
      id,
      label,
      line: Number.isFinite(finding.line) ? finding.line : 0,
      at: finding.at,
      count: Number.isFinite(finding.count) ? finding.count : 1,
      evidence: textOf(finding.evidence),
    });
  });
}

// Minimal convenience for a status line: is a boot-loop observation among these
// findings? An observation, not a verdict about the target.
export function isBootLoopHint(state) {
  const findings = Array.isArray(state) ? state : state?.findings;
  if (!Array.isArray(findings)) return false;
  return findings.some((finding) => finding?.kind === "boot-loop");
}
