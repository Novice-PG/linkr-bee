// Passive hints only: device output is untrusted and prompts can be customized.
// Match the current tail, never grant shell access from an earlier boot message.
export function inspectSerialConsole(log) {
  const lines = (log.text || "").slice(-4000).replace(/\r/g, "\n").split("\n");
  const tail = lines.at(-1).trimEnd();
  let kind = "unknown";
  let evidence = "";
  if (/^(?:[^\s:]+\s+)?login:\s*$/i.test(tail)) kind = "login";
  else if (/^\[sudo\] password for .+:\s*$/i.test(tail)) kind = "sudo-password";
  else if (/(?:\[(?:Y\/n|y\/N|y\/n|yes\/no)\]|\((?:y\/n|yes\/no)\)|(?:continue|proceed)\?\s*)[:?]?\s*$/i.test(tail)) kind = "confirmation";
  else if (/(?:--More--(?:\([^)]*\))?|\(END\)|Press (?:any key|ENTER|RETURN)(?: to [^.]+)?[.:]?)\s*$/i.test(tail)) kind = "pager";
  else if (/^(?:[^\r\n]{0,80}\s)?password:\s*$/i.test(tail)) kind = "password";
  else if (/^(?:=>|U-Boot>)\s*$/.test(tail)) kind = "bootloader";
  /* Some distributions ship a starship/oh-my-zsh prompt: a time prefix, the
   * user@host, then an arrow or a bare % instead of $/#. Keep the user@host
   * anchor (a line that merely ends in an arrow is not a prompt) and allow only
   * a clock prefix, so old boot output cannot be mistaken for a live shell. */
  else if (/^(?:\d{1,2}:\d{2}(?::\d{2})?\s)?(?:[\w.-]+@[\w.-]+:[^\r\n]*|\[[\w.-]+@[\w.-]+ [^\]\r\n]*\]|[\w.-]+@[\w.-]+(?:\s+[^\r\n]{0,120}?)?)\s*(?:[$#%]|\u2192|\u276f|\u03bb|\u00bb)\s*$/u.test(tail)) kind = "shell";
  else if (lines.some((line) => /Kernel panic - not syncing:/.test(line))) kind = "panic";
  if (kind !== "unknown") evidence = kind === "panic"
    ? lines.findLast((line) => /Kernel panic - not syncing:/.test(line)).slice(0, 240)
    : tail.slice(0, 240);
  return { kind, evidence, cursor: log.latestCursor, source: "serial-output-heuristic" };
}
