import assert from 'node:assert/strict';
import test from 'node:test';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { DEFAULT_CHUNK_BYTES, MAX_CHUNK_BYTES, MAX_READ_BYTES, MAX_UPLOAD_COMMAND_BYTES, TARGET_FILE_PROBE,
  encodeBase64, formatBytes, parseFileRead, parseUploadProgress, parseUploadResult, quoteShell,
  readFileCommand, uploadChunkCommand, uploadPlan } from '../../web/target_files.js';

// The target's `base64` wraps at 76 columns; fixtures wrap deliberately so the
// parser is exercised against the shape a real console produces, not against
// one long unwrapped line that only the host's base64 emits.
const wrap76 = (text) => text.replace(/(.{76})/g, '$1\n');

// A whole console capture: prompt noise, the start marker, the payload, the end
// marker and the next prompt, with CRLF as the serial console sends it.
const readFixture = (bytes, { total = bytes.length, from = 0, payload = Buffer.from(bytes).toString('base64'), end = bytes.length } = {}) =>
  `root@target:~# dd bs=1 skip=${from} count=${bytes.length} < /f 2>/dev/null | base64\r\n` +
  `LINKR_FILE:begin total=${total} from=${from}\r\n${wrap76(payload)}\r\nLINKR_FILE:end bytes=${end}\r\nroot@target:~# `;

const sh = (command, cwd = undefined) => spawnSync('sh', ['-c', command], { cwd, encoding: 'utf8' });
const tempDir = (tag) => mkdtempSync(join(tmpdir(), `linkr-target-files-${tag}-`));
const sha256Of = (bytes) => createHash('sha256').update(bytes).digest('hex');
const bytesOf = (length, seed = 1) => Uint8Array.from({ length }, (_, index) => (index * 31 + seed * 7) & 0xff);

test('the read command reports every refusal in the output and quotes the path', () => {
  const command = readFileCommand({ path: "/tmp/it's a file.bin", bytes: 32 });
  assert.ok(command.includes(`p=${quoteShell("/tmp/it's a file.bin")}`), 'the path must go through quoteShell');
  assert.ok(command.includes(`'/tmp/it'\\''s a file.bin'`), 'an apostrophe must be closed and reopened');
  // A relative path is a runtime diagnosis, not an exception: the shell case
  // guard is what makes `read_file` on a relative path explain itself.
  assert.ok(readFileCommand({ path: 'relative/f.bin', bytes: 8 }).includes("printf '\\nLINKR_FILE:error not-absolute\\n'"));
  assert.ok(command.includes("printf '\\nLINKR_FILE:error missing\\n'"));
  assert.ok(command.includes('[ -e "$p" ]'));
  assert.ok(command.includes("printf '\\nLINKR_FILE:error directory\\n'"));
  assert.ok(command.includes('[ -d "$p" ]'));
  assert.ok(command.includes("printf '\\nLINKR_FILE:error denied\\n'"));
  assert.ok(command.includes('[ -r "$p" ]'));
  assert.ok(command.includes("printf '\\nLINKR_FILE:error not-regular\\n'"));
  assert.ok(command.includes('[ -f "$p" ]'));
  assert.ok(!command.includes('echo -e'), 'markers must use printf on busybox');
});

test('the read command clamps the page and passes busybox dd arguments', () => {
  const command = readFileCommand({ path: '/var/log/messages', offset: 1024, bytes: 512 });
  assert.ok(command.includes("t=$(wc -c < \"$p\" | tr -d ' \\t')"), 'the size comes from wc, stripped of padding');
  assert.ok(command.includes('n=$((t - 1024))'), 'the page is clamped to what is left of the file');
  assert.ok(command.includes('[ "$n" -gt 0 ] || n=0'), 'a page at or past EOF must read zero bytes, not fail');
  assert.ok(command.includes('[ "$n" -gt 512 ] && n=512'), 'the page is clamped to the requested count');
  assert.ok(command.includes("printf '\\nLINKR_FILE:begin total=%s from=1024\\n' \"$t\""));
  assert.ok(command.includes('dd bs=1 skip=1024 count=$n < "$p" 2>/dev/null | base64'));
  assert.ok(command.includes("printf '\\nLINKR_FILE:end bytes=%s\\n' \"$n\""));
  assert.equal(readFileCommand({ path: '/f', bytes: 8 }).includes('echo'), false);
});

