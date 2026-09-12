import assert from "node:assert/strict";
import test from "node:test";
import {
  addUsage,
  emptyUsage,
  estimateCost,
  formatCost,
  formatTokens,
  loadPricing,
  parsePricing,
  savePricing,
} from "../../web/agent_usage.js";

function storage() {
  const values = new Map();
  return { getItem: (key) => values.get(key) ?? null,
    setItem: (key, value) => values.set(key, value), removeItem: (key) => values.delete(key) };
}

const usage = (input, output, extra = {}) => ({ input, output, cacheRead: 0, cacheWrite: 0, totalTokens: input + output, ...extra });

test("usage accumulates across a conversation", () => {
  let total = emptyUsage();
  total = addUsage(total, usage(1000, 200));
  total = addUsage(total, usage(3000, 400, { cacheRead: 500, totalTokens: 3900 }));
  // 4000 in + 600 out + 500 cache reads; the reported totals are summed too.
  assert.deepEqual(total, { input: 4000, output: 600, cacheRead: 500, cacheWrite: 0, totalTokens: 5100, messages: 2 });
  // A missing or partial record must not corrupt the totals.
  assert.deepEqual(addUsage(total, null), total);
  assert.deepEqual(addUsage(total, { input: undefined, output: "x" }), { ...total, messages: 3 });
});

test("cost is estimated only when the user configured prices", () => {
  const total = addUsage(emptyUsage(), usage(1_000_000, 500_000));
  assert.equal(estimateCost(total, { input: 0, output: 0 }), null);
  assert.equal(estimateCost(total, undefined), null);
  const cost = estimateCost(total, { input: 2, output: 8 });
  assert.equal(cost.input, 2);
  assert.equal(cost.output, 4);
  assert.equal(cost.total, 6);
});

test("prices are validated and stored outside the model configuration", () => {
  const store = storage();
  assert.deepEqual(loadPricing(store), { input: 0, output: 0 });
  assert.deepEqual(savePricing(store, { input: "0.27", output: "1.1" }), { input: 0.27, output: 1.1 });
  assert.deepEqual(loadPricing(store), { input: 0.27, output: 1.1 });
  assert.deepEqual(parsePricing({ input: "", output: null }), { input: 0, output: 0 });
  for (const bad of [{ input: -1 }, { output: "free" }, { input: 100001 }]) {
    assert.throws(() => parsePricing(bad), /pricing/, JSON.stringify(bad));
  }
  store.setItem("linkr-agent-pricing-v1", "{broken");
  assert.deepEqual(loadPricing(store), { input: 0, output: 0 });
});

test("token and cost formatting stays readable at both ends of the range", () => {
  assert.equal(formatTokens(1234567), "1,234,567");
  assert.equal(formatTokens(0), "0");
  assert.equal(formatCost(0), "0");
  assert.equal(formatCost(0.0001234), "0.0001");
  assert.equal(formatCost(0.0123), "0.012");
  assert.equal(formatCost(1.2345), "1.23");
});
