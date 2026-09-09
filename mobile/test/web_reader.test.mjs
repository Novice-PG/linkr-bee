import assert from "node:assert/strict";
import test from "node:test";
import { readWebPage } from "../src/web-reader.mjs";

test("web reader omits credentials, bounds text, and marks evidence untrusted", async () => {
  const page = await readWebPage({ url: "https://docs.example.org/board",
    fetchImpl: async (url, options) => {
      assert.equal(options.credentials, "omit");
      assert.equal(options.referrerPolicy, "no-referrer");
      assert.equal(options.headers.Authorization, undefined);
      return new Response("a".repeat(17000), { headers: { "Content-Type": "text/plain" } });
    } });
  assert.equal(page.text.length, 16000);
  assert.equal(page.truncated, true);
  assert.equal(page.untrusted, true);
});

test("web reader rejects credentials and non-HTTP URLs before fetching", async () => {
  for (const url of ["file:///etc/passwd", "javascript:alert(1)", "https://secret@example.org"]) {
    await assert.rejects(readWebPage({ url, fetchImpl: () => assert.fail("must not fetch") }), /HTTP/);
  }
});

test("web reader reports CORS failures and rejects binaries and oversized pages", async () => {
  const run = fetchImpl => readWebPage({ url: "https://docs.example.org", fetchImpl });
  await assert.rejects(run(async () => { throw new TypeError("Failed to fetch"); }), /CORS.*curl\/wget/);
  await assert.rejects(run(async () => new Response("binary", { headers: { "Content-Type": "application/octet-stream" } })), /not a text page/);
  await assert.rejects(run(async () => new Response("x".repeat(524289))), /512 KiB/);
  await assert.rejects(run(async () => new Response("missing", { status: 404 })), /HTTP 404/);
});

test("web reader propagates cancellation without starting a request", async () => {
  const controller = new AbortController();
  controller.abort(new Error("Stopped"));
  await assert.rejects(readWebPage({ url: "https://docs.example.org", signal: controller.signal,
    fetchImpl: () => assert.fail("must not fetch") }), /Stopped/);
});