test('read sizes, offsets and paths are validated', () => {
  const path = '/tmp/a';
  assert.equal(MAX_READ_BYTES, 1024);
  for (const bytes of [0, -1, 1.5, '10', undefined, null, NaN, Infinity]) {
    assert.throws(() => readFileCommand({ path, bytes }), /Read size must be a positive integer/);
  }
  assert.throws(() => readFileCommand({ path, bytes: MAX_READ_BYTES + 1 }), /Read size must not exceed 1024 bytes/);
  for (const offset of [-1, 0.5, '0', null, NaN, Number.MAX_SAFE_INTEGER + 2]) {
    assert.throws(() => readFileCommand({ path, bytes: 8, offset }), /Read offset must be a non-negative integer/);
  }
  assert.equal(readFileCommand({ path, bytes: 8 }).includes('skip=0'), true, 'offset defaults to 0');
  assert.throws(() => readFileCommand({ path: 42, bytes: 8 }), /non-empty string/);
  assert.throws(() => readFileCommand({ path: '', bytes: 8 }), /non-empty string/);
  for (const bad of ['/tmp/a\nb', '/tmp/a\rb', '/tmp/a\u0000b']) {
    assert.throws(() => readFileCommand({ path: bad, bytes: 8 }), /control characters/);
  }
  assert.ok(readFileCommand({ path, bytes: 8, offset: undefined }).includes('skip=0'), 'an explicit undefined offset still falls back to 0');
});

test('a full read decodes, including wrapped base64 and a file shorter than requested', () => {
  const body = Uint8Array.from({ length: 300 }, (_, index) => index & 0xff);
  const full = parseFileRead(readFixture(body));
  assert.equal(full.status, 'ok');
  assert.equal(full.totalBytes, 300);
  assert.equal(full.from, 0);
  assert.equal(full.bytes, 300);
  assert.equal(full.reason, '');
  assert.deepEqual(full.data, body);

  // Asked for 1024 bytes, the file only had 300: still ok, and the caller sees
  // a short page rather than a failure.
  const short = parseFileRead(readFixture(body, { total: 300 }));
  assert.equal(short.status, 'ok');
  assert.equal(short.bytes, 300);
  assert.equal(short.data.length, 300);
  assert.ok(short.bytes < MAX_READ_BYTES);
  assert.equal(short.data.length, short.bytes, 'the caller can page from totalBytes/from/bytes alone');

  const tail = parseFileRead(readFixture(body.subarray(200, 260), { total: 300, from: 200 }));
  assert.equal(tail.status, 'ok');
  assert.equal(tail.from, 200);
  assert.equal(tail.totalBytes, 300);
  assert.deepEqual(tail.data, body.subarray(200, 260));

  const empty = parseFileRead(readFixture(new Uint8Array(0), { total: 0, from: 4096 }));
  assert.equal(empty.status, 'ok');
  assert.equal(empty.bytes, 0);
  assert.deepEqual(empty.data, new Uint8Array(0), 'a page past EOF is an empty page, not a null payload');
});

test('truncated, corrupt and marker-less reads are never ok', () => {
  const body = Uint8Array.from({ length: 64 }, (_, index) => index);

  const truncated = parseFileRead(readFixture(body).replace(/\r\nLINKR_FILE:end bytes=64\r\n.*$/s, '\r\n'));
  assert.equal(truncated.status, 'incomplete');
  assert.equal(truncated.data, null);
  assert.match(truncated.reason, /truncated/);

  const cutLine = parseFileRead(`LINKR_FILE:begin total=64 from=0\n${Buffer.from(body).toString('base64').slice(0, 40)}\nLINKR_FILE:end bytes=64\n`);
  assert.equal(cutLine.status, 'incomplete', 'a half payload keeps its length a multiple of 4 here, so the byte check must catch it');

  const invalid = parseFileRead('LINKR_FILE:begin total=3 from=0\n!!!!\nLINKR_FILE:end bytes=3\n');
  assert.equal(invalid.status, 'incomplete');
  assert.equal(invalid.data, null);
  assert.match(invalid.reason, /base64/);

  const mismatch = parseFileRead('LINKR_FILE:begin total=10 from=0\nQUJD\nLINKR_FILE:end bytes=10\n');
  assert.equal(mismatch.status, 'incomplete');
  assert.equal(mismatch.data, null);
  assert.match(mismatch.reason, /3 bytes but the end marker declared 10/);

  const beyondEof = parseFileRead('LINKR_FILE:begin total=2 from=0\nQUJD\nLINKR_FILE:end bytes=3\n');
  assert.equal(beyondEof.status, 'incomplete', 'markers claiming bytes past the file size are inconsistent');
  assert.match(beyondEof.reason, /2-byte file/);

  const none = parseFileRead('root@target:~# cat /nope\ncat: /nope: No such file or directory\n');
  assert.equal(none.status, 'incomplete');
  assert.equal(none.totalBytes, null);
  assert.match(none.reason, /No LINKR_FILE markers/);
  assert.equal(parseFileRead('').status, 'incomplete');
  assert.equal(parseFileRead(null).status, 'incomplete');

  const absurd = parseFileRead(`LINKR_FILE:begin total=${'9'.repeat(20)} from=0\n\nLINKR_FILE:end bytes=0\n`);
  assert.equal(absurd.status, 'incomplete', 'a byte count past 2^53 is a corrupted marker, not a file size');
  assert.match(absurd.reason, /safe byte counts/);
});

