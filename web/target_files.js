/* Target file reads and uploads: the shell protocol only.
 *
 * `cat` is the wrong way to read a target file through this bridge. The
 * accessory's UART RX ring is 8192 bytes and the browser journal keeps 128 KiB,
 * so a single large file evicts the console history the operator is watching,
 * and there is no way at all to push a local file the other way. This module is
 * the shell half of the alternative and nothing else:
 *
 *   - page a file with `dd bs=1 skip=N count=M | base64` and parse the page;
 *   - upload a file as base64 chunks appended to a sibling part file, verify
 *     the part file, and only then move it over the destination.
 *
 * It is pure: no imports, no DOM, no serial port, no storage. The tool layer
 * decides when a command may be sent, the approval UI decides whether the user
 * agreed, and the caller owns the bytes (uploadPlan only encodes what it is
 * handed).
 *
 * MARKERS. Every marker is one whole line and is matched only as a complete
 * line, so the shell's echo of a command can never be mistaken for its output:
 * the echoed text always carries its surrounding quoting on the same line.
 * `printf` prints every marker, never `echo -e`, because busybox `echo` has no
 * portable escape handling.
 *
 *   LINKR_FILE:begin total=<n> from=<offset>    size known, page starts here
 *   <base64 payload, wrapped; whitespace ignored>
 *   LINKR_FILE:end bytes=<n>                    n bytes were really read
 *   LINKR_FILE:error missing                    no such file
 *   LINKR_FILE:error directory                  path is a directory
 *   LINKR_FILE:error not-absolute               path does not start with /
 *   LINKR_FILE:error denied                     exists but is not readable
 *   LINKR_FILE:error not-regular                fifo/device: no size, no end
 *
 *   LINKR_UPLOAD:chunk index=<i> offset=<o> bytes=<n> total=<m>    append done
 *   LINKR_UPLOAD:bytes=<n>                      measured part-file size
 *   LINKR_UPLOAD:sha256=<hash>|unavailable      verifyCommand result
 *   LINKR_UPLOAD:complete                       part file moved into place
 *   LINKR_UPLOAD:missing                        part file does not exist
 *   LINKR_UPLOAD:error size-mismatch actual=<n>
 *   LINKR_UPLOAD:error hash-mismatch actual=<hash>
 *
 * A read that runs past a shorter file still ends with `end bytes=<n>`, just
 * with a smaller n: that is a normal short page, status ok. A read that dies
 * before printing `end` is incomplete, so "the file is shorter than requested"
 * and "the probe failed" never collapse into each other.
 */

/* One page must fit comfortably in a single console round trip: 1024 raw bytes
 * encode to 1368 base64 characters, and with the markers and the read command
 * itself the whole exchange stays around 2 KiB, well inside the 8192-byte ring
 * and small enough that the decoded page cannot bury the journal. */
export const MAX_READ_BYTES = 1024;
export const DEFAULT_CHUNK_BYTES = 720;
export const MAX_CHUNK_BYTES = 2048;
export const PART_SUFFIX = ".linkr-part";

/* Command-size budget for one upload chunk.
 *
 * base64 turns 3 bytes into 4 characters, so a chunk of c bytes costs
 * 4*ceil(c/3) payload characters: 960 at the 720-byte default and 2732 at the
 * 2048-byte maximum. The upload payload is deliberately unwrapped (one command
 * per chunk, so there is nothing to re-wrap). The fixed frame around it --
 * assigning the part path, `printf '%s' '<base64>' | base64 -d >> "$p"`, a
 * `wc -c` of the part file and the confirmation marker -- measured 173
 * characters with a 24-character quoted part path. Half of the 8192-byte ring
 * is the ceiling because the ring also carries the shell's echo of the command
 * and the next prompt; a command that filled the ring could be cut mid-line and
 * silently swallow the following chunk. MAX_UPLOAD_COMMAND_BYTES leaves room
 * for a ~1 KB path on top of the maximum chunk.
 */
export const MAX_UPLOAD_COMMAND_BYTES = 4096;

/* A plan is built in memory before anything is sent, so a size that implies an
 * absurd chunk list (a model hallucinating 1 TB) must fail here rather than
 * freeze the panel. 20000 chunks is ~14 MiB at the default chunk size. */
export const MAX_UPLOAD_CHUNKS = 20000;

