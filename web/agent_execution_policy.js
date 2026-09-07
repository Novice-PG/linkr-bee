// Exact, deliberately small shell-query allowlist. Never trust a model's risk label.
const queries = new Set([
  "pwd", "whoami", "id", "uptime", "date",
  "uname", "uname -a", "uname -r", "uname -m",
  "ls", "ls -l", "ls -a", "ls -la", "ls -al", "ls -lh",
  "df", "df -h", "df -T", "free", "free -h", "free -m",
  "lsblk", "lsblk -f", "dmesg", "dmesg -T",
  "ip addr show", "ip link show", "ip route show",
  "cat /proc/version", "cat /proc/cpuinfo", "cat /proc/meminfo",
  "cat /proc/uptime", "cat /proc/cmdline", "cat /etc/os-release",
]);

export function requiresInputApproval(mode, args, payload, inputPending = false) {
  if (mode === "full-auto") return false;
  if (mode !== "auto" || inputPending || args.appendEnter !== true) return true;
  // Compare the exact wire text: no multiline, escapes, operators, substitution,
  // arbitrary flags/paths or partial input can inherit query permission.
  return !queries.has(args.text) || !["\r", "\n", "\r\n"].some((enter) => payload === args.text + enter);
}

export function inputLeavesPendingLine(bytes, pending) {
  for (const byte of bytes) {
    // Other controls (including cursor movement/backspace) leave state uncertain.
    pending = byte !== 10 && byte !== 13 && byte !== 3;
  }
  return pending;
}

export function executionModePrompt(mode = "auto") {
  if (mode === "full-auto") return "Execution mode: Full Auto. The user authorizes direct serial execution without confirmation.";
  if (mode === "auto") return "Execution mode: Auto (recommended). The app automatically sends only exact allowlisted low-risk queries on a clear input line with a currently recognized shell prompt. All other input requires user approval. Do not assume shell queries are suitable until logs establish the target console state.";
  return "Execution mode: Manual. Propose commands with send_serial_input; nothing is sent until the user clicks Send.";
}
