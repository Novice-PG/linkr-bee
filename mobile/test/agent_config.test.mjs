import assert from "node:assert/strict";
import test from "node:test";
import { AGENT_CONFIG_KEY, loadAgentConfig, saveAgentConfig, clearAgentConfig } from "../../web/agent_config.js";

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
