/* Board-stored AI configuration.
 *
 * The app serialises the model configuration and keeps it in the Linkr Bee's
 * NVS, so a configured board can move between phones and browsers. The blob is
 * uploaded in chunks (begin, put…, end) because one management command is
 * small; reading works the same way. The API key and custom headers are only
 * included when the user explicitly opts in, and the UI says they are readable
 * by any paired host.
 *
 * Pure builders, parsers and serialisation so the settings panel, the tests
 * and the board share one contract.
 */
import { hexToUtf8, utf8ToHex } from "./board_io.js";

export const AICONF_QUERY_COMMAND = "@y?";
export const AICONF_BEGIN_COMMAND = "@y begin";
export const AICONF_END_COMMAND = "@y end";
export const AICONF_CLEAR_COMMAND = "@y clear";
export const AICONF_CHUNK_BYTES = 48;
export const AICONF_PUT_BYTES = 160;
export const AICONF_MAX_BYTES = 960;
export const AICONF_VERSION = 1;

export function aiconfPutCommand(bytes) {
  const hex = typeof bytes === "string" ? bytes : utf8ToHex(bytes);
  return `@y put ${hex}`;
}

export function aiconfGetCommand({ offset = 0 } = {}) {
  const start = Number(offset ?? 0);
  if (!Number.isInteger(start) || start < 0) {
    throw new Error("aiconf offset must be a non-negative integer");
  }
  return `@y get ${start}`;
}

/* "OK aiconf bytes=140 max=960" */
export function parseAiconfStatus(text) {
  const line = String(text ?? "")
    .split(/\r?\n/)
    .map((candidate) => candidate.trim())
    .find((candidate) => /^OK\s+aiconf\s+bytes=/i.test(candidate));
  if (!line) return null;
  return {
    bytes: Number(line.match(/\bbytes=(\d+)/i)?.[1] ?? 0),
    max: Number(line.match(/\bmax=(\d+)/i)?.[1] ?? AICONF_MAX_BYTES),
  };
}

/* "OK aiconf off=48 hex=... total=140 done=no" */
export function parseAiconfChunk(text) {
  const line = String(text ?? "")
    .split(/\r?\n/)
    .map((candidate) => candidate.trim())
    .find((candidate) => /^OK\s+aiconf\s+off=/i.test(candidate));
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

/* One JSON record, versioned so a future schema can be rejected explicitly. */
export function serializeAiconf(config, { includeSecrets = false } = {}) {
  const record = {
    v: AICONF_VERSION,
    endpoint: config.endpoint,
    model: config.model,
    provider: config.provider,
    reasoning: config.reasoning,
    contextWindow: config.contextWindow ?? 0,
    maxTokens: config.maxTokens ?? 0,
  };
  if (includeSecrets) {
    record.apiKey = config.apiKey ?? "";
    record.headers = config.headers ?? {};
  }
  return JSON.stringify(record);
}

/* Parse the blob back; the caller validates it with validateAgentConfig. */
export function parseAiconfPayload(text) {
  let record;
  try {
    record = JSON.parse(String(text ?? ""));
  } catch {
    throw new Error("board configuration is not valid JSON");
  }
  if (!record || typeof record !== "object" || record.v !== AICONF_VERSION) {
    throw new Error("board configuration has an unsupported version");
  }
  return {
    endpoint: record.endpoint,
    model: record.model,
    provider: record.provider,
    reasoning: record.reasoning,
    contextWindow: Number(record.contextWindow) || 0,
    maxTokens: Number(record.maxTokens) || 0,
    apiKey: typeof record.apiKey === "string" ? record.apiKey : "",
    headers: record.headers && typeof record.headers === "object" ? record.headers : {},
    hasSecrets: Boolean(record.apiKey) || Object.keys(record.headers || {}).length > 0,
  };
}

/* Split a JSON string into hex chunks that fit one management command. The
 * split happens on UTF-8 bytes, so a multi-byte character may span two chunks;
 * assembly therefore concatenates hex before decoding. */
export function aiconfHexChunks(text, chunkBytes = AICONF_PUT_BYTES) {
  const bytes = new TextEncoder().encode(String(text ?? ""));
  const chunks = [];
  for (let offset = 0; offset < bytes.length; offset += chunkBytes) {
    chunks.push(Array.from(bytes.slice(offset, offset + chunkBytes),
      (byte) => byte.toString(16).padStart(2, "0")).join(""));
  }
  return chunks;
}

/* Assemble chunk replies read back from the board. */
export function assembleAiconf(chunks, total = 0) {
  const ordered = [...chunks].sort((a, b) => a.offset - b.offset);
  let hex = "";
  let expected = 0;
  for (const chunk of ordered) {
    if (chunk.offset !== expected) {
      throw new Error("board configuration chunks are out of order");
    }
    hex += chunk.hex;
    expected += chunk.hex.length / 2;
  }
  if (total && expected !== total) {
    throw new Error("board configuration read is incomplete");
  }
  return hexToUtf8(hex);
}