/* Which target tools the commands below need. The caller probes once, like
 * download_plan.js does, and reports what is missing instead of sending a
 * command that dies halfway. `shasum` is only needed when a sha256 is checked. */
export const TARGET_FILE_PROBE = `for t in dd base64 wc tr sha256sum shasum; do command -v "$t" >/dev/null 2>&1 && printf 'LINKR_TOOL:%s\\n' "$t"; done; :`;

const READ_BEGIN = /^LINKR_FILE:begin total=(\d+) from=(\d+)$/;
const READ_END = /^LINKR_FILE:end bytes=(\d+)$/;
const READ_ERROR = /^LINKR_FILE:error ([a-z-]+)$/;
const UPLOAD_CHUNK = /^LINKR_UPLOAD:chunk index=(\d+) offset=(\d+) bytes=(\d+) total=(\d+)$/;

/* An error marker the shell can print before a read starts maps to the status
 * the caller sees. `not-absolute` and `not-regular` are both refusals, so they
 * share `denied`; the distinct detail survives in `reason`. */
const READ_FAILURES = {
  missing: ["missing", "Target file does not exist."],
  directory: ["directory", "Target path is a directory."],
  "not-absolute": ["denied", "Target path is not absolute."],
  denied: ["denied", "Target file is not readable."],
  "not-regular": ["denied", "Target path is not a regular file."],
};

export function quoteShell(text) {
  if (typeof text !== "string") throw new Error("Shell quoting needs a string.");
  // A NUL cannot cross argv at all, and a shell would drop it silently: refuse
  // rather than send a command that names a different path than the caller asked for.
  if (text.includes("\0")) throw new Error("Shell quoting cannot represent a NUL byte.");
  return "'" + text.replaceAll("'", "'\\''") + "'";
}

export function formatBytes(n) {
  if (!Number.isFinite(n) || n < 0) throw new Error("Byte count must be a non-negative number.");
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  let value = n;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  // Whole scaled values read better without a decimal ("8 KiB", not "8.0 KiB").
  return `${unit === 0 || Number.isInteger(value) ? value : value.toFixed(1)} ${units[unit]}`;
}

const B64_ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const B64_VALUE = new Map([...B64_ALPHABET].map((char, index) => [char, index]));

/* Encoder and decoder live here instead of using atob/Buffer so the same code
 * runs in the browser, in the mobile WebView and under `node --test`, and so
 * the decoder can refuse anything `base64 -d` would refuse. Both are exported:
 * the tool layer may want to slice and encode a file itself. */
export function encodeBase64(bytes) {
  if (!(bytes instanceof Uint8Array)) throw new Error("encodeBase64 expects a Uint8Array.");
  let text = "";
  for (let i = 0; i < bytes.length; i += 3) {
    const a = bytes[i];
    const b = bytes[i + 1] ?? 0;
    const c = bytes[i + 2] ?? 0;
    text += B64_ALPHABET[a >> 2];
    text += B64_ALPHABET[((a & 3) << 4) | (b >> 4)];
    text += i + 1 < bytes.length ? B64_ALPHABET[((b & 15) << 2) | (c >> 6)] : "=";
    text += i + 2 < bytes.length ? B64_ALPHABET[c & 63] : "=";
  }
  return text;
}

// Only called with text that already passed the charset and length checks in parseFileRead.
function decodeBase64(text) {
  const padding = text.endsWith("==") ? 2 : text.endsWith("=") ? 1 : 0;
  const data = new Uint8Array((text.length / 4) * 3 - padding);
  let at = 0;
  for (let i = 0; i < text.length; i += 4) {
    const a = B64_VALUE.get(text[i]);
    const b = B64_VALUE.get(text[i + 1]);
    const c = text[i + 2] === "=" ? 0 : B64_VALUE.get(text[i + 2]);
    const d = text[i + 3] === "=" ? 0 : B64_VALUE.get(text[i + 3]);
    data[at++] = (a << 2) | (b >> 4);
    if (text[i + 2] !== "=") data[at++] = ((b & 15) << 4) | (c >> 2);
    if (text[i + 3] !== "=") data[at++] = ((c & 3) << 6) | d;
  }
  return data;
}

