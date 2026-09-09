const MIN_TERMINAL_COLUMNS = 2;
const MIN_TERMINAL_ROWS = 2;
const MAX_TERMINAL_DIMENSION = 1000;

function clampDimension(value, minimum) {
  const number = Number(value);
  if (!Number.isFinite(number)) return minimum;
  return Math.max(
    minimum,
    Math.min(MAX_TERMINAL_DIMENSION, Math.floor(number)),
  );
}

export function normalizeTerminalGeometry(cols, rows) {
  const normalizedCols = clampDimension(cols, MIN_TERMINAL_COLUMNS);
  const normalizedRows = clampDimension(rows, MIN_TERMINAL_ROWS);
  return {
    cols: normalizedCols,
    rows: normalizedRows,
    key: `${normalizedCols}x${normalizedRows}`,
  };
}

export function terminalGeometryCommand(cols, rows) {
  const geometry = normalizeTerminalGeometry(cols, rows);
  return `stty rows ${geometry.rows} cols ${geometry.cols} >/dev/null 2>&1\r`;
}

export function looksLikeShellPrompt(line) {
  if (typeof line !== "string" || !/[$#] $/.test(line)) return false;

  const prefix = line.slice(0, -2).trim();
  if (!prefix) return true;

  return (
    /@\S+(?::\S*)?$/.test(prefix) ||
    /^(?:ba|da|a|z)?sh(?:-[\d.]+)?$/.test(prefix) ||
    /^(?:~|\/\S*)$/.test(prefix) ||
    /^\[[^\]]+\]$/.test(prefix)
  );
}

// Track incoming UART text, including prompts split across notifications. A
// target can reboot or start getty again without disconnecting the accessory.
export function createTerminalSessionTracker() {
  let line = "";
  let matched = false;
  return {
    reset() { line = ""; matched = false; },
    push(text) {
      let restarted = false;
      for (const char of text) {
        if (char === "\r" || char === "\n") {
          line = "";
          matched = false;
          continue;
        }
        line = (line + char).slice(-1024);
        if (!matched && (/^(?:[^\s:]+\s+)?login:\s*$/i.test(line) ||
            /^(?:\[\s*\d+(?:\.\d+)?\]\s*)?Linux version\s/.test(line))) {
          restarted = true;
          matched = true;
        }
      }
      return restarted;
    },
  };
}
