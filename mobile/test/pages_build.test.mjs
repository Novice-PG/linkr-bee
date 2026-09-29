import assert from 'node:assert/strict';
import test from 'node:test';
import { spawnSync } from 'node:child_process';
import { readdirSync, readFileSync, rmSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { join } from 'node:path';

const repoDir = fileURLToPath(new URL('../..', import.meta.url));
const script = join(repoDir, 'tools', 'build_pages.sh');
const webDir = join(repoDir, 'web');

// The interpreter is absolute on purpose: the script runs with a normal PATH,
// but every test in this suite that hands a child a narrow environment would
// otherwise resolve nothing and "pass" on a script that never ran.
function buildPages(outDir) {
  return spawnSync('/bin/sh', [script, outDir], { encoding: 'utf8' });
}

// A build that edits more than the switch is not reviewable: the point of the
// Pages variant is that it is web/ with one file changed, so the whole tree is
// compared byte for byte rather than spot-checked.
function tree(root) {
  const files = {};
  const walk = (dir, prefix = '') => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const relative = prefix ? `${prefix}/${entry.name}` : entry.name;
      if (entry.isDirectory()) walk(join(dir, entry.name), relative);
      else files[relative] = readFileSync(join(dir, entry.name));
    }
  };
  walk(root);
  return files;
}

test('the pages build copies web/ and flips only the LAN switch', () => {
  const outDir = join(repoDir, 'build-pages-test');
  try {
    const build = buildPages(outDir);
    assert.equal(build.status, 0, `tools/build_pages.sh failed: ${build.stderr}`);

    const source = tree(webDir);
    const output = tree(outDir);

    // The variant is a superset: everything in web/ is copied byte for byte.
    for (const [relative, bytes] of Object.entries(source)) {
      assert.ok(relative in output, `${relative} is missing from the pages build`);
      if (relative !== 'build_flags.js') {
        assert.ok(output[relative].equals(bytes), `${relative} must be an unmodified copy`);
      }
    }
    // The published tree gains the Jekyll escape hatch and nothing else.
    assert.deepEqual(Object.keys(output).filter((name) => !(name in source)), ['.nojekyll']);
    assert.equal(readFileSync(join(outDir, '.nojekyll')).length, 0);

    // The one differing file is the switch, off, and app.js still reads it.
    assert.match(output['build_flags.js'].toString(), /lanBridge: false/);
    assert.match(output['app.js'].toString(), /buildFlags\.lanBridge/);
  } finally {
    rmSync(outDir, { recursive: true, force: true });
  }
});

test('the committed default keeps the LAN bridge on', () => {
  // The variant is an override, so the source file has to stay the variant with
  // the fewest surprises: a local or mobile build must not lose the LAN entry.
  assert.match(readFileSync(join(webDir, 'build_flags.js'), 'utf8'), /lanBridge: true/);
  assert.match(readFileSync(join(webDir, 'app.js'), 'utf8'), /buildFlags\.lanBridge/);
});

test('the build is reproducible and does not write outside its output', () => {
  const first = join(repoDir, 'build-pages-test-a');
  const second = join(repoDir, 'build-pages-test-b');
  try {
    assert.equal(buildPages(first).status, 0);
    assert.equal(buildPages(first).status, 0, 'rebuilding over an existing output must work');
    assert.equal(buildPages(second).status, 0);
    assert.deepEqual(tree(first), tree(second), 'two builds of the same source must be identical');
  } finally {
    rmSync(first, { recursive: true, force: true });
    rmSync(second, { recursive: true, force: true });
  }
});