// Serial consoles send CRLF, and a bare CR is its own line break on progress output.
const markerLines = (text) => String(text ?? "").replace(/\r\n?/g, "\n").split("\n").map((line) => line.trimEnd());

function readPath(value) {
  if (typeof value !== "string" || value === "") throw new Error("Target path must be a non-empty string.");
  /* Control characters (CR/LF/NUL) would break the line-oriented markers and
   * could let a crafted path forge a marker line. A path without a leading `/`
   * is deliberately let through: the shell reports it as not-absolute so the
   * caller gets a diagnosis instead of an exception. */
  if (/[\x00-\x1f\x7f]/.test(value)) throw new Error("Target path must not contain control characters.");
  return value;
}

function readOffset(value) {
  if (!Number.isSafeInteger(value) || value < 0) throw new Error("Read offset must be a non-negative integer.");
  return value;
}

function readCount(value) {
  if (!Number.isSafeInteger(value) || value <= 0) throw new Error("Read size must be a positive integer.");
  if (value > MAX_READ_BYTES) throw new Error(`Read size must not exceed ${MAX_READ_BYTES} bytes.`);
  return value;
}

/* Read at most `bytes` bytes from `offset`.
 *
 * The page is clamped in the shell from the size `wc -c` reports, and that
 * clamped count is what `end bytes=` announces. busybox and coreutils `dd`
 * disagree on the wording of their stderr summary, so the count is computed
 * here instead of scraped from it; parseFileRead then re-checks the announced
 * count against the decoded payload, which catches a wrong clamp too.
 *
 * Every refusal `exit 0`s on purpose: a missing file is a result the caller
 * reads from the marker, not a transport failure that should make the tool
 * layer report the console as broken.
 *
 * Paging is O(offset) on the target: `bs=1 skip=N` reads through the file. A
 * caller walking a large file should page sequentially, not jump around.
 */
export function readFileCommand({ path, offset = 0, bytes } = {}) {
  const target = quoteShell(readPath(path));
  const from = readOffset(offset);
  const count = readCount(bytes);
  return [
    `p=${target}`,
    // `case` rather than `[[ ]]`: dash, ash and busybox sh all support it.
    `case "$p" in /*) ;; *) printf '\\nLINKR_FILE:error not-absolute\\n'; exit 0 ;; esac`,
    `[ -e "$p" ] || { printf '\\nLINKR_FILE:error missing\\n'; exit 0; }`,
    `[ -d "$p" ] && { printf '\\nLINKR_FILE:error directory\\n'; exit 0; }`,
    `[ -r "$p" ] || { printf '\\nLINKR_FILE:error denied\\n'; exit 0; }`,
    // A fifo or character device has no end and makes `wc -c` block forever.
    `[ -f "$p" ] || { printf '\\nLINKR_FILE:error not-regular\\n'; exit 0; }`,
    // `tr -d` because some `wc -c` builds pad the number into a column.
    `t=$(wc -c < "$p" | tr -d ' \\t') || { printf '\\nLINKR_FILE:error denied\\n'; exit 0; }`,
    `n=$((t - ${from})); [ "$n" -gt 0 ] || n=0`,
    `[ "$n" -gt ${count} ] && n=${count}`,
    `printf '\\nLINKR_FILE:begin total=%s from=${from}\\n' "$t"`,
    `dd bs=1 skip=${from} count=$n < "$p" 2>/dev/null | base64`,
    `printf '\\nLINKR_FILE:end bytes=%s\\n' "$n"`,
  ].join('; ');
}

