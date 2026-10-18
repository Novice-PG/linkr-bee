// Pull the three cursor shapes the terminal UI needs out of the macos-cursors
// package and write them as PNGs the browser can use, in the two contrast
// variants the themes ask for.
//
// Upstream ships .cur (Windows cursor) files: a 32-bit DIB frame plus the
// hotspot, exactly one per shape. Nothing here is a general .cur tool -- it
// extracts exactly what the stylesheet uses, so the extraction can be
// reproduced when the package is updated or a different shape is wanted.
//
// The output is committed, so this only has to run when that changes:
//
//   node tools/extract_mac_cursors.mjs
//
// Licensing: the artwork is MIT, Copyright (c) 2026 Bogdan Koch. Converting it
// to PNG and deriving the white variant is a modification, so
// web/vendor/mac-cursors/README.md credits the author and says what was
// changed. Do not remove that file.

import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { deflateSync, gunzipSync } from "node:zlib";

/* Pinned so a rerun extracts the same artwork. The registry tarball is the
 * source of truth: this machine cannot reach GitHub, and the registry is what
 * npm itself would serve. */
const PACKAGE = "macos-cursors";
const VERSION = "1.1.0";
const TARBALL = `https://registry.npmjs.org/${PACKAGE}/-/${PACKAGE}-${VERSION}.tgz`;

/* The three shapes the stylesheet uses. The arrow and the I-beam are drawn in
 * black with a white outline, which vanishes on the dark theme's near-black
 * surfaces, so a white variant (inverted RGB, alpha kept) is derived for it.
 * The hand is a white glove with a dark outline in both: that reads on either
 * background, and inverting it would hollow it out. */
const SHAPES = {
  "auto.cur": { shape: "left_ptr", purpose: "arrow", variants: ["black", "white"] },
  "text.cur": { shape: "xterm", purpose: "text I-beam", variants: ["black", "white"] },
  "pointer.cur": { shape: "hand2", purpose: "pointing hand", variants: [null] },
};

/* One size, 32px, because that is the size MDN recommends for CSS cursors:
 * Chromium and Firefox cap them at 128x128, but anything above 32x32 tends to
 * be refused or downscaled on high-DPI screens. Offering a 2x variant through
 * image-set() is not an option either -- Chromium parses the unprefixed
 * function in `cursor` but does not use it, and falls back to the keyword, which
 * silently leaves the system cursor in place. The cost is a little softness on
 * a retina screen; the alternative is not having a cursor at all. */
const SIZE = 32;

const outputDir = join(dirname(fileURLToPath(import.meta.url)), "..", "web", "vendor", "mac-cursors");

// .cur: an ICONDIR header, an ICONDIRENTRY per frame, then the DIB. The
// entries carry the hotspot; the frame carries bottom-up BGRA pixels followed
// by a 1bpp AND mask for clients without an alpha channel.
function readCursor(buffer) {
  const reserved = buffer.readUInt16LE(0);
  const type = buffer.readUInt16LE(2);
  const count = buffer.readUInt16LE(4);
  if (reserved !== 0 || type !== 2) throw new Error("not a .cur file");
  const frames = [];
  for (let index = 0; index < count; index++) {
    const entry = 6 + index * 16;
    const width = buffer[entry] || 256;
    const height = buffer[entry + 1] || 256;
    const hotspotX = buffer.readUInt16LE(entry + 4);
    const hotspotY = buffer.readUInt16LE(entry + 6);
    const size = buffer.readUInt32LE(entry + 8);
    const offset = buffer.readUInt32LE(entry + 12);
    const headerSize = buffer.readUInt32LE(offset);
    const bitCount = buffer.readUInt16LE(offset + 14);
    const compression = buffer.readUInt32LE(offset + 16);
    if (headerSize !== 40 || bitCount !== 32 || compression !== 0) {
      throw new Error(`unsupported DIB (header ${headerSize}, ${bitCount}bpp, compression ${compression})`);
    }
    frames.push({ width, height, hotspotX, hotspotY, size, pixels: offset + headerSize });
  }
  if (frames.length !== 1) throw new Error(`expected one frame, found ${frames.length}`);
  return frames[0];
}

/* The frame is bottom-up BGRA. The alpha channel is used when the file has
 * one; older writers leave it at zero and rely on the AND mask, where a set
 * bit means "transparent". */
