import assert from "node:assert/strict";
import test from "node:test";
import { requiresInputApproval, inputLeavesPendingLine } from "../../web/agent_execution_policy.js";

const args = (text, appendEnter = true) => ({ text, appendEnter });

test("Auto permits only complete exact queries with supported Enter sequences", () => {
  for (const text of ["pwd", "uname -a", "free -h", "ip addr show", "cat /proc/version"]) {
    for (const enter of ["\r", "\n", "\r\n"]) {
      assert.equal(requiresInputApproval("auto", args(text), text + enter), false);
    }
  }
});

test("Auto does not grant query permission to mutations or shell syntax", () => {
  for (const text of ["reboot", "rm -rf /", "dmesg -c", "dmesg -C", "date -s now", "ip link set eth0 down",
    "uname -a; reboot", "uname -a\nreboot", "uname -a\rreboot", "uname | sh", "uname > /tmp/file",
    "uname $(reboot)", "uname `reboot`", "uname && reboot", "uname &", "ls /root", "cat /dev/zero",
    "cat /etc/shadow", "sudo uname", "busybox uname", "/bin/uname", "\x1buname", "\x03uname", " uname", "uname\t-a"]) {
    assert.equal(requiresInputApproval("auto", args(text), text + "\r"), true, text);
  }
});

test("Auto requires approval for partial, altered, or concatenated input", () => {
  assert.equal(requiresInputApproval("auto", args("uname", false), "uname"), true);
  assert.equal(requiresInputApproval("auto", args("uname"), "uname\rreboot\r"), true);
  assert.equal(requiresInputApproval("auto", args("uname"), "uname\r", true), true);
});

test("Manual and unknown modes require approval; Full Auto executes any valid input", () => {
  for (const text of ["uname", "reboot", "\x03", "echo yes > /tmp/test"]) {
    assert.equal(requiresInputApproval("manual", args(text), text + "\r"), true);
    assert.equal(requiresInputApproval("unknown", args(text), text + "\r"), true);
    assert.equal(requiresInputApproval("full-auto", args(text, false), text, true), false);
  }
});

test("terminal input remains pending through editing controls until a line boundary", () => {
  const state = (text, pending = false) => inputLeavesPendingLine(new TextEncoder().encode(text), pending);
  assert.equal(state("rm "), true);
  assert.equal(state("\x1b[A"), true);
  assert.equal(state("\b", true), true);
  assert.equal(state("\r\n", true), false);
  assert.equal(state("\x03", true), false);
  assert.equal(state("uname\nnext"), true);
  assert.equal(state("", true), true);
});
