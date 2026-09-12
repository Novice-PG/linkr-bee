import assert from "node:assert/strict";
import test from "node:test";
import { AGENT_CONFIG_KEY, loadAgentConfig, saveAgentConfig, clearAgentConfig, endpointSecurity, isEndpointUnreachable, parseAgentHeaders, formatAgentHeaders } from "../../web/agent_config.js";

function storage() {
  const values = new Map();
  return { getItem: (key) => values.get(key) ?? null,
    setItem: (key, value) => values.set(key, value), removeItem: (key) => values.delete(key) };
}
const config = { endpoint: "https://model.test/v1/", model: " model ", apiKey: "device-key" };

test("model settings round-trip endpoint, model and key, and can be removed", () => {
  const store = storage();
  const saved = saveAgentConfig(store, config);
  assert.deepEqual(saved, { endpoint: "https://model.test/v1", model: "model", apiKey: "device-key", headers: {} });
  assert.deepEqual(loadAgentConfig(store), saved);
  clearAgentConfig(store);
  assert.equal(loadAgentConfig(store), null);
});

test("legacy model settings migrate without a key; corrupt settings are ignored", () => {
  const store = storage();
  store.setItem(AGENT_CONFIG_KEY, JSON.stringify({ endpoint: config.endpoint, model: config.model }));
  assert.equal(loadAgentConfig(store).apiKey, "");
  for (const raw of ["broken json", "null", "[]", '{"endpoint":"https://host","model":3}',
    JSON.stringify({ ...config, apiKey: {} }), JSON.stringify({ ...config, endpoint: "javascript:alert(1)" })]) {
    store.setItem(AGENT_CONFIG_KEY, raw);
    assert.equal(loadAgentConfig(store), null);
  }
});

test("invalid model edits leave the previously saved configuration intact", () => {
  const store = storage();
  const saved = saveAgentConfig(store, config);
  for (const endpoint of ["", "file:///model", "https://secret@host/v1", "https://host?key=secret", "https://host/#x"]) {
    assert.throws(() => saveAgentConfig(store, { ...config, endpoint }));
    assert.deepEqual(loadAgentConfig(store), saved);
  }
  assert.throws(() => saveAgentConfig(store, { ...config, model: " " }));
  assert.deepEqual(loadAgentConfig(store), saved);
});

test("unavailable storage propagates save and clear failure", () => {
  const store = { getItem() { throw new Error("blocked"); },
    setItem() { throw new Error("quota"); }, removeItem() { throw new Error("blocked"); } };
  assert.throws(() => loadAgentConfig(store), /blocked/);
  assert.throws(() => saveAgentConfig(store, config), /quota/);
  assert.throws(() => clearAgentConfig(store), /blocked/);
});

test("plaintext remote endpoints are reported, loopback and https are not", () => {
  // A credential sent over cleartext to a remote host needs consent.
  for (const endpoint of ["http://model.test/v1", "http://192.168.1.9:8080/v1", "http://[2001:db8::1]/v1", "http://model.test"]) {
    const security = endpointSecurity(endpoint);
    assert.equal(security.plaintext, true, endpoint);
    assert.equal(security.exposesKey, true, endpoint);
  }

  // A local model server never leaves the machine, so it stays unremarkable.
  for (const endpoint of ["http://localhost:11434/v1", "http://127.0.0.1:8080/v1", "http://127.9.9.9/v1", "http://[::1]:8080/v1", "http://ollama.localhost/v1"]) {
    const security = endpointSecurity(endpoint);
    assert.equal(security.plaintext, true, endpoint);
    assert.equal(security.exposesKey, false, endpoint);
  }

  // https is encrypted regardless of host, and junk must not claim to be safe
  // or unsafe by accident.
  assert.deepEqual(endpointSecurity("https://model.test/v1"), { plaintext: false, loopback: false, exposesKey: false });
  assert.deepEqual(endpointSecurity("https://localhost/v1"), { plaintext: false, loopback: true, exposesKey: false });
  for (const bad of ["", "not a url", null, undefined, "ftp://model.test"]) {
    assert.equal(endpointSecurity(bad).exposesKey, false, String(bad));
  }
});

test("only requests that never reached the endpoint are reported as unreachable", () => {
  // Browser engines word a blocked or unreachable cross-origin request differently.
  for (const message of ["Failed to fetch", "NetworkError when attempting to fetch resource.",
    "Load failed", "Network request failed", "fetch failed", "Connection error."]) {
    assert.equal(isEndpointUnreachable(new Error(message)), true, message);
  }
  // The OpenAI SDK wraps a transport failure in its own error type.
  const connectionError = new Error("Connection error.");
  connectionError.name = "APIConnectionError";
  assert.equal(isEndpointUnreachable(connectionError), true);
  for (const error of [new Error("401 Unauthorized"), new Error("model not found"),
    new Error("endpoint"), new Error("The model returned an empty response"), null, undefined]) {
    assert.equal(isEndpointUnreachable(error), false, String(error));
  }
});

test("extra request headers round-trip and cannot inject a new request line", () => {
  assert.deepEqual(parseAgentHeaders("anthropic-dangerous-direct-browser-access: true"), { "anthropic-dangerous-direct-browser-access": "true" });
  assert.deepEqual(parseAgentHeaders("X-Api-Key: secret\n\nX-Trace: abc  "), { "X-Api-Key": "secret", "X-Trace": "abc" });
  assert.deepEqual(parseAgentHeaders(""), {});
  assert.equal(formatAgentHeaders({ "X-Api-Key": "secret", "X-Trace": "abc" }), "X-Api-Key: secret\nX-Trace: abc");
  assert.equal(formatAgentHeaders(undefined), "");

  for (const bad of ["no colon", ": value", "Bad Name: value", "X-Test:", "X-Test: " + "x".repeat(257)]) {
    assert.throws(() => parseAgentHeaders(bad), Error, bad);
  }
  // The text area is line-based, so one line is one header, and only the first
  // colon separates the name (a URL value keeps its own colons).
  assert.deepEqual(parseAgentHeaders("X-Test: ok\r\nX-Evil: 1"), { "X-Test": "ok", "X-Evil": "1" });
  assert.deepEqual(parseAgentHeaders("Referer: https://host/x"), { Referer: "https://host/x" });
  // A stored record is not line-based: a newline in a value must be rejected,
  // otherwise it would add its own header to the request.
  assert.throws(() => parseAgentHeaders(Array.from({ length: 9 }, (_, i) => `X-${i}: v`).join("\n")), /headers/);
  assert.throws(() => validateAgentConfig({ endpoint: "https://host/v1", model: "m", headers: { "X-Test": "ok\nX-Evil: 1" } }), Error);
});

test("saved configuration carries the custom headers", () => {
  const store = storage();
  saveAgentConfig(store, { ...config, headers: { "X-Api-Key": "secret" } });
  assert.deepEqual(loadAgentConfig(store).headers, { "X-Api-Key": "secret" });
  // Records written before this field existed load with no headers.
  store.setItem(AGENT_CONFIG_KEY, JSON.stringify({ endpoint: "https://model.test/v1", model: "m", apiKey: "k" }));
  assert.deepEqual(loadAgentConfig(store).headers, {});
});
