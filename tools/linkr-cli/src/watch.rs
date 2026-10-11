//! Serial watch engine: panic/boot/pattern findings over untrusted output.
//! Port of web/serial_watch.js — observations, never proofs.
//!
//! Matching is a case-insensitive literal substring, never a regular
//! expression: a device can echo a hostile pattern back without changing how
//! the engine behaves. Repeated occurrences increment `count` instead of
//! appending entries, so a panic storm costs one finding.

use std::collections::HashMap;

/// Retained unfinished line cap (older characters are dropped).
pub const MAX_LINE_CHARS: usize = 1024;
/// Hard cap on retained findings (default is 20).
pub const MAX_FINDINGS: usize = 200;
/// Extra literal patterns beyond the built-ins.
pub const MAX_PATTERNS: usize = 64;
/// Boot banner observations kept in the boot-loop window.
pub const MAX_BOOT_HISTORY: usize = 64;
/// Characters kept per finding's evidence line.
pub const MAX_EVIDENCE: usize = 240;

pub const DEFAULT_BOOT_LOOP_THRESHOLD: u32 = 3;
pub const DEFAULT_BOOT_LOOP_WINDOW_MS: u64 = 120_000;
const MAX_BOOT_LOOP_THRESHOLD: u32 = 1000;
const MAX_BOOT_LOOP_WINDOW_MS: u64 = 3_600_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindingKind {
    Panic,
    BootLoop,
    Pattern,
}

impl FindingKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FindingKind::Panic => "panic",
            FindingKind::BootLoop => "boot-loop",
            FindingKind::Pattern => "pattern",
        }
    }
}

/// `line` is the 1-based line counter (JS `line`), `evidence` is the offending
/// line clipped to `MAX_EVIDENCE`; `at_ms` is the caller clock (JS `at`).
#[derive(Debug, Clone)]
pub struct Finding {
    pub kind: FindingKind,
    pub id: String,
    pub label: String,
    pub line: u64,
    pub at_ms: u64,
    pub count: u32,
    pub evidence: String,
}

#[derive(Debug, Clone)]
pub struct WatchOptions {
    pub max_findings: usize,
    pub boot_threshold: u32,
    pub boot_window_ms: u64,
}

