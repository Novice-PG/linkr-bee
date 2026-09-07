// Passive hints only: device output is untrusted and prompts can be customized.
// Match the current tail, never grant shell access from an earlier boot message.
export function inspectSerialConsole(log) {
  const lines = (log.text || "").slice(-4000).replace(/\r/g, "\n").split("\n");
  const tail = lines.at(-1).trimEnd();
  let kind = "unknown";
  let evidence = "";
  if (/^(?:[^\s:]+\s+)?login:\s*$/i.test(tail)) kind = "login";
  else if (/^(?:[^\r\n]{0,80}\s)?password:\s*$/i.test(tail)) kind = "password";
  else if (/^(?:=>|U-Boot>)\s*$/.test(tail)) kind = "bootloader";
  else if (/^(?:[\w.-]+@[\w.-]+:[^\r\n]*|\[[\w.-]+@[\w.-]+ [^\]\r\n]*\])[$#]\s*$/.test(tail)) kind = "shell";
  else if (lines.some((line) => /Kernel panic - not syncing:/.test(line))) kind = "panic";
  if (kind !== "unknown") evidence = kind === "panic"
    ? lines.findLast((line) => /Kernel panic - not syncing:/.test(line)).slice(0, 240)
    : tail.slice(0, 240);
  return { kind, evidence, cursor: log.latestCursor, source: "serial-output-heuristic" };
}
