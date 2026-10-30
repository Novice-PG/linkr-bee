import assert from "node:assert/strict";
import test from "node:test";
import { defaultHostLabel, labelFromUserAgent } from "../../web/host_label.js";

test("user-agent labels name the OS and browser", () => {
  assert.equal(
    labelFromUserAgent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36"),
    "Chrome · Windows",
  );
  assert.equal(
    labelFromUserAgent("Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1"),
    "Safari · iOS",
  );
  assert.equal(
    labelFromUserAgent("Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Mobile Safari/537.36"),
    "Chrome · Android",
  );
  assert.equal(
    labelFromUserAgent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36 Edg/120.0"),
    "Edge · Linux",
  );
  assert.equal(labelFromUserAgent(""), "");
  assert.equal(labelFromUserAgent("curl/8.0"), "");
});

test("native shells prefer the bluetooth device name", () => {
  assert.equal(
    defaultHostLabel({ platform: "android", deviceName: "Pixel 8", userAgent: "x" }),
    "Pixel 8",
  );
  assert.equal(
    defaultHostLabel({ platform: "web", deviceName: "Pixel 8", userAgent: "Mozilla/5.0 (Windows NT 10.0) Chrome/120" }),
    "Chrome · Windows",
  );
  assert.equal(defaultHostLabel({ platform: "ios", deviceName: "", userAgent: "" }), "");
});
