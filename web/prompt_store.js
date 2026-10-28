/* Board-stored AI prompt.
 *
 * One piece of text lives in the Linkr Bee's NVS, so the custom instructions
 * follow the board instead of one browser profile. Text travels as UTF-8 hex
 * in 48-byte chunks: the management response buffer is small, and hex keeps
 * newlines, spaces and non-ASCII bytes unambiguous.
 *
 * Pure builders and parsers so the panel, the browser tests and the board all
 * share one definition.
 */
import { hexToUtf8, utf8ToHex } from "./board_io.js";

export const PROMPT_MAX_BYTES = 240;
export const PROMPT_CHUNK_BYTES = 48;
export const PROMPT_QUERY_COMMAND = "@p?";
export const PROMPT_CLEAR_COMMAND = "@p clear";

export function normalizePromptText(value) {
  return String(value ?? "").replace(/\r\n?/g, "\n").replace(/^\s+|\s+$/g, "");
}

export function promptFits(text) {
  return new TextEncoder().encode(normalizePromptText(text)).length <= PROMPT_MAX_BYTES;
}

/* "@p get" reads from the start; an explicit offset continues a download. */
export function promptGetCommand({ offset = 0 } = {}) {
  const start = Number(offset ?? 0);
  if (!Number.isInteger(start) || start < 0) {
    throw new Error("prompt offset must be a non-negative integer");
  }
  return start === 0 ? "@p get" : `@p get ${start}`;
}

export function promptSetCommand({ text = "" } = {}) {
  const normalized = normalizePromptText(text);
  if (!normalized) {
    throw new Error("prompt text is empty");
  }
  if (!promptFits(normalized)) {
    throw new Error(`prompt text is longer than ${PROMPT_MAX_BYTES} bytes`);
  }
  return `@p set ${utf8ToHex(normalized)}`;
}

/* "OK prompt=140" */
export function parsePromptStatus(text) {
  const line = String(text ?? "")
    .split(/\r?\n/)
    .map((candidate) => candidate.trim())
    .find((candidate) => /^OK\s+prompt=\d+/i.test(candidate));
  if (!line) return null;
  return { bytes: Number(line.match(/\bprompt=(\d+)/i)?.[1] ?? 0) };
}

/* "OK p off=96 hex=... total=240 done=no" */
export function parsePromptChunk(text) {
  const line = String(text ?? "")
    .split(/\r?\n/)
    .map((candidate) => candidate.trim())
    .find((candidate) => /^OK\s+p\b/i.test(candidate));
  if (!line) return null;
  const offset = Number(line.match(/\boff=(\d+)/i)?.[1]);
  const total = Number(line.match(/\btotal=(\d+)/i)?.[1]);
  const done = /\bdone=yes\b/i.test(line);
  const hex = line.match(/\bhex=([0-9a-f]*)/i)?.[1] ?? "";
  let chunk;
  try {
    chunk = hexToUtf8(hex);
  } catch {
    return null;
  }
  return {
    offset: Number.isFinite(offset) ? offset : 0,
    total: Number.isFinite(total) ? total : 0,
    done,
    hex,
    chunk,
  };
}

/* Assemble the chunks of one prompt, refusing a short or out-of-order read. */
export function assemblePrompt(chunks, total = 0) {
  const ordered = [...chunks].sort((a, b) => a.offset - b.offset);
  let hex = "";
  let expected = 0;
  for (const chunk of ordered) {
    if (chunk.offset !== expected) {
      throw new Error("prompt chunks are out of order");
    }
    hex += chunk.hex;
    expected += chunk.hex.length / 2;
  }
  if (total && expected !== total) {
    throw new Error("prompt read is incomplete");
  }
  return hexToUtf8(hex);
}
