import { test } from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { fileURLToPath, pathToFileURL } from "node:url";

/* The static web terminal ships a prebuilt copy of the assistant runtime, so
 * the artifact must exist, match its build manifest, and stay in step with the
 * installed SDK versions. Rebuild with tools/build_agent_bundle.sh. */
const ROOT = fileURLToPath(new URL("../..", import.meta.url));
const bundlePath = `${ROOT}web/vendor/agent/agent-runtime.js`;
const build = JSON.parse(readFileSync(`${ROOT}web/vendor/agent/BUILD.json`, "utf8"));

test("the vendored bundle matches its build manifest", () => {
  const digest = createHash("sha256").update(readFileSync(bundlePath)).digest("hex");
  assert.equal(digest, build.sha256, "agent-runtime.js does not match BUILD.json; rebuild the bundle");
  assert.equal(build.entry, "mobile/src/agent-runtime.mjs");
});

test("the bundle is browser-loadable", () => {
  const source = readFileSync(bundlePath, "utf8");
  assert.ok(source.length > 1000, "bundle is suspiciously small");
  assert.doesNotMatch(source, /from\s*["']node:/, "bundle must not require Node builtins");
  assert.doesNotMatch(source, /require\(/, "bundle must not contain CommonJS requires");
  assert.doesNotMatch(source, /["']@earendil-works\//, "bundle must not leave bare import specifiers");
});

test("the bundle was built from the current source files", () => {
  const sources = build.sources || {};
  assert.ok(Object.keys(sources).length > 0, "BUILD.json must record the bundled project sources");
  for (const [file, digest] of Object.entries(sources)) {
    const actual = createHash("sha256").update(readFileSync(`${ROOT}${file}`)).digest("hex");
    assert.equal(actual, digest, `${file} changed after the bundle was built; rerun tools/build_agent_bundle.sh`);
  }
});

test("every bundled package is still at the version it was built from", () => {
  for (const [name, version] of Object.entries(build.packages)) {
    const manifest = JSON.parse(readFileSync(`${ROOT}mobile/node_modules/${name}/package.json`, "utf8"));
    assert.equal(manifest.version, version,
      `${name} is ${manifest.version} but the bundle was built from ${version}; rerun tools/build_agent_bundle.sh`);
  }
});

test("the bundle exposes what the static loader imports", async () => {
  const module = await import(pathToFileURL(bundlePath).href);
  assert.equal(module.available, true);
  assert.equal(typeof module.createSerialAgent, "function");
});

test("the static web loader offers the assistant", async () => {
  const loader = await import(pathToFileURL(`${ROOT}web/agent_runtime.js`).href);
  assert.equal(loader.available, true, "the static web build must expose the assistant");
  assert.equal(typeof loader.createSerialAgent, "function");
});
