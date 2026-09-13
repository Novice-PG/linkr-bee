import assert from "node:assert/strict";
import test from "node:test";
import {
  POLICY_ENTRY_LIMIT,
  createCommandPolicyStore,
  formatPolicyList,
  isAlwaysAsk,
  isPreApproved,
  normalizePolicy,
  parsePolicyList,
} from "../../web/command_policy.js";
import { requiresInputApproval } from "../../web/agent_execution_policy.js";

function storage() {
  const values = new Map();
  return { getItem: (key) => values.get(key) ?? null,
    setItem: (key, value) => values.set(key, value), removeItem: (key) => values.delete(key) };
}

const wire = (text) => text + "\r";

test("an always-ask entry forces approval in every mode, Full Auto included", () => {
  const policy = { alwaysAsk: ["systemctl restart"], allow: [] };
  for (const mode of ["manual", "auto", "full-auto"]) {
    assert.equal(requiresInputApproval(mode, { text: "systemctl restart nginx", appendEnter: true },
      wire("systemctl restart nginx"), false, policy), true, mode);
  }
  // Matching covers the rewritten tracked command too, not just the raw text.
  assert.equal(requiresInputApproval("full-auto", { text: "sh -c 'systemctl restart nginx'" },
    wire("sh -c 'systemctl restart nginx'"), false, policy), true);
  // A different command is unaffected.
  assert.equal(requiresInputApproval("full-auto", { text: "uptime", appendEnter: true }, wire("uptime"), false, policy), false);
});

test("a pre-approved command is sent unattended in Auto mode only", () => {
  const policy = { alwaysAsk: [], allow: ["systemctl status nginx"] };
  assert.equal(requiresInputApproval("auto", { text: "systemctl status nginx", appendEnter: true },
    wire("systemctl status nginx"), false, policy), false);
  // The same conditions as the built-in list still apply.
  assert.equal(requiresInputApproval("auto", { text: "systemctl status nginx", appendEnter: true },
    wire("systemctl status nginx"), true, policy), true, "a pending input line blocks it");
  assert.equal(requiresInputApproval("auto", { text: "systemctl status nginx", appendEnter: false },
    wire("systemctl status nginx"), false, policy), true, "appendEnter is required");
  assert.equal(requiresInputApproval("auto", { text: "systemctl status nginx", appendEnter: true },
    "systemctl status nginx", false, policy), true, "the wire text must carry one Enter");
  // Manual still asks, and an entry cannot pre-approve a destructive command.
  assert.equal(requiresInputApproval("manual", { text: "systemctl status nginx", appendEnter: true },
    wire("systemctl status nginx"), false, policy), true);
  assert.equal(requiresInputApproval("auto", { text: "rm -rf /", appendEnter: true },
    wire("rm -rf /"), false, { alwaysAsk: [], allow: ["rm -rf /"] }), true);
  // A partial command never inherits the pre-approval.
  assert.equal(requiresInputApproval("auto", { text: "systemctl status nginx; reboot", appendEnter: true },
    wire("systemctl status nginx; reboot"), false, policy), true);
});

test("a policy can only tighten or relax its own direction", () => {
  const both = { alwaysAsk: ["status"], allow: ["systemctl status nginx"] };
  // alwaysAsk wins over allow for the same command.
  assert.equal(requiresInputApproval("auto", { text: "systemctl status nginx", appendEnter: true },
    wire("systemctl status nginx"), false, both), true);
  // No policy keeps the previous behaviour.
  assert.equal(requiresInputApproval("full-auto", { text: "reboot" }, wire("reboot"), false, null), false);
  assert.equal(requiresInputApproval("auto", { text: "uptime", appendEnter: true }, wire("uptime"), false, null), false);
  assert.equal(requiresInputApproval("auto", { text: "apt-get install vim", appendEnter: true },
    wire("apt-get install vim"), false, null), true);
});

test("policy lists are normalized, capped and matched case-insensitively", () => {
  assert.deepEqual(normalizePolicy({ alwaysAsk: ["  reboot  ", "", "REBOOT", "flash"], allow: ["ls -l"] }),
    { alwaysAsk: ["reboot", "flash"], allow: ["ls -l"] });
  assert.throws(() => normalizePolicy({ alwaysAsk: Array.from({ length: POLICY_ENTRY_LIMIT + 1 }, (_, i) => `cmd ${i}`) }), /at most/);
  assert.throws(() => normalizePolicy({ allow: ["x".repeat(201)] }), /200/);
  assert.throws(() => normalizePolicy({ allow: ["a\nb"] }), /control/);
  assert.equal(isAlwaysAsk(["reboot"], "sudo REBOOT now"), true);
  assert.equal(isPreApproved(["ls -l"], "ls -l"), true);
  assert.equal(isPreApproved(["ls -l"], "ls -la"), false);
  assert.deepEqual(parsePolicyList("reboot\n\n  flash  \n"), ["reboot", "flash"]);
  assert.equal(formatPolicyList(["reboot", "flash"]), "reboot\nflash");
});

test("the policy store keeps entries per device and drops empty records", () => {
  const store = storage();
  const policies = createCommandPolicyStore(store);
  assert.deepEqual(policies.get("target:abc"), { alwaysAsk: [], allow: [] });
  policies.save("target:abc", { alwaysAsk: ["reboot"], allow: ["ls -l"] });
  policies.save("target:other", { alwaysAsk: ["flash"] });
  assert.deepEqual(policies.get("target:abc"), { alwaysAsk: ["reboot"], allow: ["ls -l"] });
  assert.deepEqual(policies.get("target:other"), { alwaysAsk: ["flash"], allow: [] });
  assert.deepEqual(policies.get(""), { alwaysAsk: [], allow: [] });
  policies.save("target:abc", { alwaysAsk: [], allow: [] });
  assert.deepEqual(policies.get("target:abc"), { alwaysAsk: [], allow: [] });
  assert.deepEqual(policies.get("target:other"), { alwaysAsk: ["flash"], allow: [] });
  assert.throws(() => policies.save("", { alwaysAsk: ["x"] }), /identity/);
  policies.clear("target:other");
  assert.deepEqual(policies.get("target:other"), { alwaysAsk: [], allow: [] });
});