test('shell refusals map to distinct statuses', () => {
  const cases = [
    ['LINKR_FILE:error missing', 'missing', /does not exist/],
    ['LINKR_FILE:error directory', 'directory', /directory/],
    ['LINKR_FILE:error denied', 'denied', /not readable/],
    ['LINKR_FILE:error not-absolute', 'denied', /not absolute/],
    ['LINKR_FILE:error not-regular', 'denied', /not a regular file/],
    ['LINKR_FILE:error something-else', 'incomplete', /something-else/],
  ];
  for (const [marker, status, reason] of cases) {
    const parsed = parseFileRead(`root@target:~# sh -c 'p=/f; [ -e "$p" ] || { printf ...; exit 0; }'\r\n${marker}\r\nroot@target:~# `);
    assert.equal(parsed.status, status, marker);
    assert.match(parsed.reason, reason, marker);
    assert.equal(parsed.data, null, marker);
    assert.equal(parsed.totalBytes, null, marker);
  }
  // The echo of the command carries the marker text with its quoting around it;
  // only a whole-line marker counts, so the echo must not be read as output.
  const echoed = parseFileRead("printf '\\nLINKR_FILE:error missing\\n'; exit 0\nLINKR_FILE:begin total=3 from=0\nQUJD\nLINKR_FILE:end bytes=3\n");
  assert.equal(echoed.status, 'ok', 'an echoed error marker must not override a real read');
});

test('the newest read in the journal wins over a stale earlier one', () => {
  const old = 'LINKR_FILE:begin total=3 from=0\nQUJD\nLINKR_FILE:end bytes=3\n';
  const fresh = 'LINKR_FILE:begin total=2 from=0\nQkM=\nLINKR_FILE:end bytes=2\n';
  const parsed = parseFileRead(old + 'root@target:~# dd ...\n' + fresh);
  assert.equal(parsed.status, 'ok');
  assert.equal(parsed.bytes, 2);
  assert.deepEqual(parsed.data, new Uint8Array([0x42, 0x43]));
  // A new read that died mid-payload must not fall back to the older complete one.
  const died = parseFileRead(old + 'LINKR_FILE:begin total=9 from=0\nQUJ');
  assert.equal(died.status, 'incomplete');
  assert.equal(died.data, null);
});

test('binary payloads survive the base64 round trip byte for byte', () => {
  // 0x00, 0xff, CR and LF are exactly the bytes a line-oriented transport eats.
  const body = Uint8Array.from([0x00, 0xff, 0x0d, 0x0a, 0x00, 0x1a, 0x7f, 0x80, 0xff, 0x0d, 0x0a, 0x41, 0x42]);
  const encoded = Buffer.from(body).toString('base64');
  assert.equal(encodeBase64(body), encoded, 'the module encoder must agree with the shell byte for byte');
  const parsed = parseFileRead(readFixture(body));
  assert.equal(parsed.status, 'ok');
  assert.deepEqual(parsed.data, body);
  assert.equal(parsed.bytes, body.length);
  // The same bytes arriving as one long line (no wrapping) decode identically.
  assert.deepEqual(parseFileRead(readFixture(body, { payload: encoded })).data, body);
  for (const length of [1, 2, 3, 4, 5, 62, 63, 64, 65]) {
    const sample = bytesOf(length, length);
    assert.equal(encodeBase64(sample), Buffer.from(sample).toString('base64'), `${length} bytes`);
    assert.deepEqual(parseFileRead(readFixture(sample)).data, sample, `${length} bytes`);
  }
});