export function parseFileRead(text) {
  const lines = markerLines(text);
  /* The LAST begin marker wins: a journal tail can still hold an earlier read
   * of the same file, and a stale complete pair must never be reported as this
   * read's data. With no begin at all, only then is an error marker trusted --
   * a read that started can no longer fail through those early exits. */
  const begin = lines.findLastIndex((line) => READ_BEGIN.test(line));
  const miss = (reason, totalBytes = null, from = null, bytes = null) =>
    ({ status: "incomplete", totalBytes, from, bytes, data: null, reason });
  if (begin < 0) {
    const failure = lines.map((line) => READ_ERROR.exec(line)).find(Boolean);
    if (failure) {
      const [status, reason] = READ_FAILURES[failure[1]] ?? ["incomplete", `Target reported LINKR_FILE:error ${failure[1]}.`];
      return { status, totalBytes: null, from: null, bytes: null, data: null, reason };
    }
    return miss("No LINKR_FILE markers in the output: the command did not run or its output was truncated.");
  }
  const [, totalText, fromText] = READ_BEGIN.exec(lines[begin]);
  const totalBytes = Number(totalText);
  const from = Number(fromText);
  const end = lines.findIndex((line, index) => index > begin && READ_END.test(line));
  if (end < 0) return miss("The read stopped before its end marker: the payload is truncated.", totalBytes, from);
  const endBytes = Number(READ_END.exec(lines[end])[1]);
  // Anything past 2^53 is a corrupted marker, not a file size.
  if (![totalBytes, from, endBytes].every(Number.isSafeInteger)) {
    return miss("The markers do not carry safe byte counts: the output was corrupted.");
  }
  // Wrapped base64 lines are slices of one stream, so join before stripping.
  const payload = lines.slice(begin + 1, end).join("").replace(/\s+/g, "");
  if (payload.length % 4 !== 0 || !/^[A-Za-z0-9+/]*={0,2}$/.test(payload)) {
    return miss("The payload is not valid base64: the transfer was corrupted or truncated.", totalBytes, from, endBytes);
  }
  const data = decodeBase64(payload);
  if (data.length !== endBytes) {
    return miss(`The payload holds ${data.length} bytes but the end marker declared ${endBytes}.`, totalBytes, from, endBytes);
  }
  // A page that starts at or past EOF reports zero bytes from an offset beyond
  // the size, which is a legitimate short page; anything else must fit.
  if (endBytes > 0 && from + endBytes > totalBytes) {
    return miss(`The read claims bytes ${from}..${from + endBytes} of a ${totalBytes}-byte file.`, totalBytes, from, endBytes);
  }
  return { status: "ok", totalBytes, from, bytes: endBytes, data, reason: "" };
}

