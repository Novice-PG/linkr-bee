import assert from "node:assert/strict";
import test from "node:test";
import { AGENT_CONFIG_KEY, loadAgentConfig, saveAgentConfig, clearAgentConfig, endpointSecurity } from "../../web/agent_config.js";

function storage() {
  const values = new Map();
  return { getItem: (key) => values.get(key) ?? null,
    setItem: (key, value) => values.set(key, value), removeItem: (key) => values.delete(key) };
}
const config = { endpoint: "https://model.test/v1/", model: " model ", apiKey: "device-key" };

test("model settings round-trip endpoint, model and key, and can be removed", () => {
  const store = storage();
  const saved = saveAgentConfig(store, config);
  assert.deepEqual(saved, { endpoint: "https://model.test/v1", model: "model", apiKey: "device-key" });
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