test('quoteShell and formatBytes behave for the values a caller will pass', () => {
  assert.equal(quoteShell(''), "''");
  assert.equal(quoteShell('/tmp/a b'), "'/tmp/a b'");
  assert.equal(quoteShell("it's"), "'it'\\''s'");
  assert.equal(quoteShell("'; rm -rf /; '"), "''\\''; rm -rf /; '\\'''");
  assert.equal(quoteShell('$(whoami)`id`\\'), "'$(whoami)`id`\\'", 'quotes neutralise substitution and backslash');
  assert.throws(() => quoteShell(7), /needs a string/);
  assert.throws(() => quoteShell('/tmp/a\u0000b'), /NUL/);
  // A quoted value must survive a real shell as one argument.
  const probe = sh(`printf '%s' ${quoteShell("a'b c\"d$e`f\\g")}`);
  assert.equal(probe.stdout, "a'b c\"d$e`f\\g");

  assert.equal(formatBytes(0), '0 B');
  assert.equal(formatBytes(720), '720 B');
  assert.equal(formatBytes(1024), '1 KiB');
  assert.equal(formatBytes(1536), '1.5 KiB');
  assert.equal(formatBytes(8192), '8 KiB');
  assert.equal(formatBytes(128 * 1024), '128 KiB');
  assert.equal(formatBytes(1024 * 1024), '1 MiB');
  assert.throws(() => formatBytes(-1), /non-negative/);
  assert.throws(() => formatBytes(Infinity), /non-negative/);
});

test('an upload plan pages a file into chunks with resumable offsets', () => {
  const plan = uploadPlan({ path: '/tmp/fw.bin', size: 1800 });
  assert.equal(plan.path, '/tmp/fw.bin');
  assert.equal(plan.size, 1800);
  assert.equal(plan.tempPath, '/tmp/fw.bin.linkr-part', 'the part file sits next to the destination');
  assert.equal(plan.from, 0);
  assert.equal(plan.chunkBytes, DEFAULT_CHUNK_BYTES);
  assert.deepEqual(plan.chunks.map((chunk) => [chunk.index, chunk.offset, chunk.bytes]), [[0, 0, 720], [1, 720, 720], [2, 1440, 360]]);
  assert.equal(plan.prepareCommand, ": > '/tmp/fw.bin.linkr-part'");
  assert.equal(plan.chunks[0].command, null, 'without data only the geometry exists');
  assert.equal(plan.chunks[0].base64, null);

  const sized = uploadPlan({ path: '/tmp/fw.bin', size: 1800, chunkBytes: 512 });
  assert.deepEqual(sized.chunks.map((chunk) => [chunk.offset, chunk.bytes]), [[0, 512], [512, 512], [1024, 512], [1536, 264]]);
  assert.equal(sized.chunks.length, 4);

  const resumed = uploadPlan({ path: '/tmp/fw.bin', size: 1800, startAt: 720 });
  assert.deepEqual(resumed.chunks.map((chunk) => [chunk.index, chunk.offset, chunk.bytes]), [[1, 720, 720], [2, 1440, 360]]);
  assert.equal(resumed.from, 720);
  assert.equal(resumed.prepareCommand, null, 'a resume must never truncate the part file');
  assert.equal(resumed.tempPath, plan.tempPath, 'a resume reuses the same part file');
  assert.equal(resumed.completeCommand, plan.completeCommand);

  // A resume point off a chunk boundary emits a short first chunk from there.
  const mid = uploadPlan({ path: '/tmp/fw.bin', size: 1800, startAt: 700 });
  assert.deepEqual(mid.chunks.map((chunk) => [chunk.offset, chunk.bytes]), [[700, 720], [1420, 380]]);

  assert.deepEqual(uploadPlan({ path: '/tmp/empty.bin', size: 0 }).chunks, [], 'a zero-byte upload has no chunks');
  assert.equal(uploadPlan({ path: '/tmp/empty.bin', size: 0 }).prepareCommand, ": > '/tmp/empty.bin.linkr-part'");
  assert.deepEqual(uploadPlan({ path: '/tmp/fw.bin', size: 1800, startAt: 1800 }).chunks, []);
});

test('upload chunk commands truncate once, append after, and confirm what landed', () => {
  const body = bytesOf(1500, 3);
  const plan = uploadPlan({ path: '/tmp/fw.bin', size: 1500, sha256: sha256Of(body), data: body });
  assert.equal(plan.expectedSha256, sha256Of(body));
  assert.equal(plan.chunks.length, 3);
  for (const chunk of plan.chunks) {
    assert.equal(encodeBase64(body.subarray(chunk.offset, chunk.offset + chunk.bytes)), chunk.base64, `chunk ${chunk.index} payload`);
    assert.ok(chunk.command.includes(`printf '%s' ${quoteShell(chunk.base64)} | base64 -d`));
    assert.ok(chunk.command.includes('"$p"'), 'the payload is appended to the part file variable');
    assert.ok(!chunk.command.includes("'/tmp/fw.bin'"), 'no chunk may name the destination');
    assert.ok(chunk.command.includes(`index=${chunk.index} offset=${chunk.offset} bytes=${chunk.bytes} total=%s`));
    assert.ok(chunk.command.includes('m=$(wc -c < "$p"'), 'each chunk reports the measured part size');
  }
  assert.ok(plan.chunks[0].command.includes('| base64 -d > "$p"'), 'the first chunk starts a clean part file');
  assert.ok(plan.chunks[1].command.includes('| base64 -d >> "$p"'), 'later chunks append');
  assert.ok(plan.chunks[1].command.includes('total=%s'), 'the marker prints the measured size, not the planned one');
  assert.ok(plan.verifyCommand.includes("wc -c < \"$p\""));
  assert.ok(plan.verifyCommand.includes('LINKR_UPLOAD:bytes=%s'));
  assert.ok(plan.completeCommand.includes("mv -f \"$t\" \"$d\""));
  assert.ok(plan.completeCommand.includes("'1500'"), 'the move is bound to the expected size');
  assert.ok(plan.completeCommand.includes('sha256sum'), 'the move is gated on the digest too, so a corrupt part file cannot land');

  const plain = uploadPlan({ path: '/tmp/fw.bin', size: 64, data: bytesOf(64, 5) });
  assert.equal(plain.verifyCommand.includes('sha256sum'), false, 'no digest is computed when none was requested');
  assert.equal(plain.completeCommand.includes('sha256sum'), false, 'and the move stays a pure size check without one');
  const hashed = uploadPlan({ path: '/tmp/fw.bin', size: 64, sha256: 'a'.repeat(64) });
  assert.ok(hashed.verifyCommand.includes('sha256sum'));
  assert.ok(hashed.verifyCommand.includes('shasum -a 256'), 'shasum is the fallback when sha256sum is absent');
  assert.ok(hashed.verifyCommand.includes('LINKR_UPLOAD:sha256=%s'));

  const lazy = uploadChunkCommand({ tempPath: '/tmp/fw.bin.linkr-part', index: 2, offset: 1440, bytes: 60, base64: encodeBase64(bytesOf(60, 9)) });
  assert.ok(lazy.includes('>> "$p"'), 'a lazily built chunk appends by default');
  assert.ok(lazy.includes('index=2 offset=1440 bytes=60'));
});

test('upload chunk commands stay inside the serial command budget', () => {
  const path = `/tmp/${'d'.repeat(180)}/firmware.bin`;
  const body = bytesOf(MAX_CHUNK_BYTES * 2, 11);
  const plan = uploadPlan({ path, size: body.length, chunkBytes: MAX_CHUNK_BYTES, data: body });
  assert.equal(plan.chunks.length, 2);
  for (const chunk of plan.chunks) {
    assert.ok(chunk.command.length <= MAX_UPLOAD_COMMAND_BYTES, `${chunk.command.length} characters`);
    assert.ok(chunk.command.length > 2732, 'the chunk really carries a full base64 payload');
  }
  assert.ok(MAX_UPLOAD_COMMAND_BYTES < 8192, 'the budget leaves the UART ring room for echo and prompts');
  assert.throws(
    () => uploadPlan({ path: `/tmp/${'d'.repeat(2000)}/firmware.bin`, size: 2048, chunkBytes: MAX_CHUNK_BYTES, data: bytesOf(2048, 2) }),
    /UART budget/,
  );
  assert.throws(
    () => uploadPlan({ path: `/tmp/${'d'.repeat(2000)}/firmware.bin`, size: 2048, chunkBytes: MAX_CHUNK_BYTES }),
    /UART budget/,
    'an unusable destination fails at plan time, before any bytes are handed over',
  );
  assert.throws(() => uploadPlan({ path: '/tmp/fw.bin', size: 10_000_000, chunkBytes: 64 }), /chunks/);
});

test('upload inputs are validated before anything is sent', () => {
  const ok = { path: '/tmp/fw.bin', size: 10 };
  for (const path of ['fw.bin', '', 'relative/fw.bin']) {
    assert.throws(() => uploadPlan({ ...ok, path }), /must be absolute|non-empty string/);
  }
  for (const path of ['/tmp/two words.bin', "/tmp/it's.bin", '/tmp/say"hi.bin']) {
    assert.throws(() => uploadPlan({ ...ok, path }), /whitespace or quote/);
  }
  assert.throws(() => uploadPlan({ ...ok, path: '/tmp/a\tb' }), /control characters/);
  assert.throws(() => uploadPlan({ ...ok, path: '/tmp/back\\slash.bin' }), /backslash/);
  assert.throws(() => uploadPlan({ ...ok, path: '/tmp/dir/' }), /not a directory/);
  for (const size of [-1, 1.5, '10', null, NaN, Infinity, Number.MAX_SAFE_INTEGER + 2]) {
    assert.throws(() => uploadPlan({ path: '/tmp/fw.bin', size }), /non-negative integer/);
  }
  for (const chunkBytes of [0, -720, 1.5, MAX_CHUNK_BYTES + 1, null]) {
    assert.throws(() => uploadPlan({ ...ok, chunkBytes }), /chunk size/);
  }
  assert.equal(MAX_CHUNK_BYTES, 2048);
  for (const sha256 of ['abc', 'a'.repeat(63), `${'a'.repeat(64)}z`, 7]) {
    assert.throws(() => uploadPlan({ ...ok, sha256 }), /64 hexadecimal/);
  }
  assert.equal(uploadPlan({ ...ok, sha256: 'A'.repeat(64) }).expectedSha256, 'a'.repeat(64), 'digests are normalised to lower case');
  for (const startAt of [-1, 0.5, 11, '0']) {
    assert.throws(() => uploadPlan({ ...ok, startAt }), /resume offset/);
  }
  assert.throws(() => uploadPlan({ ...ok, data: bytesOf(9) }), /holds 9 bytes but 10 bytes remain/);
  assert.throws(() => uploadPlan({ ...ok, data: 'AAAA' }), /Uint8Array/);
  assert.throws(() => uploadPlan({ ...ok, startAt: 4, data: bytesOf(5) }), /holds 5 bytes but 6 bytes remain/);
  assert.throws(() => uploadChunkCommand({ tempPath: 'relative.part', index: 0, offset: 0, bytes: 3, base64: 'QUJD' }), /absolute/);
  assert.throws(() => uploadChunkCommand({ tempPath: '/tmp/f.part', index: 0, offset: 0, bytes: 3, base64: 'QUJ' }), /padded base64/);
  assert.throws(() => uploadChunkCommand({ tempPath: '/tmp/f.part', index: 0, offset: 0, bytes: 0, base64: 'QUJD' }), /positive integer/);
});

test('upload progress is read back from the per-chunk markers', () => {
  const text = 'root@t:~# printf ...\nLINKR_UPLOAD:chunk index=0 offset=0 bytes=720 total=720\n'
    + 'root@t:~# printf ...\r\nLINKR_UPLOAD:chunk index=1 offset=720 bytes=720 total=1440\r\n';
  const good = parseUploadProgress(text);
  assert.equal(good.status, 'ok');
  assert.equal(good.count, 2);
  assert.equal(good.from, 0);
  assert.equal(good.bytes, 1440);
  assert.equal(good.nextOffset, 1440, 'the resume point is the size the target measured');
  assert.deepEqual(good.markers, [{ index: 0, offset: 0, bytes: 720, total: 720 }, { index: 1, offset: 720, bytes: 720, total: 1440 }]);

  const resumed = parseUploadProgress('LINKR_UPLOAD:chunk index=2 offset=1440 bytes=360 total=1800\n');
  assert.equal(resumed.status, 'ok');
  assert.equal(resumed.from, 1440);
  assert.equal(resumed.nextOffset, 1800);

  const gap = parseUploadProgress('LINKR_UPLOAD:chunk index=0 offset=0 bytes=720 total=720\nLINKR_UPLOAD:chunk index=2 offset=1440 bytes=360 total=1800\n');
  assert.equal(gap.status, 'incomplete', 'a chunk whose marker never arrived must break the chain');
  assert.equal(gap.nextOffset, null, 'no resume point is offered from an inconsistent chain');
  assert.match(gap.reason, /verifyCommand/);

  const short = parseUploadProgress('LINKR_UPLOAD:chunk index=0 offset=0 bytes=720 total=600\n');
  assert.equal(short.status, 'incomplete');
  assert.match(short.reason, /600 bytes instead of 720/);

  assert.equal(parseUploadProgress('no markers').status, 'incomplete');
  assert.equal(parseUploadProgress('').nextOffset, null);
  // An echoed command line contains the marker text but not as a whole line.
  assert.equal(parseUploadProgress("printf '\\nLINKR_UPLOAD:chunk index=0 offset=0 bytes=720 total=%s\\n' \"$m\"\n").status, 'incomplete');
});

test('upload results are only ok when the count and digest agree', () => {
  const body = bytesOf(1800, 4);
  const plan = uploadPlan({ path: '/tmp/fw.bin', size: 1800, sha256: sha256Of(body), data: body });
  const verifyOk = `LINKR_UPLOAD:bytes=1800\nLINKR_UPLOAD:sha256=${sha256Of(body)}\n`;

  const ok = parseUploadResult(verifyOk, plan);
  assert.equal(ok.status, 'ok');
  assert.equal(ok.bytes, 1800);
  assert.equal(ok.sha256, sha256Of(body));
  assert.equal(ok.expectedSha256, sha256Of(body));
  assert.equal(ok.moved, false, 'verify alone does not move the file');
  assert.equal(ok.reason, '');
  assert.equal(parseUploadResult(verifyOk, { size: 1800, sha256: sha256Of(body) }).status, 'ok');

  const moved = parseUploadResult(`LINKR_UPLOAD:sha256=${sha256Of(body)}\nLINKR_UPLOAD:bytes=1800\nLINKR_UPLOAD:complete\n`, plan);
  assert.equal(moved.status, 'ok');
  assert.equal(moved.moved, true);
  assert.equal(parseUploadResult('LINKR_UPLOAD:complete\n').status, 'ok', 'only the complete marker can stand alone');
  // The real completeCommand always prints the count (and the digest when one
  // was expected) before moving, so a bare complete marker cannot satisfy
  // expectations the caller asked to be checked.
  assert.equal(parseUploadResult('LINKR_UPLOAD:complete\n', plan).status, 'incomplete');
  assert.equal(parseUploadResult(verifyOk).status, 'incomplete', 'a bare count without an expected size proves nothing');

  const short = parseUploadResult('LINKR_UPLOAD:error size-mismatch actual=1200\n', plan);
  assert.equal(short.status, 'mismatch');
  assert.equal(short.bytes, 1200);
  assert.match(short.reason, /1200 bytes instead of 1800/);

  const counted = parseUploadResult('LINKR_UPLOAD:bytes=1799\n', plan);
  assert.equal(counted.status, 'mismatch');
  assert.match(counted.reason, /1799 bytes instead of 1800/);

  const hashMismatch = parseUploadResult(`LINKR_UPLOAD:bytes=1800\nLINKR_UPLOAD:sha256=${'b'.repeat(64)}\n`, plan);
  assert.equal(hashMismatch.status, 'mismatch');
  assert.match(hashMismatch.reason, /not [0-9a-f]{64}/);

  const noTool = parseUploadResult('LINKR_UPLOAD:bytes=1800\nLINKR_UPLOAD:sha256=unavailable\n', plan);
  assert.equal(noTool.status, 'incomplete');
  assert.match(noTool.reason, /sha256sum nor shasum/);

  const noHash = parseUploadResult('LINKR_UPLOAD:bytes=1800\n', plan);
  assert.equal(noHash.status, 'incomplete', 'an expected digest that never arrived is not ok');
  assert.equal(parseUploadResult('LINKR_UPLOAD:missing\n', plan).status, 'incomplete');
  assert.match(parseUploadResult('LINKR_UPLOAD:missing\n', plan).reason, /no part file/);
  assert.equal(parseUploadResult('', plan).status, 'incomplete');
  assert.equal(parseUploadResult('', plan).bytes, 0);
  assert.equal(parseUploadResult(`LINKR_UPLOAD:bytes=1800\nLINKR_UPLOAD:sha256=${'c'.repeat(64)}\n`, { size: 1800, sha256: 'C'.repeat(64) }).status, 'ok', 'digests compare case-insensitively once normalised');
  assert.equal(parseUploadResult(`LINKR_UPLOAD:bytes=1800\nLINKR_UPLOAD:sha256=${'f'.repeat(64)}\n`, { size: 1800, sha256: 'C'.repeat(64) }).status, 'mismatch');
  assert.equal(parseUploadResult('LINKR_UPLOAD:error hash-mismatch actual=' + 'd'.repeat(64) + '\n', { sha256: 'e'.repeat(64) }).status, 'mismatch');
  assert.throws(() => parseUploadResult('LINKR_UPLOAD:bytes=1800\n', { size: '1800' }), /non-negative integer/, 'a malformed expectation throws instead of disabling the check');
  assert.throws(() => parseUploadResult('LINKR_UPLOAD:bytes=1800\n', { size: 1800, sha256: 'nope' }), /64 hexadecimal/);
  // Without a digest to compare, a target-reported hash failure still blocks ok.
  assert.equal(parseUploadResult(`LINKR_UPLOAD:bytes=8\nLINKR_UPLOAD:error hash-mismatch actual=${'d'.repeat(64)}\n`, { size: 8 }).status, 'mismatch');
});

test('the shell probe names the tools the commands need', () => {
  for (const tool of ['dd', 'base64', 'wc', 'tr', 'sha256sum', 'shasum']) assert.ok(TARGET_FILE_PROBE.includes(tool));
  const probed = sh(TARGET_FILE_PROBE);
  assert.equal(probed.status, 0);
  assert.match(probed.stdout, /^LINKR_TOOL:(dd|base64|wc|tr|sha256sum|shasum)$/m, 'the probe prints one LINKR_TOOL line per tool present');
});

test('the read command really pages a file through sh', () => {
  const dir = tempDir('read');
  try {
    const tricky = Uint8Array.from([0x00, 0xff, 0x0d, 0x0a, 0x00, 0x7f, 0x80, 0xfe, 0x41, 0x0a, 0x42, 0x00]);
    const path = join(dir, 'binary file.bin');
    writeFileSync(path, tricky);
    writeFileSync(join(dir, 'big.bin'), bytesOf(2000, 6));
    mkdirSync(join(dir, 'adir'));

    const page = (args) => parseFileRead(sh(readFileCommand(args)).stdout);
    assert.deepEqual(page({ path, bytes: MAX_READ_BYTES }).data, tricky, 'a full binary page round trips through a real shell');
    assert.deepEqual(page({ path, offset: 4, bytes: 5 }).data, tricky.subarray(4, 9));
    assert.equal(page({ path, offset: 4, bytes: 5 }).from, 4);
    const short = page({ path: join(dir, 'big.bin'), offset: 1900, bytes: 512 });
    assert.equal(short.status, 'ok');
    assert.equal(short.totalBytes, 2000);
    assert.equal(short.bytes, 100, 'the page stops at EOF instead of failing');
    assert.deepEqual(short.data, bytesOf(2000, 6).subarray(1900));
    const past = page({ path: join(dir, 'big.bin'), offset: 5000, bytes: 16 });
    assert.equal(past.status, 'ok');
    assert.equal(past.bytes, 0);
    assert.equal(past.data.length, 0);
    assert.equal(page({ path: join(dir, 'nope.bin'), bytes: 16 }).status, 'missing');
    assert.equal(page({ path: join(dir, 'adir'), bytes: 16 }).status, 'directory');
    const relative = page({ path: 'binary file.bin', bytes: 16 });
    assert.equal(relative.status, 'denied');
    assert.match(relative.reason, /not absolute/);
    // Reading a directory or a missing file must not spill a payload.
    assert.equal(sh(readFileCommand({ path: join(dir, 'adir'), bytes: 16 })).stdout.includes('LINKR_FILE:end'), false);
    if (typeof process.getuid === 'function' && process.getuid() !== 0) {
      const locked = join(dir, 'locked.bin');
      writeFileSync(locked, 'secret', { mode: 0o000 });
      assert.equal(page({ path: locked, bytes: 16 }).status, 'denied');
    }
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('an upload really lands through sh, and a short one leaves no destination', () => {
  const dir = tempDir('upload');
  try {
    const body = bytesOf(1800, 8);
    const destination = join(dir, 'firmware.bin');
    const digest = sha256Of(body);
    const plan = uploadPlan({ path: destination, size: body.length, sha256: digest, data: body });

    const progress = plan.chunks.map((chunk) => sh(chunk.command).stdout).join('');
    const read = parseUploadProgress(progress);
    assert.equal(read.status, 'ok');
    assert.equal(read.nextOffset, 1800);

    const verify = parseUploadResult(sh(plan.verifyCommand).stdout, plan);
    assert.equal(verify.status, 'ok', verify.reason);
    assert.equal(verify.sha256, digest, 'the host shasum/sha256sum agrees with node');
    assert.equal(existsSync(destination), false, 'nothing is visible before the move');

    const done = sh(plan.completeCommand);
    assert.equal(done.status, 0, done.stderr);
    assert.match(done.stdout, /LINKR_UPLOAD:complete/);
    assert.equal(parseUploadResult(done.stdout, plan).status, 'ok');
    assert.deepEqual(new Uint8Array(readFileSync(destination)), body);
    assert.equal(existsSync(plan.tempPath), false, 'the part file is gone after the move');

    // A failed transfer must leave the destination untouched: the size gate
    // refuses the move, and the caller sees a mismatch rather than a file.
    const broken = join(dir, 'broken.bin');
    const brokenPlan = uploadPlan({ path: broken, size: 900, chunkBytes: 720, data: bytesOf(900, 3) });
    sh(brokenPlan.prepareCommand);
    sh(brokenPlan.chunks[0].command);
    writeFileSync(brokenPlan.tempPath, bytesOf(500, 1), { flag: 'w' });
    const refused = sh(brokenPlan.completeCommand);
    assert.equal(refused.status, 65, 'a size that does not match must block the move');
    assert.equal(parseUploadResult(refused.stdout, brokenPlan).status, 'mismatch');
    assert.equal(existsSync(broken), false, 'the half-written upload never became the destination');
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('a resumed upload continues the same part file and still verifies', () => {
  const dir = tempDir('resume');
  try {
    const body = bytesOf(2600, 13);
    const destination = join(dir, 'resumed.bin');
    const digest = sha256Of(body);
    const plan = uploadPlan({ path: destination, size: body.length, sha256: digest, data: body });
    assert.equal(plan.chunks.length, 4);

    // Only the first two chunks make it before the session drops.
    for (const chunk of plan.chunks.slice(0, 2)) assert.equal(sh(chunk.command).status, 0);
    const partial = parseUploadProgress(plan.chunks.slice(0, 2).map((chunk) => sh(chunk.command).stdout).join(''));
    assert.equal(partial.nextOffset, 1440);

    const resumed = uploadPlan({ path: destination, size: body.length, sha256: digest, startAt: partial.nextOffset, data: body.subarray(partial.nextOffset) });
    assert.equal(resumed.prepareCommand, null);
    assert.deepEqual(resumed.chunks.map((chunk) => [chunk.offset, chunk.bytes]), [[1440, 720], [2160, 440]]);
    const tail = parseUploadProgress(resumed.chunks.map((chunk) => sh(chunk.command).stdout).join(''));
    assert.equal(tail.status, 'ok', tail.reason);
    assert.equal(tail.from, 1440);
    assert.equal(tail.nextOffset, 2600);

    assert.equal(parseUploadResult(sh(resumed.verifyCommand).stdout, resumed).status, 'ok');
    assert.equal(sh(resumed.completeCommand).status, 0);
    assert.deepEqual(new Uint8Array(readFileSync(destination)), body, 'the resumed upload is byte-identical');
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('a corrupted part file is caught by the digest before the move', () => {
  const dir = tempDir('corrupt');
  try {
    const body = bytesOf(900, 21);
    const destination = join(dir, 'corrupt.bin');
    const plan = uploadPlan({ path: destination, size: body.length, sha256: sha256Of(body), data: body });
    sh(plan.prepareCommand);
    for (const chunk of plan.chunks) sh(chunk.command);
    // Same size, different bytes: only the digest can tell the difference.
    const corrupt = Uint8Array.from(body, (value, index) => (index === 400 ? value ^ 0xff : value));
    writeFileSync(plan.tempPath, corrupt);
    const verdict = parseUploadResult(sh(plan.verifyCommand).stdout, plan);
    assert.equal(verdict.status, 'mismatch');
    assert.match(verdict.reason, /sha256/);
    // The move is gated on the same digest, so the corrupt part file cannot
    // become the destination even if the caller skips verifyCommand.
    const refused = sh(plan.completeCommand);
    assert.equal(refused.status, 65);
    assert.equal(parseUploadResult(refused.stdout, plan).status, 'mismatch');
    assert.equal(existsSync(destination), false);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
