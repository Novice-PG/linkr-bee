import assert from 'node:assert/strict';
import test from 'node:test';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { chmodSync, mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { MAX_LISTENER_ROWS, MAX_PROCESS_PIDS, MAX_VERIFY_COMMAND_BYTES, MAX_VERIFY_PATH, SERVICE_EXPECTATIONS,
  normalizeExpectedBytes, normalizeExpectedSha256, normalizeExpectation,
  parseServiceResult, parseVerifyResult, verifyFileCommand, verifyServiceCommand } from '../../web/target_verify.js';

/* The interpreter is named by absolute path on purpose: several tests hand the
 * script a deliberately narrow PATH to simulate a target that lacks a tool, and
 * a bare `sh` would then fail to resolve before the script ever ran -- which
 * looks exactly like "the target said nothing". */
const sh = (command, env = undefined) => spawnSync('/bin/sh', ['-c', command], { encoding: 'utf8', env });
const tempDir = (tag) => mkdtempSync(join(tmpdir(), `linkr-target-verify-${tag}-`));
const sha256Of = (text) => createHash('sha256').update(text).digest('hex');
/* The console pads `wc -c` output on some targets, so a fixture keeps the
 * padding the parser has to tolerate. */
const crlf = (text) => text.replace(/\n/g, '\r\n');

/* A fake target tool: the command's only dependency is `command -v`, so a stub
 * earlier on PATH is a faithful stand-in for a target that has (or lacks) it. */
function fakeTool(dir, name, body) {
  const path = join(dir, name);
  writeFileSync(path, `#!/bin/sh\n${body}\n`);
  chmodSync(path, 0o755);
  return path;
}
const withStubs = (dir) => ({ ...process.env, PATH: `${dir}:${process.env.PATH}` });

test('the verify command reports every refusal and quotes the path', () => {
  const command = verifyFileCommand({ path: "/tmp/it's a file.bin" });
  assert.ok(command.includes("'/tmp/it'\\''s a file.bin'"), 'an apostrophe must be closed and reopened');
  assert.ok(command.includes("printf '\\nLINKR_VERIFY:path=%s\\n' \"$p\""), 'the path is echoed first so a verdict is attributable');
  assert.ok(command.includes("case \"$p\" in /*) ;; *) printf 'LINKR_VERIFY:not-absolute\\nLINKR_VERIFY:done\\n'; exit 0 ;; esac"));
  assert.ok(command.includes("printf 'LINKR_VERIFY:missing\\nLINKR_VERIFY:done\\n'"));
  assert.ok(command.includes("printf 'LINKR_VERIFY:directory\\nLINKR_VERIFY:done\\n'"));
  assert.ok(command.includes("printf 'LINKR_VERIFY:not-regular\\nLINKR_VERIFY:done\\n'"));
  assert.ok(command.includes("printf 'LINKR_VERIFY:denied\\nLINKR_VERIFY:done\\n'"));
  assert.ok(command.includes("printf 'LINKR_VERIFY:unreadable\\nLINKR_VERIFY:done\\n'"));
  assert.ok(!command.includes('echo -e'), 'markers must use printf on busybox');
  // The digest is optional money: hashing reads the whole file on the target.
  assert.equal(verifyFileCommand({ path: '/f' }).includes('sha256sum'), false, 'no digest work unless one was asked for');
  assert.ok(verifyFileCommand({ path: '/f', hash: true }).includes('sha256sum "$p"'));
  assert.ok(verifyFileCommand({ path: '/f', hash: true }).includes("printf 'LINKR_VERIFY:sha256=%s\\n' \"$h\""));
});

test('paths, digests and byte counts are validated before a command is built', () => {
  assert.throws(() => verifyFileCommand({ path: '' }), /non-empty/);
  assert.throws(() => verifyFileCommand({ path: `/${'x'.repeat(MAX_VERIFY_PATH)}` }), /must not exceed/);
  assert.throws(() => verifyFileCommand({ path: '/a\nb' }), /control characters/);
  // A relative path is a diagnosis the shell reports, not an exception.
  assert.ok(verifyFileCommand({ path: 'relative/f' }).includes('LINKR_VERIFY:not-absolute'));

  assert.equal(normalizeExpectedSha256(''), '');
  assert.equal(normalizeExpectedSha256('AB'.repeat(32)), 'ab'.repeat(32), 'an expectation is normalised, so case cannot cause a false mismatch');
  assert.throws(() => normalizeExpectedSha256('abc'), /64 hexadecimal/);
  assert.equal(normalizeExpectedBytes(null), null);
  assert.equal(normalizeExpectedBytes(0), 0, 'a zero-byte expectation is a real check');
  assert.throws(() => normalizeExpectedBytes(-1), /non-negative integer/);
  assert.throws(() => normalizeExpectedBytes(1.5), /non-negative integer/);

  assert.equal(normalizeExpectation('unit', ''), 'active', 'the common case is the default');
  assert.equal(normalizeExpectation('process', ''), 'running');
  assert.equal(normalizeExpectation('port', ''), 'listening');
  assert.equal(normalizeExpectation('port', 'closed'), 'closed');
  assert.throws(() => normalizeExpectation('port', 'stopped'), /must be one of listening, closed/);
  assert.deepEqual(SERVICE_EXPECTATIONS.unit, ['active', 'inactive', 'failed']);
});

test('the verify command really measures a file through sh', () => {
  const dir = tempDir('file');
  try {
    const body = 'ID=linkr-bee\nVERSION=1\n';
    const path = join(dir, 'a file.bin');
    writeFileSync(path, body);
    mkdirSync(join(dir, 'adir'));

    /* Every case below runs the generated command through a real shell, so the
     * command and the parser are checked against each other rather than against
     * a hand-written fixture that only resembles the output. */
    const probe = (args, expected = {}) => parseVerifyResult(sh(verifyFileCommand(args)).stdout, expected);

    const measured = probe({ path, hash: true }, { path });
    assert.equal(measured.status, 'observed', 'no expectation means a measurement, never a verification');
    assert.equal(measured.found, 'file');
    assert.equal(measured.complete, true);
    assert.equal(measured.bytes, Buffer.byteLength(body));
    assert.equal(measured.sha256, sha256Of(body));

    const hit = probe({ path, hash: true }, { path, bytes: Buffer.byteLength(body), sha256: sha256Of(body).toUpperCase() });
    assert.equal(hit.status, 'match');
    assert.deepEqual(hit.checks, { bytes: 'match', sha256: 'match' });
    // The verdict is the application's, so the evidence that produced it ships.
    assert.ok(hit.evidence.some((line) => line.startsWith('LINKR_VERIFY:bytes=')));
    assert.ok(hit.evidence.some((line) => line.startsWith('LINKR_VERIFY:sha256=')));

    const short = probe({ path, hash: true }, { path, bytes: 4096 });
    assert.equal(short.status, 'mismatch');
    assert.equal(short.checks.bytes, 'mismatch');
    assert.match(short.reason, /holds 23 bytes, not 4096/);

    const wrongHash = probe({ path, hash: true }, { path, sha256: 'f'.repeat(64) });
    assert.equal(wrongHash.status, 'mismatch');
    assert.equal(wrongHash.checks.sha256, 'mismatch');

    const gone = probe({ path: join(dir, 'nope.bin') }, { path: join(dir, 'nope.bin'), bytes: 1 });
    assert.equal(gone.status, 'mismatch', 'an expected file that is absent refutes the expectation');
    assert.equal(gone.found, 'missing');

    const goneUnasked = probe({ path: join(dir, 'nope.bin') }, { path: join(dir, 'nope.bin') });
    assert.equal(goneUnasked.status, 'observed', 'nothing was expected, so a missing file is a measurement');
    assert.equal(goneUnasked.found, 'missing');

    const isDir = probe({ path: join(dir, 'adir') }, { path: join(dir, 'adir'), bytes: 1 });
    assert.equal(isDir.status, 'mismatch');
    assert.equal(isDir.found, 'directory');

    /* A path with an apostrophe and a space survives both the shell quoting and
     * the marker comparison, so a tricky real path cannot be reported as
     * "belongs to another path". */
    const tricky = join(dir, "it's a file.bin");
    writeFileSync(tricky, 'xyz');
    const quoted = probe({ path: tricky, hash: true }, { path: tricky, bytes: 3 });
    assert.equal(quoted.status, 'match');
    assert.equal(quoted.path, tricky);

    const relative = probe({ path: 'relative/f' }, { path: 'relative/f', bytes: 1 });
    assert.equal(relative.status, 'indeterminate', 'a relative path means the check never ran');
    assert.match(relative.reason, /not absolute/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('a digest request on a target without a hash tool is indeterminate, not a failure', () => {
  const dir = tempDir('nohash');
  try {
    const path = join(dir, 'f');
    writeFileSync(path, 'abc');
    /* A PATH holding the real `wc` and nothing else is a target with neither
     * sha256sum nor shasum. The size check still succeeds, so the verdict has to
     * come from the digest being unanswerable rather than from the whole command
     * failing -- and the size result must survive that. */
    symlinkSync('/usr/bin/wc', join(dir, 'wc'));
    const output = sh(verifyFileCommand({ path, hash: true }), { PATH: dir }).stdout;
    const parsed = parseVerifyResult(output, { path, bytes: 3, sha256: 'a'.repeat(64) });
    assert.equal(parsed.status, 'indeterminate');
    assert.equal(parsed.sha256Unavailable, true);
    assert.equal(parsed.checks.bytes, 'match', 'the check that could run still reports its result');
    assert.equal(parsed.bytes, 3);
    assert.match(parsed.reason, /neither sha256sum nor shasum/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('a digest the target cannot compute is reported as unavailable', () => {
  // A target with sha256sum absent and shasum present but silent: the marker
  // says so instead of the check silently passing.
  const text = crlf('LINKR_VERIFY:path=/f\nLINKR_VERIFY:file\nLINKR_VERIFY:bytes=3\nLINKR_VERIFY:sha256=unavailable\nLINKR_VERIFY:done\n');
  const parsed = parseVerifyResult(text, { path: '/f', bytes: 3, sha256: 'a'.repeat(64) });
  assert.equal(parsed.status, 'indeterminate');
  assert.equal(parsed.sha256Unavailable, true);
  assert.equal(parsed.checks.bytes, 'match', 'a check that succeeded still reports its own result');
  assert.match(parsed.reason, /neither sha256sum nor shasum/);
});

test('an unfinished or foreign check is never read as a finding', () => {
  const partial = crlf('LINKR_VERIFY:path=/f\nLINKR_VERIFY:file\nLINKR_VERIFY:bytes=3\n');
  const unfinished = parseVerifyResult(partial, { path: '/f', bytes: 3 });
  assert.equal(unfinished.status, 'indeterminate', 'no completion marker means the command did not finish');
  assert.equal(unfinished.complete, false);
  assert.match(unfinished.reason, /did not finish/);

  /* Console evidence is a window into a shared journal, so an earlier verify of
   * another path can still be in it. It must not answer for this request. */
  const foreign = parseVerifyResult(crlf('LINKR_VERIFY:path=/old\nLINKR_VERIFY:file\nLINKR_VERIFY:bytes=3\nLINKR_VERIFY:done\n'), { path: '/new', bytes: 3 });
  assert.equal(foreign.status, 'indeterminate');
  assert.match(foreign.reason, /belongs to \/old, not \/new/);

  const silent = parseVerifyResult('root@target:~# ', { path: '/f', bytes: 3 });
  assert.equal(silent.status, 'indeterminate');
  assert.match(silent.reason, /never ran or its output was lost/);

  // The echoed command line contains every marker; only whole lines count.
  const echoed = crlf("root@target:~# p='/f'; printf '\\nLINKR_VERIFY:path=%s\\n' \"$p\"; printf 'LINKR_VERIFY:missing\\nLINKR_VERIFY:done\\n'\n");
  const fromEcho = parseVerifyResult(echoed, { path: '/f', bytes: 3 });
  assert.equal(fromEcho.status, 'indeterminate', 'the echo of a command is not its output');
  assert.equal(fromEcho.found, null);
});

test('the service command asks for exactly one claim', () => {
  assert.throws(() => verifyServiceCommand({}), /exactly one of unit, process or port/);
  assert.throws(() => verifyServiceCommand({ unit: 'a.service', port: 80 }), /exactly one/);
  assert.throws(() => verifyServiceCommand({ unit: '-x.service' }), /must start with a letter or digit/);
  assert.throws(() => verifyServiceCommand({ unit: 'a.service', expect: 'running' }), /must be one of active, inactive, failed/);
  assert.throws(() => verifyServiceCommand({ port: 0 }), /between 1 and 65535/);
  assert.throws(() => verifyServiceCommand({ port: 65536 }), /between 1 and 65535/);
  assert.throws(() => verifyServiceCommand({ process: `x`.repeat(201) }), /must not exceed/);
  assert.throws(() => verifyServiceCommand({ process: 'a\nb' }), /control characters/);

  const unit = verifyServiceCommand({ unit: 'linkr.service' });
  assert.ok(unit.includes("printf '\\nLINKR_VERIFY:subject=unit:%s\\n' \"$s\""));
  assert.ok(unit.includes('systemctl show -p LoadState -p ActiveState -p SubState -p MainPID -p ExecMainStatus -- "$s"'));
  assert.ok(unit.includes('LINKR_VERIFY:unsupported=no-systemctl'));
  assert.ok(verifyServiceCommand({ process: 'linkrd' }).includes('pgrep -f "$s"'));
  assert.ok(verifyServiceCommand({ port: 8765 }).includes('$t -ltn'));
  assert.ok(verifyServiceCommand({ port: 8765 }).includes('LINKR_VERIFY:unsupported=no-listener-tool'));
});

test('the service command really reads a unit through sh', () => {
  const dir = tempDir('unit');
  try {
    const scenarios = {
      running: 'printf "LoadState=loaded\\nActiveState=active\\nSubState=running\\nMainPID=4242\\nExecMainStatus=0\\n"',
      dead: 'printf "LoadState=loaded\\nActiveState=inactive\\nSubState=dead\\nMainPID=0\\nExecMainStatus=0\\n"',
      failed: 'printf "LoadState=loaded\\nActiveState=failed\\nSubState=failed\\nMainPID=0\\nExecMainStatus=1\\n"',
      absent: 'printf "LoadState=not-found\\nActiveState=inactive\\nSubState=dead\\nMainPID=0\\nExecMainStatus=0\\n"',
    };
    const run = (scenario, expected) => {
      fakeTool(dir, 'systemctl', scenarios[scenario]);
      return parseServiceResult(sh(verifyServiceCommand({ unit: 'linkr.service', expect: '' }), withStubs(dir)).stdout, expected);
    };

    const up = run('running', { unit: 'linkr.service' });
    assert.equal(up.status, 'match', 'active is what a caller asking about a service usually means');
    assert.equal(up.observed.active, 'active');
    assert.equal(up.observed.mainPid, 4242);
    assert.ok(up.evidence.includes('ActiveState=active'), 'the setting that decided the verdict travels with it');

    const down = run('dead', { unit: 'linkr.service' });
    assert.equal(down.status, 'mismatch');
    assert.match(down.reason, /ActiveState=inactive/);
    assert.equal(run('dead', { unit: 'linkr.service', expect: 'inactive' }).status, 'match');
    assert.equal(run('failed', { unit: 'linkr.service', expect: 'failed' }).status, 'match');
    assert.equal(run('failed', { unit: 'linkr.service' }).status, 'mismatch', 'failed is not what "active" asked for');

    /* A unit that does not exist must not read as a stopped service: that is
     * how a typo becomes a confident all-clear. */
    const typo = run('absent', { unit: 'linkr.service', expect: 'inactive' });
    assert.equal(typo.status, 'indeterminate');
    assert.match(typo.reason, /not a unit on this target/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('a target without systemctl, pgrep or ss says so instead of failing', () => {
  const dir = tempDir('bare');
  try {
    const bare = { ...process.env, PATH: dir };
    for (const [args, reason] of [[{ unit: 'a.service' }, 'no systemctl'], [{ process: 'x' }, 'no pgrep'], [{ port: 80 }, 'neither ss nor netstat']]) {
      const parsed = parseServiceResult(sh(verifyServiceCommand(args), bare).stdout, args);
      assert.equal(parsed.status, 'indeterminate', `an unanswerable claim is not a failed one: ${JSON.stringify(args)}`);
      assert.match(parsed.reason, new RegExp(reason));
      assert.ok(parsed.unsupported.startsWith('no-'));
    }
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('the service command really reads processes and listening ports through sh', () => {
  const dir = tempDir('runtime');
  try {
    const scenarios = {
      two: 'printf "1001\\n1002\\n"',
      none: 'exit 1',
    };
    const runProcess = (scenario, expected) => {
      fakeTool(dir, 'pgrep', scenarios[scenario]);
      return parseServiceResult(sh(verifyServiceCommand({ process: 'linkrd' }), withStubs(dir)).stdout, expected);
    };
    const present = runProcess('two', { process: 'linkrd' });
    assert.equal(present.status, 'match');
    assert.deepEqual(present.observed.pids, ['1001', '1002']);
    assert.equal(runProcess('none', { process: 'linkrd' }).status, 'mismatch');
    assert.equal(runProcess('none', { process: 'linkrd', expect: 'absent' }).status, 'match', 'absent is a claim worth being able to make');

    /* The listening-socket table is dumped whole and filtered here, because ss
     * and netstat disagree about filter syntax. A stub pins the table so the
     * verdict is checked against rows a real console would produce. */
    fakeTool(dir, 'ss', 'printf "State Recv-Q Send-Q Local Address:Port Peer Address:Port\\nLISTEN 0 4096 127.0.0.1:8765 0.0.0.0:*\\nLISTEN 0 128 0.0.0.0:22 0.0.0.0:*\\n"');
    const runPort = (port, expected) => parseServiceResult(sh(verifyServiceCommand({ port }), withStubs(dir)).stdout, expected);
    const open = runPort(8765, { port: 8765 });
    assert.equal(open.status, 'match');
    assert.equal(open.observed.tool, 'ss');
    assert.equal(open.observed.rows.length, 1, 'only the rows that matched the port are reported');
    assert.match(open.evidence.join('\n'), /127\.0\.0\.1:8765/);
    assert.equal(runPort(9999, { port: 9999 }).status, 'mismatch');
    assert.equal(runPort(9999, { port: 9999, expect: 'closed' }).status, 'match');
    // A peer column is never a bare port, so a match cannot come from the far side.
    assert.equal(runPort(22, { port: 22 }).status, 'match');
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('a service answer that belongs to another claim is not accepted', () => {
  const text = crlf('LINKR_VERIFY:subject=unit:other.service\nLINKR_VERIFY:unit-begin\nActiveState=active\nLINKR_VERIFY:unit-end\nLINKR_VERIFY:done\n');
  const parsed = parseServiceResult(text, { unit: 'linkr.service' });
  assert.equal(parsed.status, 'indeterminate');
  assert.match(parsed.reason, /belongs to unit other\.service/);

  /* A block that opened and never closed while the command did reach its end:
   * one line was lost in transit, which is the case the `closed` flag exists
   * for. It must not read as "nothing listens". */
  const truncated = crlf('LINKR_VERIFY:subject=port:80\nLINKR_VERIFY:listener-tool=ss\nLINKR_VERIFY:listeners-begin\nLISTEN 0 4096 0.0.0.0:80 0.0.0.0:*\nLINKR_VERIFY:done\n');
  const cut = parseServiceResult(truncated, { port: 80 });
  assert.equal(cut.status, 'indeterminate');
  assert.match(cut.reason, /arrived incomplete/);

  // A table that never finished at all is a different failure and says so.
  const silentTable = crlf('LINKR_VERIFY:subject=port:80\nLINKR_VERIFY:listener-tool=ss\nLINKR_VERIFY:listeners-begin\nLISTEN 0 4096 0.0.0.0:80 0.0.0.0:*\n');
  assert.match(parseServiceResult(silentTable, { port: 80 }).reason, /did not finish/);

  const noSubject = parseServiceResult(crlf('LINKR_VERIFY:done\n'), { port: 80 });
  assert.equal(noSubject.status, 'indeterminate');
  assert.match(noSubject.reason, /does not name the claim/);
});

test('a socket table cut at the bound cannot be read as nothing listening', () => {
  const row = (port) => `LISTEN 0 4096 0.0.0.0:${port} 0.0.0.0:*`;
  const table = (ports) => crlf(`LINKR_VERIFY:subject=port:8765\nLINKR_VERIFY:listener-tool=ss\n` +
    `LINKR_VERIFY:listeners-begin\n${ports.map(row).join("\n")}\nLINKR_VERIFY:listeners-end\nLINKR_VERIFY:done\n`);

  // A table that ended on its own and does not mention the port is a real negative.
  const negative = parseServiceResult(table([1000, 1001, 1002]), { port: 8765 });
  assert.equal(negative.status, 'mismatch');
  assert.equal(negative.observed.listening, false);
  assert.equal(negative.observed.truncated, false);

  /* A table that stopped at the bound proves nothing by absence: the row may
   * simply never have been printed, and "nothing listens" would then be a
   * confident wrong answer -- the one outcome this module exists to prevent. */
  const cut = parseServiceResult(table(Array.from({ length: MAX_LISTENER_ROWS }, (_, i) => 1000 + i)), { port: 8765 });
  assert.equal(cut.status, 'indeterminate');
  assert.match(cut.reason, /cannot show that nothing listens/);
  assert.equal(cut.observed.truncated, true);

  // Presence is still proven by a truncated table: only the negative is refused.
  const present = parseServiceResult(table(Array.from({ length: MAX_LISTENER_ROWS }, (_, i) => (i === 3 ? 8765 : 1000 + i))), { port: 8765 });
  assert.equal(present.status, 'match');

  /* The pid list is the mirror image and needs no such guard: its bound is only
   * reached when entries were found, which is exactly what "running" claims. */
  const manyPids = crlf(`LINKR_VERIFY:subject=process:linkrd\nLINKR_VERIFY:process-begin\n` +
    `${Array.from({ length: MAX_PROCESS_PIDS }, (_, i) => 2000 + i).join("\n")}\nLINKR_VERIFY:process-end\nLINKR_VERIFY:done\n`);
  const running = parseServiceResult(manyPids, { process: 'linkrd' });
  assert.equal(running.status, 'match');
  assert.equal(running.observed.truncated, true);
});

test('the listener bound stays inside the bridge ring', () => {
  /* The response has to fit the 8192-byte RX ring together with the echo of the
   * command and the next prompt, or a row gets cut mid-line. ~65 characters per
   * row is what ss -ltn prints, so the bound is a ring budget, not a taste. */
  assert.ok(MAX_LISTENER_ROWS * 65 < 4096, `${MAX_LISTENER_ROWS} rows must leave half the ring free`);
});

test('a verify command stays inside the UART budget', () => {
  const longest = verifyFileCommand({ path: `/${'p'.repeat(MAX_VERIFY_PATH - 1)}`, hash: true });
  assert.ok(longest.length < MAX_VERIFY_COMMAND_BYTES, `a maximum-length path must still fit: ${longest.length}`);
  assert.throws(() => verifyServiceCommand({ unit: `a${'b'.repeat(1000)}` }), /unit name/);
});