function decodeFrame(buffer, frame) {
  const { width, height, pixels } = frame;
  const stride = width * 4;
  const maskStride = Math.ceil(width / 8 / 4) * 4;
  const maskOffset = pixels + stride * height;
  const rgba = Buffer.alloc(width * height * 4);
  let hasAlpha = false;
  for (let index = 0; index < width * height; index++) {
    if (buffer[pixels + index * 4 + 3] !== 0) hasAlpha = true;
  }
  for (let y = 0; y < height; y++) {
    const sourceRow = pixels + (height - 1 - y) * stride;
    const maskRow = maskOffset + (height - 1 - y) * maskStride;
    for (let x = 0; x < width; x++) {
      const source = sourceRow + x * 4;
      const target = (y * width + x) * 4;
      const masked = (buffer[maskRow + (x >> 3)] >> (7 - (x & 7))) & 1;
      rgba[target] = buffer[source + 2];     // red
      rgba[target + 1] = buffer[source + 1]; // green
      rgba[target + 2] = buffer[source];     // blue
      rgba[target + 3] = hasAlpha ? buffer[source + 3] : masked ? 0 : 255;
    }
  }
  return rgba;
}

/* RGB inverted, alpha untouched: the black fill becomes white, the white
 * outline becomes black, and the antialiased edges stay antialiased. */
function invert(rgba) {
  const out = Buffer.from(rgba);
  for (let index = 0; index < out.length; index += 4) {
    out[index] = 255 - out[index];
    out[index + 1] = 255 - out[index + 1];
    out[index + 2] = 255 - out[index + 2];
  }
  return out;
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
function encodePng(rgba, width, height) {
  const rows = [];
  for (let y = 0; y < height; y++) {
    rows.push(Buffer.concat([Buffer.from([0]), rgba.subarray(y * width * 4, (y + 1) * width * 4)]));
  }
  const header = Buffer.alloc(13);
  header.writeUInt32BE(width, 0);
  header.writeUInt32BE(height, 4);
  header[8] = 8;    // bit depth
  header[9] = 6;    // truecolour with alpha
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", header),
    chunk("IDAT", deflateSync(Buffer.concat(rows), { level: 9 })),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

/* Minimal ustar reader: npm tarballs are one flat archive of regular files and
 * directories, with the name either in the header or split over name/prefix. */
function untar(gzipped) {
  const buffer = gunzipSync(gzipped);
  const files = new Map();
  for (let offset = 0; offset + 512 <= buffer.length; ) {
    const header = buffer.subarray(offset, offset + 512);
    if (header.every((byte) => byte === 0)) break;
    const name = header.toString("utf8", 0, 100).replace(/\0.*$/, "");
    const prefix = header.toString("utf8", 345, 500).replace(/\0.*$/, "");
    const size = parseInt(header.toString("ascii", 124, 136).replace(/\0.*$/, "").trim(), 8) || 0;
    const type = String.fromCharCode(header[156]);
    const path = prefix ? `${prefix}/${name}` : name;
    offset += 512;
    if (type === "0" || type === "\0") files.set(path, buffer.subarray(offset, offset + size));
    offset += Math.ceil(size / 512) * 512;
  }
  return files;
}

const response = await fetch(TARBALL);
if (!response.ok) throw new Error(`${TARBALL} answered ${response.status}`);
const files = untar(Buffer.from(await response.arrayBuffer()));

mkdirSync(outputDir, { recursive: true });
const manifest = {};

for (const [file, { shape, purpose, variants }] of Object.entries(SHAPES)) {
  const source = files.get(`package/cursors/${file}`);
  if (!source) throw new Error(`${file} is not in the tarball`);
  const frame = readCursor(source);
  if (frame.width !== SIZE || frame.height !== SIZE) {
    throw new Error(`${file}: expected ${SIZE}x${SIZE}, found ${frame.width}x${frame.height}`);
  }
  const rgba = decodeFrame(source, frame);
  const entries = [];
  for (const variant of variants) {
    const name = variant ? `${shape}-${SIZE}-${variant}.png` : `${shape}-${SIZE}.png`;
    writeFileSync(join(outputDir, name), encodePng(variant === "white" ? invert(rgba) : rgba, frame.width, frame.height));
    const entry = { file: name, size: SIZE, width: frame.width, height: frame.height, hotspot: [frame.hotspotX, frame.hotspotY] };
    if (variant) entry.variant = variant;
    entries.push(entry);
    console.log(`${name.padEnd(22)} ${frame.width}x${frame.height}  hotspot (${frame.hotspotX}, ${frame.hotspotY})`);
  }
  manifest[shape] = { purpose, sizes: entries };
}

/* The hotspots live in the CSS, so they are recorded here too: a test compares
 * the two, because a hotspot typed wrong in the stylesheet points the click a
 * few pixels away from the arrow's tip and nothing else would notice. */
writeFileSync(join(outputDir, "cursors.json"), `${JSON.stringify(manifest, null, 2)}\n`);
console.log(`\nwrote ${Object.values(manifest).reduce((n, { sizes }) => n + sizes.length, 0)} PNGs and cursors.json to web/vendor/mac-cursors/`);
