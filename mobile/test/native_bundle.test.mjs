import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import { copyNativeVendor, stripWebFlasher } from "../native-bundle.mjs";

test("native bundles drop the flasher entry but keep the terminal", () => {
  const html = `<header>
  <a href="./index.html">terminal</a>
  <!-- web-flasher:start -->
  <a href="./flash.html">flash</a>
  <!-- web-flasher:end -->
</header>`;
  const stripped = stripWebFlasher(html);
  assert.doesNotMatch(stripped, /flash\.html|web-flasher/);
  assert.match(stripped, /terminal/);
});

test("native bundles copy terminal vendors but not Web Serial libraries", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "linkr-vendor-"));
  try {
    const source = path.join(root, "vendor");
    for (const name of ["xterm", "esptool-js", "jszip"]) {
      fs.mkdirSync(path.join(source, name), { recursive: true });
      fs.writeFileSync(path.join(source, name, "asset.js"), name);
    }
    const destination = path.join(root, "dist", "vendor");
    copyNativeVendor(source, destination);
    assert.equal(fs.existsSync(path.join(destination, "xterm/asset.js")), true);
    assert.equal(fs.existsSync(path.join(destination, "esptool-js")), false);
    assert.equal(fs.existsSync(path.join(destination, "jszip")), false);
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});
