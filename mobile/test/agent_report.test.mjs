import assert from "node:assert/strict";
import test from "node:test";
import { buildTaskReport, REPORT_MAX_CHARS } from "../../web/agent_report.js";

const task = {
  id: "task-1", goal: "Why did the board fail to boot?", status: "answered",
  summary: "The root filesystem could not be mounted.",
  plan: [
    { title: "Read the boot log", status: "completed", verification: "panic line observed at 115200" },
    { title: "Check the root device", status: "blocked", verification: "mmcblk0 missing", nextAction: "reseat the SD card" },
  ],
};
const records = [{
  id: "serial-1", payload: "dmesg | tail -5\r", delivery: "sent", executionStatus: "completed",
  exitCode: 0, observation: "prompt-returned", evidence: "mmcblk0: error -110\r\nroot@board:~# ",
}];

test("a report carries the goal, plan, executions and evidence", () => {
  const report = buildTaskReport({ lang: "en", device: "target:abc", task, records, notes: [{ text: "SD slot is loose", evidence: "observed twice" }] });
  assert.match(report, /# Linkr Bee diagnostic report/);
  assert.match(report, /Device · target:abc/);
  assert.match(report, /Why did the board fail to boot\?/);
  assert.match(report, /## Assistant summary/);
  assert.match(report, /- \[completed\] Read the boot log/);
  assert.match(report, /- \[blocked\] Check the root device/);
  assert.match(report, /Next: reseat the SD card/);
  assert.match(report, /### serial-1/);
  assert.match(report, /`dmesg \| tail -5`/);
  assert.match(report, /exit code 0/);
  assert.match(report, /mmcblk0: error -110/);
  assert.match(report, /## Device notes/);
  assert.match(report, /SD slot is loose/);
  // The closing caution keeps exit codes from reading as proof of success.
  assert.match(report, /not proof that the goal was met/);
});

test("a report is redacted like the task store and bounded in size", () => {
  const report = buildTaskReport({
    lang: "zh",
    device: "target:abc",
    task: { ...task, goal: "use password=hunter2 to log in", summary: "fetch http://user:pw@host/x?token=abc failed" },
    records: [{ ...records[0], evidence: "password: hunter2\r\n" }],
    notes: [{ text: "api_key=abcdef" }],
  });
  assert.doesNotMatch(report, /hunter2/);
  assert.doesNotMatch(report, /token=abc/);
  assert.doesNotMatch(report, /abcdef/);
  assert.match(report, /# Linkr Bee 排查报告/);

  const many = Array.from({ length: 150 }, (_, index) => ({ ...records[0], id: `serial-${index}`, evidence: "y".repeat(2000) }));
  const huge = buildTaskReport({ task, records: many });
  assert.ok(huge.length <= REPORT_MAX_CHARS + 32, String(huge.length));
  assert.match(huge, /\[truncated\]/);
});

test("a report works with no task at all", () => {
  const report = buildTaskReport({});
  assert.match(report, /# Linkr Bee diagnostic report/);
  assert.match(report, /not proof that the goal was met/);
});
