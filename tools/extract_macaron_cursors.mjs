// Pull the three cursor shapes the light theme needs out of the deepin macaron
// cursor theme and write them as PNGs the browser can use.
//
// The upstream files are XCursor (X11) bundles: one file per shape holding every
// size as raw ARGB pixels, with no PNG or vector source anywhere in the theme.
// Nothing here is a general XCursor tool -- it extracts exactly what the light
// theme uses, so the extraction can be reproduced when the theme is updated or a
// different size is wanted.
//
// The output is committed, so this only has to run when that changes:
//
//   node tools/extract_macaron_cursors.mjs
//
// Licensing: the macaron artwork is CC-BY-4.0, Copyright UnionTech Software
// Technology Co., Ltd. Converting it to PNG and shipping it here is a
// modification, so web/vendor/macaron-cursors/README.md credits the author and
// says what was changed. Do not remove that file.

import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { deflateSync } from "node:zlib";

/* Pinned so a rerun extracts the same artwork. */
const REVISION = "a0c79e31467ab602d96605475dca69eb835c5d90";
const BASE = `https://raw.githubusercontent.com/linuxdeepin/deepin-desktop-theme/${REVISION}/macaron/icons/macaron/cursors/cursors`;

/* The shape names the theme uses, and what each one is for here. */
const SHAPES = { left_ptr: "arrow", xterm: "text I-beam", hand2: "pointing hand" };

/* One size, 32px, because that is the size MDN recommends for CSS cursors:
 * Chromium and Firefox cap them at 128x128, but anything above 32x32 tends to
 * be refused or downscaled on high-DPI screens. Offering a 2x variant through
 * image-set() is not an option either -- Chromium parses the unprefixed
 * function in `cursor` but does not use it, and falls back to the keyword, which
 * silently leaves the system cursor in place. The cost is a little softness on
 * a retina screen; the alternative is not having a cursor at all. */
const SIZE = 32;

const outputDir = join(dirname(fileURLToPath(import.meta.url)), "..", "web", "vendor", "macaron-cursors");

// XCursor: a header, a table of contents, then chunks. Only image chunks matter
// here (type 0xfffd0002); each one carries its size, hotspot and ARGB pixels.
function readCursor(buffer) {
  if (buffer.toString("ascii", 0, 4) !== "Xcur") throw new Error("not an XCursor file");
  const headerSize = buffer.readUInt32LE(4);
  const count = buffer.readUInt32LE(12);
  const images = new Map();
  for (let index = 0; index < count; index++) {
    const entry = headerSize + index * 12;
    if (buffer.readUInt32LE(entry) !== 0xfffd0002) continue;
    const size = buffer.readUInt32LE(entry + 4);
    const position = buffer.readUInt32LE(entry + 8);
    const chunkHeader = buffer.readUInt32LE(position);
    images.set(size, {
      size,
      width: buffer.readUInt32LE(position + 16),
      height: buffer.readUInt32LE(position + 20),
      hotspotX: buffer.readUInt32LE(position + 24),
      hotspotY: buffer.readUInt32LE(position + 28),
      pixels: position + chunkHeader,
    });
  }
  return images;
}

const CRC_TABLE = Array.from({ length: 256 }, (_, n) => {
  let value = n;
  for (let bit = 0; bit < 8; bit++) value = value & 1 ? 0xedb88320 ^ (value >>> 1) : value >>> 1;
  return value >>> 0;
});
const crc32 = (buffer) => {
  let value = 0xffffffff;
  for (const byte of buffer) value = CRC_TABLE[(value ^ byte) & 0xff] ^ (value >>> 8);
  return (value ^ 0xffffffff) >>> 0;
};
const chunk = (type, data) => {
  const length = Buffer.alloc(4);
  length.writeUInt32BE(data.length);
  const body = Buffer.concat([Buffer.from(type, "ascii"), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(body));
  return Buffer.concat([length, body, crc]);
};

/* The alpha channel is kept: the browser composites the cursor, so baking a
 * background in would put a grey box behind it on every other surface. */
function encodePng(image, source) {
  const rows = [];
  for (let y = 0; y < image.height; y++) {
    const row = Buffer.alloc(1 + image.width * 4);
    for (let x = 0; x < image.width; x++) {
      const value = source.readUInt32LE(image.pixels + (y * image.width + x) * 4);
      row[1 + x * 4] = (value >>> 16) & 0xff;   // red
      row[2 + x * 4] = (value >>> 8) & 0xff;    // green
      row[3 + x * 4] = value & 0xff;            // blue
      row[4 + x * 4] = (value >>> 24) & 0xff;   // alpha
    }
    rows.push(row);
  }
  const header = Buffer.alloc(13);
  header.writeUInt32BE(image.width, 0);
  header.writeUInt32BE(image.height, 4);
  header[8] = 8;    // bit depth
  header[9] = 6;    // truecolour with alpha
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", header),
    chunk("IDAT", deflateSync(Buffer.concat(rows), { level: 9 })),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

/* The theme aliases some shapes as git symlinks -- xterm -> text, hand2 ->
 * pointing_hand -- and the raw endpoint serves the link target as its text
 * rather than following it, so the chain is walked here. */
async function fetchShape(name) {
  for (let hop = 0; hop < 4; hop++) {
    const response = await fetch(`${BASE}/${name}`);
    if (!response.ok) throw new Error(`${name}: upstream answered ${response.status}`);
    const buffer = Buffer.from(await response.arrayBuffer());
    if (buffer.toString("ascii", 0, 4) === "Xcur") return { buffer, resolved: name };
    const target = buffer.toString("utf8").trim();
    if (!/^[A-Za-z0-9_.-]{1,64}$/.test(target)) {
      throw new Error(`${name}: neither an XCursor file nor a symlink target`);
    }
    name = target;
  }
  throw new Error("cursor symlink chain is longer than expected");
}

mkdirSync(outputDir, { recursive: true });
const manifest = {};

for (const [shape, purpose] of Object.entries(SHAPES)) {
  const { buffer: source, resolved } = await fetchShape(shape);
  if (resolved !== shape) console.log(`${shape} is a symlink to ${resolved}`);
  const images = readCursor(source);
  const entries = [];

  const image = images.get(SIZE);
  if (!image) throw new Error(`${shape}: no ${SIZE}px entry (has ${[...images.keys()].sort((a, b) => a - b).join(", ")})`);
  const file = `${shape}-${SIZE}.png`;
  writeFileSync(join(outputDir, file), encodePng(image, source));
  entries.push({ file, size: SIZE, width: image.width, height: image.height, hotspot: [image.hotspotX, image.hotspotY] });
  console.log(`${file.padEnd(18)} ${image.width}x${image.height}  hotspot (${image.hotspotX}, ${image.hotspotY})`);
  manifest[shape] = { purpose, sizes: entries };
}

/* The hotspots live in the CSS, so they are recorded here too: a test compares
 * the two, because a hotspot typed wrong in the stylesheet points the click a
 * few pixels away from the arrow's tip and nothing else would notice. */
writeFileSync(join(outputDir, "cursors.json"), `${JSON.stringify(manifest, null, 2)}\n`);
console.log(`\nwrote ${Object.keys(SHAPES).length} PNGs and cursors.json to web/vendor/macaron-cursors/`);