function uploadPath(value) {
  const path = readPath(value);
  if (!path.startsWith("/")) throw new Error("Target upload path must be absolute.");
  if (path.endsWith("/")) throw new Error("Target upload path must name a file, not a directory.");
  /* Whitespace and quotes are refused rather than quoted away because the part
   * file name is derived from the destination by appending a suffix: a path
   * that cannot be reprinted verbatim is a path an operator cannot check
   * against the target's own `ls` before the move. */
  if (/[\s'"]/.test(path)) throw new Error("Target upload path must not contain whitespace or quote characters.");
  // coreutils `sha256sum` prefixes its line with a backslash when the file name
  // contains one, which would corrupt the LINKR_UPLOAD:sha256 marker.
  if (path.includes("\\")) throw new Error("Target upload path must not contain a backslash.");
  return path;
}

function uploadSize(value) {
  if (!Number.isSafeInteger(value) || value < 0) throw new Error("Upload size must be a non-negative integer.");
  return value;
}

function uploadChunkSize(value) {
  if (!Number.isSafeInteger(value) || value <= 0) throw new Error("Upload chunk size must be a positive integer.");
  if (value > MAX_CHUNK_BYTES) throw new Error(`Upload chunk size must not exceed ${MAX_CHUNK_BYTES} bytes.`);
  return value;
}

// Mirrors download_plan.js: empty means "do not check", anything else must be a full digest.
function uploadSha256(value) {
  const sha256 = String(value ?? "").trim().toLowerCase();
  if (sha256 && !/^[a-f0-9]{64}$/.test(sha256)) throw new Error("Expected SHA-256 must contain 64 hexadecimal characters.");
  return sha256;
}

function uploadStart(value, size) {
  if (!Number.isSafeInteger(value) || value < 0 || value > size) {
    throw new Error("Upload resume offset must be a non-negative integer no greater than the upload size.");
  }
  return value;
}

/* One chunk: decode the base64 and append it to the part file, then report the
 * size the target actually measured. The measured size (not the planned one) is
 * what makes a resume safe: a short write is visible immediately, and
 * parseUploadProgress returns the byte position the target really reached.
 *
 * `fresh` truncates instead of appending, so a stale part file from an older
 * attempt cannot be extended into a corrupt upload. Resume passes fresh:false.
 */
export function uploadChunkCommand({ tempPath, index, offset, bytes, base64, fresh = offset === 0 }) {
  const part = uploadPath(tempPath);
  for (const [name, value] of [["index", index], ["offset", offset]]) {
    if (!Number.isSafeInteger(value) || value < 0) throw new Error(`Upload chunk ${name} must be a non-negative integer.`);
  }
  if (!Number.isSafeInteger(bytes) || bytes <= 0) throw new Error("Upload chunk length must be a positive integer.");
  if (typeof base64 !== "string" || base64.length % 4 !== 0 || !/^[A-Za-z0-9+/]*={0,2}$/.test(base64)) {
    throw new Error("Upload chunk payload must be padded base64.");
  }
  return `p=${quoteShell(part)}; printf '%s' ${quoteShell(base64)} | base64 -d ${fresh ? ">" : ">>"} "$p" && m=$(wc -c < "$p" | tr -d ' \\t') && printf '\\nLINKR_UPLOAD:chunk index=${index} offset=${offset} bytes=${bytes} total=%s\\n' "$m"`;
}

function uploadVerifyCommand({ tempPath, sha256 }) {
  const parts = [
    `p=${quoteShell(tempPath)}`,
    `[ -f "$p" ] || { printf '\\nLINKR_UPLOAD:missing\\n'; exit 0; }`,
    `n=$(wc -c < "$p" | tr -d ' \\t') || { printf '\\nLINKR_UPLOAD:missing\\n'; exit 0; }`,
    `printf '\\nLINKR_UPLOAD:bytes=%s\\n' "$n"`,
  ];
  // The hash costs a full read of the part file, so it is only computed when a
  // digest is actually waiting to be compared against.
  if (sha256) parts.push(
    `if command -v sha256sum >/dev/null 2>&1; then h=$(sha256sum "$p" || printf unavailable); elif command -v shasum >/dev/null 2>&1; then h=$(shasum -a 256 "$p" || printf unavailable); else h=unavailable; fi`,
    // Both printers write "<hash>  <file>"; keep only the digest column.
    `h=\${h%% *}`,
    `printf '\\nLINKR_UPLOAD:sha256=%s\\n' "$h"`,
  );
  return parts.join('; ');
}

/* The only command that touches the destination, and it runs only after
 * re-reading the part file and confirming both the size and, when one was
 * expected, the digest. The digest is checked again here instead of being
 * trusted from an earlier verifyCommand: the move is the irreversible step, and
 * one more read costs milliseconds next to the UART transfer that produced the
 * file. That is also what lets parseUploadResult accept the output of this
 * command on its own: every ok it reports carries the same evidence.
 * Exit 65 covers a wrong size or digest, 66 a missing part file; the marker
 * says which. */
function uploadCompleteCommand({ path, tempPath, size, sha256 }) {
  const parts = [
    `t=${quoteShell(tempPath)}`,
    `d=${quoteShell(path)}`,
    `[ -f "$t" ] || { printf '\\nLINKR_UPLOAD:missing\\n'; exit 66; }`,
    `n=$(wc -c < "$t" | tr -d ' \\t') || { printf '\\nLINKR_UPLOAD:missing\\n'; exit 66; }`,
    `[ "$n" = '${size}' ] || { printf '\\nLINKR_UPLOAD:error size-mismatch actual=%s\\n' "$n"; exit 65; }`,
  ];
  if (sha256) parts.push(
    `if command -v sha256sum >/dev/null 2>&1; then h=$(sha256sum "$t" || printf unavailable); elif command -v shasum >/dev/null 2>&1; then h=$(shasum -a 256 "$t" || printf unavailable); else h=unavailable; fi`,
    `h=\${h%% *}`,
    `printf '\\nLINKR_UPLOAD:sha256=%s\\n' "$h"`,
    `[ "$h" = ${quoteShell(sha256)} ] || { printf '\\nLINKR_UPLOAD:error hash-mismatch actual=%s\\n' "$h"; exit 65; }`,
  );
  parts.push(
    `printf '\\nLINKR_UPLOAD:bytes=%s\\n' "$n"`,
    `mv -f "$t" "$d" && printf '\\nLINKR_UPLOAD:complete\\n'`,
  );
  return parts.join('; ');
}

/* Plan a chunked upload.
 *
 * `data` is optional: without it the plan still carries the geometry, the part
 * path and the verify/complete commands, but `chunks[].command` and
 * `chunks[].base64` are null because only the caller has the bytes. Pass the
 * remaining bytes (exactly size - startAt of them) to get runnable commands, or
 * build them lazily with uploadChunkCommand while streaming a file.
 *
 * `startAt` is how a resume reuses the same part file: chunks below that offset
 * are not emitted, the part file must already hold exactly those bytes, and
 * `prepareCommand` is null so no caller can truncate it by accident. The resume
 * point need not sit on a chunk boundary; the first emitted chunk is then short.
 */
export function uploadPlan({ path, size, chunkBytes = DEFAULT_CHUNK_BYTES, sha256 = "", startAt = 0, data = null } = {}) {
  const destination = uploadPath(path);
  const total = uploadSize(size);
  const chunk = uploadChunkSize(chunkBytes);
  const expected = uploadSha256(sha256);
  const from = uploadStart(startAt, total);
  const tempPath = destination + PART_SUFFIX;
  const remaining = total - from;
  if (data !== null) {
    if (!(data instanceof Uint8Array)) throw new Error("Upload data must be a Uint8Array.");
    if (data.length !== remaining) throw new Error(`Upload data holds ${data.length} bytes but ${remaining} bytes remain to send.`);
  }
  if (Math.ceil(remaining / chunk) > MAX_UPLOAD_CHUNKS) {
    throw new Error(`Upload would need more than ${MAX_UPLOAD_CHUNKS} chunks; send a smaller file or a larger chunk size.`);
  }
  const chunks = [];
  for (let offset = from; offset < total; offset += chunk) {
    const bytes = Math.min(chunk, total - offset);
    const entry = { index: Math.floor(offset / chunk), offset, bytes, base64: null, command: null };
    /* The frame length depends only on the encoded size, so the budget is
     * checked even before the caller hands over the bytes: a destination that
     * cannot work should fail while planning, not halfway through sending. */
    const encoded = data !== null
      ? encodeBase64(data.subarray(offset - from, offset - from + bytes))
      : "A".repeat(Math.ceil(bytes / 3) * 4);
    const command = uploadChunkCommand({ tempPath, index: entry.index, offset, bytes, base64: encoded });
    if (command.length > MAX_UPLOAD_COMMAND_BYTES) {
      throw new Error(`Upload command would be ${command.length} characters, over the ${MAX_UPLOAD_COMMAND_BYTES}-character UART budget; shorten the path or lower chunkBytes.`);
    }
    if (data !== null) {
      entry.base64 = encoded;
      entry.command = command;
    }
    chunks.push(entry);
  }
  return {
    path: destination,
    size: total,
    tempPath,
    from,
    chunkBytes: chunk,
    expectedSha256: expected,
    chunks,
    // Creates the part file before the first chunk, and is how a zero-byte
    // upload gets a file to verify and move. Must not run when resuming.
    prepareCommand: from === 0 ? `: > ${quoteShell(tempPath)}` : null,
    verifyCommand: uploadVerifyCommand({ tempPath, sha256: expected }),
    completeCommand: uploadCompleteCommand({ path: destination, tempPath, size: total, sha256: expected }),
  };
}

/* Read back the per-chunk confirmations.
 *
 * ok means every confirmation that arrived is contiguous with the previous one
 * and matches its own announced length -- it does NOT mean the whole plan
 * arrived. The caller compares nextOffset with plan.size (or trusts
 * verifyCommand) to decide whether to resume. nextOffset is null when nothing
 * consistent was confirmed, so a resume never starts from a guessed offset.
 */
export function parseUploadProgress(text) {
  const markers = markerLines(text)
    .map((line) => UPLOAD_CHUNK.exec(line))
    .filter(Boolean)
    .map((match) => ({ index: Number(match[1]), offset: Number(match[2]), bytes: Number(match[3]), total: Number(match[4]) }));
  if (markers.length === 0) {
    return { status: "incomplete", count: 0, from: null, bytes: 0, nextOffset: null, markers, reason: "No LINKR_UPLOAD:chunk confirmations in the output." };
  }
  const fail = (reason) => ({ status: "incomplete", count: markers.length, from: markers[0].offset, bytes: 0, nextOffset: null, markers, reason });
  for (let i = 0; i < markers.length; i += 1) {
    const marker = markers[i];
    const startedAt = i === 0 ? marker.offset : markers[i - 1].total;
    if (marker.offset !== startedAt) {
      return fail(`Chunk ${marker.index} starts at ${marker.offset} but the target reported ${startedAt} bytes: run verifyCommand and resume from its byte count.`);
    }
    if (marker.total !== marker.offset + marker.bytes) {
      return fail(`Chunk ${marker.index} appended ${marker.total - marker.offset} bytes instead of ${marker.bytes}: run verifyCommand and resume from its byte count.`);
    }
  }
  const last = markers.at(-1);
  return { status: "ok", count: markers.length, from: markers[0].offset, bytes: last.total - markers[0].offset, nextOffset: last.total, markers, reason: "" };
}

/* Read back verifyCommand or completeCommand output.
 *
 * Pass the plan (or { size, sha256 }) so the byte count and digest can be
 * compared against what was intended. Without an expected size the only
 * acceptable evidence is the LINKR_UPLOAD:complete marker, which the target
 * prints only after the move: a bare count on its own proves nothing. A
 * malformed expectation throws instead of quietly switching the check off.
 */
export function parseUploadResult(text, expected = {}) {
  const wanted = expected?.size;
  const expectSize = wanted === undefined || wanted === null ? null : uploadSize(wanted);
  const expectSha = uploadSha256(expected?.sha256 ?? expected?.expectedSha256 ?? "");
  let bytes = null;
  let sha256 = "";
  let shaUnavailable = false;
  let missing = false;
  let moved = false;
  let sizeMismatch = null;
  let hashMismatch = false;
  for (const line of markerLines(text)) {
    if (line === "LINKR_UPLOAD:missing") missing = true;
    else if (line === "LINKR_UPLOAD:complete") moved = true;
    else if (line === "LINKR_UPLOAD:sha256=unavailable") shaUnavailable = true;
    else {
      let match = /^LINKR_UPLOAD:bytes=(\d+)$/.exec(line);
      if (match) { bytes = Number(match[1]); continue; }
      match = /^LINKR_UPLOAD:sha256=([0-9a-f]{64})$/.exec(line);
      if (match) { sha256 = match[1]; continue; }
      match = /^LINKR_UPLOAD:error size-mismatch(?: actual=(\d+))?$/.exec(line);
      if (match) { sizeMismatch = match[1] === undefined ? null : Number(match[1]); continue; }
      match = /^LINKR_UPLOAD:error hash-mismatch(?: actual=(\S*))?$/.exec(line);
      if (match) { hashMismatch = true; if (/^[0-9a-f]{64}$/.test(match[1] ?? "")) sha256 = match[1]; }
    }
  }
  const result = { status: "incomplete", bytes: bytes ?? 0, sha256, expectedSha256: expectSha, moved, reason: "" };
  const mismatch = (reason) => ({ ...result, status: "mismatch", reason });
  const incomplete = (reason) => ({ ...result, status: "incomplete", reason });
  // Most specific failure first: a visible digest mismatch is a mismatch even
  // when the command stopped before it could report a byte count.
  if (missing) return incomplete("The target has no part file for this upload; nothing was written.");
  if (sizeMismatch !== null) {
    return { ...mismatch(`The target reported ${sizeMismatch} bytes${expectSize === null ? "" : ` instead of ${expectSize}`}.`), bytes: sizeMismatch };
  }
  if (expectSize !== null && bytes !== null && bytes !== expectSize) return mismatch(`The target reported ${bytes} bytes instead of ${expectSize}.`);
  if (expectSha) {
    if (hashMismatch || (sha256 && sha256 !== expectSha)) return mismatch(`The target's sha256 is ${sha256 || "unreadable"}, not ${expectSha}.`);
    if (shaUnavailable) return incomplete("The target has neither sha256sum nor shasum, so the expected digest could not be checked.");
    if (!sha256) return incomplete("The output has no sha256 result, so the expected digest could not be checked.");
  } else if (hashMismatch) {
    return mismatch("The target reported a sha256 mismatch.");
  }
  if (expectSize !== null && bytes === null) return incomplete("The output has no byte count, so the upload cannot be confirmed.");
  if (expectSize === null && !moved) {
    return incomplete("Without an expected size only the LINKR_UPLOAD:complete marker confirms an upload; pass the plan or { size }.");
  }
  return { ...result, status: "ok", reason: "" };
}
