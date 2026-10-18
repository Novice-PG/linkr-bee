import assert from 'node:assert/strict';
import test from 'node:test';
import { readFileSync, existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { join } from 'node:path';

const webDir = fileURLToPath(new URL('../../web', import.meta.url));
const cursorDir = join(webDir, 'vendor', 'mac-cursors');
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

test('every cursor shape ships a file per variant it declares', () => {
  assert.deepEqual(Object.keys(manifest).sort(), ['hand2', 'left_ptr', 'xterm'],
    'the manifest should list the arrow, the I-beam and the hand');
  for (const [shape, { sizes }] of Object.entries(manifest)) {
    assert.ok(sizes.length >= 1, `${shape} must list at least one file`);
    const variants = sizes.map((entry) => entry.variant ?? null);
    assert.equal(new Set(variants).size, variants.length, `${shape} lists a variant twice`);
  }
  assert.ok(entries.length >= 3, 'the manifest should list at least three files');
});

test('every vendored cursor is a PNG of the size it claims', () => {
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

/* The rule that carries the usage: everything between the brace that opens its
 * body and the brace or rule end before it. */
function ruleSelectorAt(text, usageIndex) {
  const brace = text.lastIndexOf("{", usageIndex);
  const previous = Math.max(text.lastIndexOf("}", brace - 1), text.lastIndexOf("{", brace - 1));
  return text.slice(previous + 1, brace).replace(/\s+/g, " ").trim();
}

/* The stylesheet carries the hotspots as plain numbers, so the two can drift
 * apart silently: a cursor whose hotspot is off by a few pixels still shows the
 * right picture and only makes every click land in the wrong place. */
test('the stylesheet uses each cursor with the hotspot recorded for it', () => {
  for (const entry of entries) {
    const usage = `url("./vendor/mac-cursors/${entry.file}")`;
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

/* The whole point of shipping two contrast variants: the black artwork belongs
 * to the light theme, the white one to the default (dark) theme, and the shared
 * hand to both. A black cursor on the dark theme's near-black surfaces is
 * invisible, and nothing at runtime would say so. */
test('each variant is used by the theme it was derived for', () => {
  for (const entry of entries) {
    const usage = `url("./vendor/mac-cursors/${entry.file}")`;
    for (let index = stylesheet.indexOf(usage); index !== -1; index = stylesheet.indexOf(usage, index + 1)) {
      const selector = ruleSelectorAt(stylesheet, index);
      if (entry.variant === 'black') {
        assert.match(selector, /\[data-theme="light"\]/,
          `${entry.file} is the black artwork and belongs to the light theme, found in: ${selector}`);
      } else if (entry.variant === 'white') {
        assert.doesNotMatch(selector, /\[data-theme=/,
          `${entry.file} is the white artwork and must not sit under a themed override, found in: ${selector}`);
        assert.match(selector, /:root/,
          `${entry.file} is the white artwork and should be the default pointer, found in: ${selector}`);
      } else {
        assert.match(selector, /:root/,
          `${entry.file} is shared by both themes and should hang off :root, found in: ${selector}`);
      }
    }
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

/* MIT requires the licence and copyright notice next to the images, and an
 * obligation that is only in a commit message is one nobody can check. */
test('the licence and attribution travel with the images', () => {
  assert.ok(existsSync(join(cursorDir, 'README.md')), 'the vendored cursors need their attribution README');
  assert.ok(existsSync(join(cursorDir, 'LICENSE-MIT.txt')), 'the vendored cursors need their licence text');
  const readme = readFileSync(join(cursorDir, 'README.md'), 'utf8');
  assert.match(readme, /MIT/);
  assert.match(readme, /Bogdan Koch/);
  assert.match(readme, /aw3r1se\/macOS-cursors/);
  assert.match(readme, /macos-cursors@1\.1\.0/, 'the pinned package version must be recorded');
});