impl Default for WatchOptions {
    fn default() -> Self {
        Self {
            max_findings: 20,
            boot_threshold: 3,
            boot_window_ms: 120_000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuleKind {
    Panic,
    Boot,
    Pattern,
}

#[derive(Debug, Clone)]
struct Rule {
    id: &'static str,
    label: String,
    kind: RuleKind,
    lower: String,
}

/// Built-in panic-like rules, most specific first. A line is reported under
/// one rule only: "watchdog: BUG: ..." must not also be a plain "BUG: ".
pub const PANIC_PATTERNS: &[(&str, &str, &str)] = &[
    ("watchdog-bug", "watchdog: BUG", "Watchdog reported a bug"),
    ("kernel-panic", "Kernel panic", "Kernel panic message"),
    ("oops", "Oops:", "Kernel oops report"),
    ("bug", "BUG: ", "Kernel BUG warning"),
    ("oom", "Out of memory", "Kernel out-of-memory report"),
    ("oom-kill", "oom-kill", "OOM killer invoked"),
    (
        "null-deref",
        "Unable to handle kernel NULL pointer dereference",
        "Kernel NULL pointer dereference",
    ),
    ("segfault", "segfault at", "Userspace segmentation fault"),
    ("hardware-error", "Hardware Error", "Hardware error report"),
    ("cpu-warning", "WARNING: CPU", "Kernel CPU warning"),
];

/// Boot banners: U-Boot/Linux version vocabulary plus the kernel handoffs.
pub const BOOT_PATTERNS: &[(&str, &str, &str)] = &[
    ("u-boot", "U-Boot ", "Bootloader banner"),
    ("linux-version", "Linux version ", "Linux kernel banner"),
    ("booting-linux", "Booting Linux", "Linux boot message"),
    (
        "starting-kernel",
        "Starting kernel",
        "Kernel handoff message",
    ),
];

/// Built-in labels in Chinese. User-supplied labels are shown unchanged: the
/// engine does not translate text it did not write.
pub const PANIC_LABELS_ZH: &[(&str, &str)] = &[
    ("kernel-panic", "内核崩溃信息"),
    ("oops", "内核 oops 报告"),
    ("bug", "内核 BUG 警告"),
    ("oom", "内核内存耗尽报告"),
    ("oom-kill", "OOM 终止进程"),
    ("watchdog-bug", "看门狗 BUG 报告"),
    ("null-deref", "内核空指针解引用"),
    ("segfault", "用户态段错误"),
    ("hardware-error", "硬件错误报告"),
    ("cpu-warning", "内核 CPU 警告"),
    ("u-boot", "引导程序启动横幅"),
    ("linux-version", "Linux 内核启动横幅"),
    ("booting-linux", "Linux 启动信息"),
    ("starting-kernel", "内核交接信息"),
];

fn builtin_rules() -> Vec<Rule> {
    let mut rules = Vec::new();
    for (id, text, label) in PANIC_PATTERNS {
        rules.push(Rule {
            id,
            label: (*label).to_string(),
            kind: RuleKind::Panic,
            lower: text.to_lowercase(),
        });
    }
    for (id, text, label) in BOOT_PATTERNS {
        rules.push(Rule {
            id,
            label: (*label).to_string(),
            kind: RuleKind::Boot,
            lower: text.to_lowercase(),
        });
    }
    rules
}

/// Boot-loop bookkeeping: one shared window of banner observations, plus a
/// per-rule `reported` flag so each banner rule reports a burst once.
#[derive(Default)]
struct BootLoop {
    times: Vec<u64>,
    banners: Vec<String>,
    bursts: HashMap<&'static str, bool>,
    failed: bool,
    last_at: Option<u64>,
}

#[derive(Default)]
struct State {
    rules: Vec<Rule>,
    max_findings: usize,
    boot_threshold: u32,
    boot_window_ms: u64,
    findings: Vec<Finding>,
    /// De-duplication keys, index-aligned with `findings` (the JS keeps them
    /// in a WeakMap so the finding shape stays plain JSON).
    dedup_keys: Vec<String>,
    line: String,
    lines: u64,
    dropped: usize,
    boot_count: u32,
    boot_loop: BootLoop,
}

fn bounded_int(value: i64, fallback: i64, min: i64, max: i64) -> i64 {
    if value == i64::MIN {
        return fallback;
    }
    value.clamp(min, max)
}

fn clip(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() > MAX_EVIDENCE {
        chars[chars.len() - MAX_EVIDENCE..].iter().collect()
    } else {
        text.to_string()
    }
}

/// `/^zh\b|^zh[-_]/i` from serial_watch.js: `zh`, `zh-CN`, `zh_CN`, `zh x` are
/// Chinese; `zhong` is not (`\w` after `zh` leaves no boundary, and `_` is a
/// word character, so it is matched by the explicit `[-_]` branch instead).
fn is_chinese(lang: &str) -> bool {
    let lower = lang.trim().to_lowercase();
    let Some(rest) = lower.strip_prefix("zh") else {
        return false;
    };
    match rest.chars().next() {
        None => true,
        Some(c) => c == '-' || c == '_' || !(c.is_ascii_alphanumeric() || c == '_'),
    }
}

/// One watch instance. `feed_text` returns only the findings produced by that
/// call; `findings()` returns everything retained, oldest first.
#[derive(Default)]
pub struct SerialWatch {
    state: State,
    decoder: crate::journal::Utf8Decoder,
}

impl SerialWatch {
    pub fn new(opts: WatchOptions) -> Self {
        let max_findings =
            bounded_int(opts.max_findings as i64, 20, 1, MAX_FINDINGS as i64) as usize;
        let boot_threshold = bounded_int(
            opts.boot_threshold as i64,
            DEFAULT_BOOT_LOOP_THRESHOLD as i64,
            1,
            MAX_BOOT_LOOP_THRESHOLD as i64,
        ) as u32;
        let boot_window_ms = bounded_int(
            opts.boot_window_ms as i64,
            DEFAULT_BOOT_LOOP_WINDOW_MS as i64,
            1,
            MAX_BOOT_LOOP_WINDOW_MS as i64,
        ) as u64;
        let mut state = State {
            rules: builtin_rules(),
            max_findings,
            boot_threshold,
            boot_window_ms,
            ..State::default()
        };
        state.boot_loop.bursts = state.rules.iter().map(|rule| (rule.id, false)).collect();
        Self {
            state,
            decoder: crate::journal::Utf8Decoder::default(),
        }
    }

    /// Register one caller pattern; returns false when it is a duplicate of a
    /// built-in id or an already-registered id, or when the cap is reached.
    /// Never treats caller input as a regular expression and never lets an
    /// empty string match every line.
    pub fn add_pattern(&mut self, id: &str, text: &str, label: &str) -> bool {
        let id = id.trim();
        if id.is_empty() || text.is_empty() {
            return false;
        }
        if self.state.rules.len() - builtin_rules().len() >= MAX_PATTERNS {
            return false;
        }
        if self.state.rules.iter().any(|rule| rule.id == id) {
            return false;
        }
        let label = label.trim();
        let rule = Rule {
            id: leak(id),
            label: if label.is_empty() {
                text.to_string()
            } else {
                label.to_string()
            },
            kind: RuleKind::Pattern,
            lower: text.to_lowercase(),
        };
        self.state.rules.push(rule);
        true
    }

    /// Feed decoded output text (call with timestamps from the caller clock).
    /// Incomplete lines are buffered across calls; only complete lines are
    /// evaluated, and a completed line is evaluated once.
    pub fn feed_text(&mut self, text: &str, at_ms: u64) {
        self.feed(text, at_ms);
    }

    /// Feed raw bytes with streaming UTF-8 handling so a multi-byte character
    /// split across two UART notifications is not turned into replacement
    /// characters.
    pub fn feed_bytes(&mut self, bytes: &[u8], at_ms: u64) {
        let text = self.decoder.decode(bytes);
        if !text.is_empty() {
            self.feed(&text, at_ms);
        }
    }

    fn feed(&mut self, text: &str, at_ms: u64) {
        let mut start = 0usize;
        let chars: Vec<char> = text.chars().collect();
        for (index, &ch) in chars.iter().enumerate() {
            if ch != '\n' && ch != '\r' {
                continue;
            }
            // Treat CRLF as one terminator; a lone CR still ends a line.
            if ch == '\r' && chars.get(index + 1) == Some(&'\n') {
                continue;
            }
            let body: String = chars[start..index].iter().collect();
            let mut line = self.state.line.clone();
            line.push_str(&body);
            self.state.line = String::new();
            start = index + 1;
            let line = tail_chars(&line, MAX_LINE_CHARS);
            self.state.lines += 1;
            self.evaluate_line(&line, at_ms);
        }
        if start < chars.len() {
            let rest_body: String = chars[start..].iter().collect();
            let rest = format!("{}{rest_body}", self.state.line);
            if rest.chars().count() > MAX_LINE_CHARS {
                self.state.dropped += rest.chars().count() - MAX_LINE_CHARS;
            }
            self.state.line = tail_chars(&rest, MAX_LINE_CHARS);
        }
    }

    fn evaluate_line(&mut self, rawline: &str, at: u64) {
        let line = rawline.strip_suffix('\r').unwrap_or(rawline);
        let evidence = clip(line);
        let lower = line.to_lowercase();
        let line_number = self.state.lines;
        // Whether this line already produced a panic/pattern finding. Banner
        // rules do not set it: they report on their own rule and cannot
        // duplicate another rule's finding for the same line.
        let mut reported = false;
        let rule_indexes: Vec<usize> = (0..self.state.rules.len()).collect();
        for index in rule_indexes {
            let rule = self.state.rules[index].clone();
            if !lower.contains(&rule.lower) {
                continue;
            }
            match rule.kind {
                RuleKind::Boot => {
                    self.state.boot_count += 1;
                    if let Some(finding) = self.observe_boot(&rule, line_number, at, &evidence) {
                        // `found` is implicit: findings are retained in state;
                        // the feed() callers read them via findings().
                        drop(finding);
                    }
                }
                _ => {
                    // A panic line between two banners means the target did
                    // not get far enough to reboot on its own.
                    self.state.boot_loop.failed = true;
                    // At most one panic finding per line: a line matching
                    // several built-in rules is reported under the first
                    // (most specific) one.
                    if reported {
                        continue;
                    }
                    if self.observe_rule(&rule, line_number, at, &evidence) {
                        reported = true;
                    }
                }
            }
        }
    }

    /// Panic rules and user patterns: one retained record per de-duplication
    /// key, refreshed in place on every later occurrence of the same thing.
    /// Returns true when a NEW finding was appended.
    fn observe_rule(&mut self, rule: &Rule, line_number: u64, at: u64, evidence: &str) -> bool {
        let kind = if rule.kind == RuleKind::Pattern {
            FindingKind::Pattern
        } else {
            FindingKind::Panic
        };
        let key = dedup_key(rule, evidence);
        if let Some(existing) = self.find_record(&key) {
            existing.count += 1;
            existing.at_ms = at;
            return false;
        }
        let finding = Finding {
            kind,
            id: rule.id.to_string(),
            label: rule.label.clone(),
            line: line_number,
            at_ms: at,
            count: 1,
            evidence: evidence.to_string(),
        };
        self.remember(finding, key);
        true
    }

    fn observe_boot(
        &mut self,
        rule: &Rule,
        line_number: u64,
        at: u64,
        evidence: &str,
    ) -> Option<Finding> {
        if self.state.boot_loop.failed {
            // A panic sat between these banners, so they never formed one
            // boot burst.
            self.state.boot_loop.times.clear();
            self.state.boot_loop.banners.clear();
            for reported in self.state.boot_loop.bursts.values_mut() {
                *reported = false;
            }
            self.state.boot_loop.failed = false;
        }
        // A gap longer than the window since the last banner of any kind
        // starts a new burst: every rule gets to report about it again.
        // `saturating_sub` because the wall clock can step backwards (NTP, a
        // restored VM): a bare `at - last` would panic in debug and wrap to a
        // huge gap in release.
        let stale = self
            .state
            .boot_loop
            .last_at
            .map(|last| at.saturating_sub(last) > self.state.boot_window_ms)
            .unwrap_or(false);
        if stale {
            for reported in self.state.boot_loop.bursts.values_mut() {
                *reported = false;
            }
        }
        self.state.boot_loop.last_at = Some(at);
        let dropped = push_banner(
            &mut self.state.boot_loop,
            at,
            &format!("{evidence} (line {line_number})"),
            self.state.boot_window_ms,
        );
        if dropped {
            for reported in self.state.boot_loop.bursts.values_mut() {
                *reported = false;
            }
        }
        let reported = self.state.boot_loop.bursts.entry(rule.id).or_insert(false);
        if self.state.boot_loop.times.len() < self.state.boot_threshold as usize {
            return None;
        }
        if *reported {
            return None;
        }
        let evidence_text: String = self
            .state
            .boot_loop
            .banners
            .iter()
            .rev()
            .cloned()
            .collect::<Vec<_>>()
            .join(" | ");
        let key = format!("boot-loop:{}", rule.id);
        let count = self.state.boot_loop.times.len() as u32;
        if let Some(existing) = self.find_record(&key) {
            // Same rule observed again in a later burst: refresh the retained
            // record instead of appending a second entry for the same id.
            existing.count += 1;
            existing.at_ms = at;
            let clipped = clip(&evidence_text);
            if existing.evidence != clipped {
                existing.evidence = clipped;
            }
            *self.state.boot_loop.bursts.entry(rule.id).or_insert(false) = true;
            return None;
        }
        let finding = Finding {
            kind: FindingKind::BootLoop,
            id: rule.id.to_string(),
            label: rule.label.clone(),
            line: line_number,
            at_ms: at,
            count,
            evidence: clip(&evidence_text),
        };
        self.remember(finding, key);
        *self.state.boot_loop.bursts.entry(rule.id).or_insert(false) = true;
        Some(self.state.findings.last().cloned().unwrap())
    }

    fn find_record(&mut self, key: &str) -> Option<&mut Finding> {
        let mut found = None;
        for index in (0..self.state.findings.len()).rev() {
            if self.state.dedup_keys[index] == key {
                found = Some(index);
                break;
            }
        }
        found.map(|index| &mut self.state.findings[index])
    }

    fn remember(&mut self, finding: Finding, key: String) {
        self.state.findings.push(finding);
        self.state.dedup_keys.push(key);
        while self.state.findings.len() > self.state.max_findings {
            self.state.findings.remove(0);
            self.state.dedup_keys.remove(0);
        }
    }

    /// Retained findings, oldest first.
    pub fn findings(&self) -> &[Finding] {
        &self.state.findings
    }

    /// Plain JSON-ish view for the UI: `buffered` exposes the bounded line
    /// buffer so bounded memory is assertable; `dropped` counts characters
    /// discarded from an over-long unfinished line.
    pub fn snapshot(&self) -> WatchSnapshot {
        WatchSnapshot {
            findings: self.state.findings.clone(),
            lines: self.state.lines,
            boot_count: self.state.boot_count,
            buffered: self.state.line.chars().count(),
            dropped: self.state.dropped,
        }
    }

    /// Drop accumulated findings and buffered input, keep the counters and the
    /// configured rules (a new device session on the same watch).
    pub fn clear(&mut self) {
        self.state.findings.clear();
        self.state.dedup_keys.clear();
        self.state.line.clear();
        self.state.dropped = 0;
        self.state.boot_loop = BootLoop::default();
        self.state.boot_loop.bursts = self
            .state
            .rules
            .iter()
            .map(|rule| (rule.id, false))
            .collect();
    }

    pub fn lines_seen(&self) -> u64 {
        self.state.lines
    }

    /// Human sentence for a finding, en/zh like the web client.
    pub fn describe(finding: &Finding, lang: &str) -> String {
        let chinese = is_chinese(lang);
        let zh_label = if chinese {
            PANIC_LABELS_ZH
                .iter()
                .find(|(id, _)| *id == finding.id)
                .map(|(_, label)| (*label).to_string())
        } else {
            None
        };
        let label = match zh_label {
            Some(label) => label,
            None => {
                if finding.label.is_empty() {
                    finding.id.clone()
                } else {
                    finding.label.clone()
                }
            }
        };
        let line = finding.line;
        let count = finding.count;
        let evidence = &finding.evidence;
        match finding.kind {
            FindingKind::Panic => {
                if chinese {
                    format!("第 {line} 行出现{label}：{evidence}")
                } else {
                    format!("Observed {label} at line {line}: {evidence}")
                }
            }
            FindingKind::BootLoop => {
                if chinese {
                    format!(
                        "观察窗口内出现 {count} 次{label}，设备可能在稳定前反复重启：{evidence}"
                    )
                } else {
                    format!(
                        "Observed {label} {count} times within the watch window; \
the target may be restarting before it settles: {evidence}"
                    )
                }
            }
            FindingKind::Pattern => {
                if chinese {
                    format!("第 {line} 行出现{label}：{evidence}")
                } else {
                    format!("Observed {label} at line {line}: {evidence}")
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct WatchSnapshot {
    pub findings: Vec<Finding>,
    pub lines: u64,
    pub boot_count: u32,
    pub buffered: usize,
    pub dropped: usize,
}

/// Minimal convenience for a status line: is a boot-loop observation among
/// these findings? An observation, not a verdict about the target.
pub fn is_boot_loop_hint(findings: &[Finding]) -> bool {
    findings.iter().any(|f| f.kind == FindingKind::BootLoop)
}

/// What makes two observations "the same finding": a caller pattern is
/// identified by its id; a built-in panic rule keeps its id but adds the
/// offending message so an endless repeat of the same panic costs one entry
/// while a genuinely different BUG/panic line is new information.
fn dedup_key(rule: &Rule, line: &str) -> String {
    if rule.kind == RuleKind::Pattern {
        return format!("pattern:{}", rule.id);
    }
    let text = line.trim();
    let clipped: String = {
        let chars: Vec<char> = text.chars().collect();
        if chars.len() > MAX_EVIDENCE {
            chars[..MAX_EVIDENCE].iter().collect()
        } else {
            text.to_string()
        }
    };
    format!("panic:{}:{clipped}", rule.id)
}

/// One window of boot banners across all banner rules, newest last. The window
/// is trimmed in place, so it never exceeds `MAX_BOOT_HISTORY` observations.
fn push_banner(loop_: &mut BootLoop, at: u64, text: &str, window_ms: u64) -> bool {
    loop_.times.push(at);
    loop_.banners.push(text.to_string());
    if loop_.times.len() > MAX_BOOT_HISTORY {
        let overflow = loop_.times.len() - MAX_BOOT_HISTORY;
        loop_.times.drain(..overflow);
        loop_.banners.drain(..overflow);
    }
    let mut dropped = false;
    // The observation that just arrived is never dropped, so the caller can
    // always decide about the window that ends at `at`. `saturating_sub` guards
    // a backwards wall-clock step, which a bare subtraction would turn into a
    // debug panic or a release wrap.
    while loop_.times.len() > 1 && at.saturating_sub(loop_.times[0]) > window_ms {
        loop_.times.remove(0);
        loop_.banners.remove(0);
        dropped = true;
    }
    dropped
}

fn tail_chars(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_string();
    }
    text.char_indices()
        .nth(count - max)
        .map_or_else(String::new, |(idx, _)| text[idx..].to_string())
}

/// `id` must outlive the watch; patterns come from tool arguments that are
/// short-lived, so leak them into `'static` (bounded by MAX_PATTERNS).
fn leak(text: &str) -> &'static str {
    Box::leak(text.to_string().into_boxed_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn watch() -> SerialWatch {
        SerialWatch::new(WatchOptions::default())
    }

    #[test]
    fn detects_builtin_panic() {
        let mut w = watch();
        w.feed_text(
            "hello\nKernel panic - not syncing: Attempted to kill init!\n",
            1000,
        );
        let findings = w.findings();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, FindingKind::Panic);
        assert_eq!(findings[0].id, "kernel-panic");
        assert_eq!(findings[0].count, 1);
        assert_eq!(findings[0].line, 2);
        assert!(findings[0].evidence.contains("Kernel panic"));
    }

    #[test]
    fn most_specific_rule_wins_per_line() {
        let mut w = watch();
        w.feed_text("watchdog: BUG: everyone stay home\n", 1);
        let findings = w.findings();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].id, "watchdog-bug");
    }

    #[test]
    fn repeated_panic_dedups_into_count() {
        let mut w = watch();
        w.feed_text("Oops: 0000 [#1]\n", 10);
        w.feed_text("Oops: 0000 [#1]\n", 20);
        w.feed_text("Oops: 0000 [#1]\n", 30);
        assert_eq!(w.findings().len(), 1);
        assert_eq!(w.findings()[0].count, 3);
        assert_eq!(w.findings()[0].at_ms, 30);
        // A different message under the same rule is new information.
        w.feed_text("Oops: 0000 [#2]\n", 40);
        assert_eq!(w.findings().len(), 2);
    }

    #[test]
    fn user_pattern_matches_literally_case_insensitively() {
        let mut w = watch();
        assert!(w.add_pattern("user-0", "Ready.", ""));
        w.feed_text("device ready. DEVICE READY.\n", 5);
        assert_eq!(w.findings().len(), 1);
        assert_eq!(w.findings()[0].kind, FindingKind::Pattern);
        assert_eq!(w.findings()[0].label, "Ready.");
        // Never a regex: '.' matches only itself, and '(' would not group.
        let mut w2 = watch();
        w2.add_pattern("user-0", "a.c", "");
        w2.feed_text("abc\n", 1);
        assert!(w2.findings().is_empty());
        w2.feed_text("a.c\n", 2);
        assert_eq!(w2.findings().len(), 1);
    }

    #[test]
    fn pattern_cap_is_64_and_duplicate_ids_rejected() {
        let mut w = watch();
        assert!(w.add_pattern("builtin-id", "x", "")); // conflicts with built-in? no builtin has this id
        assert!(!w.add_pattern("builtin-id", "y", "")); // duplicate id
        assert!(!w.add_pattern("", "y", "")); // empty id
        assert!(!w.add_pattern("z", "", "")); // empty text
        let base = w.state.rules.len();
        for i in 0..MAX_PATTERNS + 10 {
            w.add_pattern(&format!("p{i}"), &format!("needle{i}"), "");
        }
        assert!(w.state.rules.len() - base <= MAX_PATTERNS);
    }

    #[test]
    fn boot_loop_reports_once_per_burst() {
        let mut w = watch();
        w.feed_text("U-Boot 2024.01\n", 0);
        w.feed_text("Linux version 6.6\n", 1000);
        w.feed_text("U-Boot 2024.01\n", 2000);
        assert!(
            is_boot_loop_hint(w.findings()),
            "expected a boot-loop finding"
        );
        let boot = w
            .findings()
            .iter()
            .find(|f| f.kind == FindingKind::BootLoop)
            .unwrap()
            .clone();
        assert_eq!(boot.id, "u-boot");
        assert_eq!(boot.count, 3);
        // Fourth banner inside the window refreshes count, no new entry.
        w.feed_text("U-Boot 2024.01\n", 3000);
        let boots: Vec<_> = w
            .findings()
            .iter()
            .filter(|f| f.kind == FindingKind::BootLoop)
            .collect();
        assert_eq!(boots.len(), 1);
        // Still the same burst: `reported` short-circuits before the record is
        // touched, so the count is the number of banners in the burst.
        assert_eq!(boots[0].count, 3);

        // A new burst (older banners left the window) refreshes the record
        // instead of appending a second entry for the same id.
        let at = DEFAULT_BOOT_LOOP_WINDOW_MS;
        w.feed_text("U-Boot 2024.01\n", at + 10_000);
        w.feed_text("U-Boot 2024.01\n", at + 11_000);
        w.feed_text("U-Boot 2024.01\n", at + 12_000);
        let boots: Vec<_> = w
            .findings()
            .iter()
            .filter(|f| f.kind == FindingKind::BootLoop)
            .collect();
        assert_eq!(boots.len(), 1);
        assert_eq!(boots[0].count, 4);
    }

    #[test]
    fn boot_loop_window_expires() {
        let mut w = watch();
        w.feed_text("U-Boot 1\n", 0);
        w.feed_text("U-Boot 1\n", 1000);
        w.feed_text("U-Boot 1\n", DEFAULT_BOOT_LOOP_WINDOW_MS + 10_000);
        // The third banner is outside the 120 s window: window reset before push,
        // so only 1 observation is in-window => no threshold yet.
        assert!(!is_boot_loop_hint(w.findings()));
        w.feed_text("U-Boot 1\n", DEFAULT_BOOT_LOOP_WINDOW_MS + 11_000);
        w.feed_text("U-Boot 1\n", DEFAULT_BOOT_LOOP_WINDOW_MS + 12_000);
        assert!(is_boot_loop_hint(w.findings()));
    }

    #[test]
    fn panic_between_banners_breaks_the_burst() {
        let mut w = watch();
        w.feed_text("U-Boot 1\n", 0);
        w.feed_text("Kernel panic - not syncing\n", 1000);
        w.feed_text("U-Boot 1\n", 2000);
        w.feed_text("U-Boot 1\n", 3000);
        assert!(!is_boot_loop_hint(w.findings()));
        w.feed_text("U-Boot 1\n", 4000);
        assert!(is_boot_loop_hint(w.findings()));
    }

    #[test]
    fn unfinished_line_is_buffered_and_bounded() {
        let mut w = watch();
        let long: String = "x".repeat(5000);
        w.feed_text(&long, 1);
        assert_eq!(w.findings().len(), 0);
        let snapshot = w.snapshot();
        assert_eq!(snapshot.buffered, MAX_LINE_CHARS);
        assert_eq!(snapshot.dropped, 5000 - MAX_LINE_CHARS);
        // Completing the line evaluates exactly the retained tail.
        w.feed_text("\n", 2);
        assert_eq!(w.snapshot().buffered, 0);
        assert_eq!(w.snapshot().lines, 1);
    }

    #[test]
    fn crlf_is_one_terminator_and_lone_cr_ends_line() {
        let mut w = watch();
        w.feed_text("Kernel panic\r\nOops: a\rOops: b", 1);
        assert_eq!(w.snapshot().lines, 2);
        assert_eq!(w.findings().len(), 2);
    }

    #[test]
    fn findings_are_capped_at_max_findings() {
        let mut w = SerialWatch::new(WatchOptions {
            max_findings: 3,
            ..WatchOptions::default()
        });
        for i in 0..6 {
            w.feed_text(&format!("Oops: variant {i}\n"), i);
        }
        assert_eq!(w.findings().len(), 3);
        // Oldest dropped first.
        assert!(w.findings()[0].evidence.contains("variant 3"));
    }

    #[test]
    fn evidence_is_clipped_to_240() {
        let mut w = watch();
        let long = format!("Oops: {}", "e".repeat(600));
        w.feed_text(&format!("{long}\n"), 1);
        let finding = &w.findings()[0];
        // `clip` keeps the TAIL of the line (serial_watch.js slice(-240)).
        assert_eq!(finding.evidence.chars().count(), MAX_EVIDENCE);
        assert!(finding.evidence.chars().all(|c| c == 'e'));
        // The bounded line buffer keeps the tail too, so `line` agrees.
        assert_eq!(finding.line, 1);
        assert!(finding.evidence.len() < long.len());
    }

    #[test]
    fn describe_en_and_zh() {
        let mut w = watch();
        w.feed_text("Kernel panic - not syncing: x\n", 42);
        let finding = &w.findings()[0];
        let en = SerialWatch::describe(finding, "en");
        assert_eq!(
            en,
            "Observed Kernel panic message at line 1: Kernel panic - not syncing: x"
        );
        let zh = SerialWatch::describe(finding, "zh-CN");
        assert_eq!(zh, "第 1 行出现内核崩溃信息：Kernel panic - not syncing: x");

        let mut w2 = watch();
        w2.feed_text("U-Boot 1\nU-Boot 2\nU-Boot 3\n", 0);
        let boot = w2
            .findings()
            .iter()
            .find(|f| f.kind == FindingKind::BootLoop)
            .unwrap();
        let en = SerialWatch::describe(boot, "en");
        assert!(en.starts_with("Observed Bootloader banner 3 times within the watch window"));
        let zh = SerialWatch::describe(boot, "zh-CN");
        assert!(zh.starts_with("观察窗口内出现 3 次引导程序启动横幅"));
    }

    #[test]
    fn zh_detection_rules() {
        assert!(is_chinese("zh"));
        assert!(is_chinese("zh-CN"));
        assert!(is_chinese("zh_CN"));
        assert!(is_chinese("ZH-cn"));
        assert!(is_chinese("zh "));
        assert!(!is_chinese("en"));
        assert!(!is_chinese("zhong"));
        assert!(!is_chinese("zho"));
        assert!(!is_chinese(""));
    }

    #[test]
    fn feed_bytes_handles_split_utf8() {
        let mut w = watch();
        let text = "héllo\n".as_bytes();
        w.feed_bytes(&text[..2], 1); // splits the é
        w.feed_bytes(&text[2..], 2);
        assert_eq!(w.snapshot().lines, 1);
    }

    #[test]
    fn clear_resets_findings_but_keeps_counters() {
        let mut w = watch();
        w.feed_text("Oops: x\n", 1);
        assert_eq!(w.findings().len(), 1);
        w.clear();
        assert!(w.findings().is_empty());
        assert_eq!(w.snapshot().lines, 1);
    }

    #[test]
    fn boot_rules_and_panic_rules_are_frozen_ids() {
        // Contract with the JS source: ids and texts must match byte-for-byte.
        let js = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../web/serial_watch.js"
        ))
        .expect("read web/serial_watch.js");
        for (id, text, label) in PANIC_PATTERNS.iter().chain(BOOT_PATTERNS) {
            assert!(
                js.contains(&format!("id: \"{id}\"")),
                "id {id} missing from serial_watch.js"
            );
            assert!(
                js.contains(&format!("text: \"{text}\"")),
                "text {text:?} missing from serial_watch.js"
            );
            assert!(
                js.contains(&format!("label: \"{label}\"")),
                "label {label:?} missing from serial_watch.js"
            );
        }
        for (id, label) in PANIC_LABELS_ZH {
            // The JS object literal quotes keys only when they are not
            // identifiers: `oops:` but `"kernel-panic":`.
            let mut chars = id.chars();
            let identifier = chars
                .next()
                .map(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
                .unwrap_or(false)
                && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$');
            let key = if identifier {
                format!("{id}: \"{label}\"")
            } else {
                format!("\"{id}\": \"{label}\"")
            };
            assert!(
                js.contains(&key),
                "zh label for {id} missing from serial_watch.js: {key}"
            );
        }
        assert!(js.contains("const MAX_LINE_CHARS = 1024;"));
        assert!(js.contains("const MAX_FINDINGS = 200;"));
        assert!(js.contains("const MAX_PATTERNS = 64;"));
    }
}
