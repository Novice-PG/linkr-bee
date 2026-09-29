import assert from 'node:assert/strict';
import test from 'node:test';
import { readFileSync, existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { join } from 'node:path';

const webDir = fileURLToPath(new URL('../../web', import.meta.url));
const cursorDir = join(webDir, 'vendor', 'macaron-cursors');
const manifest = JSON.parse(readFileSync(join(cursorDir, 'cursors.json'), 'utf8'));
const stylesheet = readFileSync(join(webDir, 'style.css'), 'utf8');

/* Width and height straight out of the PNG header: enough to catch a truncated
 * or wrong-size file without pulling in an image library. */
function pngSize(file) {
  const bytes = readFileSync(join(cursorDir, file));
  assert.deepEqual([...bytes.subarray(0, 8)], [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a], `${file} is not a PNG`);
  return { width: bytes.readUInt32BE(16), height: bytes.readUInt32BE(20) };
}

const entries = Object.entries(manifest).flatMap(([shape, { purpose, sizes }]) =>
  sizes.map((entry) => ({ shape, purpose, ...entry })));

test('every vendored cursor is a PNG of the size it claims', () => {
  assert.equal(entries.length, Object.keys(manifest).length, 'each shape should contribute one file');
  assert.ok(entries.length >= 3, 'the manifest should list the arrow, the I-beam and the hand');
  for (const entry of entries) {
    const actual = pngSize(entry.file);
    assert.deepEqual(actual, { width: entry.width, height: entry.height }, `${entry.file} dimensions`);
    assert.equal(entry.size, entry.width, `${entry.file} should be named for its pixel size`);
  }
});

test('every hotspot lands inside its image', () => {
  for (const entry of entries) {
    const [x, y] = entry.hotspot;
    assert.ok(Number.isInteger(x) && Number.isInteger(y), `${entry.file} hotspot must be integers`);
    assert.ok(x >= 0 && x < entry.width && y >= 0 && y < entry.height,
      `${entry.file} hotspot (${x}, ${y}) is outside ${entry.width}x${entry.height}`);
  }
});

/* The hotspot is whatever follows the last closing paren of the declaration --
 * url(...) -- and is given in image pixels, so it is the number recorded beside
 * the file. */
function hotspotInDeclaration(text, usageIndex) {
  const end = text.indexOf(";", usageIndex);
  const declaration = text.slice(usageIndex, end === -1 ? text.length : end);
  const match = declaration.slice(declaration.lastIndexOf(")") + 1).match(/(\d+)\s+(\d+)/);
  return match && `${match[1]} ${match[2]}`;
}

/* The stylesheet carries the hotspots as plain numbers, so the two can drift
 * apart silently: a cursor whose hotspot is off by a few pixels still shows the
 * right picture and only makes every click land in the wrong place. */
test('the stylesheet uses each cursor with the hotspot recorded for it', () => {
  for (const entry of entries) {
    const usage = `url("./vendor/macaron-cursors/${entry.file}")`;
    const hotspots = [];
    for (let index = stylesheet.indexOf(usage); index !== -1; index = stylesheet.indexOf(usage, index + 1)) {
      hotspots.push(hotspotInDeclaration(stylesheet, index));
    }
    assert.ok(hotspots.length, `web/style.css does not use ${entry.file}`);
    const [x, y] = entry.hotspot;
    assert.ok(hotspots.every((value) => value === `${x} ${y}`),
      `${entry.file} is used with hotspot(s) ${hotspots.join(" / ")} in web/style.css, but its file records ${x} ${y}`);
  }
});

/* Chromium parses the unprefixed image-set() inside `cursor` and then does not
 * use it for the pointer: the declaration stands, the image is never drawn, and
 * the keyword at the end of it takes over -- the system cursor, with nothing
 * anywhere saying why. This shipped once; the assertion is here so it cannot
 * ship again. */
test('the cursor rules do not lean on image-set', () => {
  assert.doesNotMatch(stylesheet, /cursor:[^;]*image-set/s,
    'image-set() in cursor leaves the system pointer in place in Chromium; ship one size instead');
});

/* MDN recommends 32x32: Chromium and Firefox cap cursors at 128x128, and images
 * above 32x32 tend to be refused or downscaled on high-DPI screens. */
test('the cursors are the size the platform recommends', () => {
  for (const entry of entries) {
    assert.equal(entry.width, entry.height, `${entry.file} should be square`);
    assert.ok(entry.width <= 32, `${entry.file} is ${entry.width}px, above the 32px the platform recommends`);
  }
});

/* CC-BY-4.0 requires attribution next to the images, and an obligation that is
 * only in a commit message is one nobody can check. */
test('the licence and attribution travel with the images', () => {
  assert.ok(existsSync(join(cursorDir, 'README.md')), 'the vendored cursors need their attribution README');
  assert.ok(existsSync(join(cursorDir, 'LICENSE-CC-BY-4.0.txt')), 'the vendored cursors need their licence text');
  const readme = readFileSync(join(cursorDir, 'README.md'), 'utf8');
  assert.match(readme, /CC-BY-4\.0/);
  assert.match(readme, /UnionTech Software Technology Co\., Ltd\./);
  assert.match(readme, /linuxdeepin\/deepin-desktop-theme/);
  assert.match(readme, /a0c79e31467ab602d96605475dca69eb835c5d90/, 'the source revision must be recorded');
});
