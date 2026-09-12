/* Token accounting for the assistant.
 *
 * Every assistant message carries the provider's Usage record; the panel adds
 * them up per conversation and can estimate a cost when the user enters the
 * endpoint's prices. Prices are display-only: they never take part in a model
 * request, so changing them must not invalidate the running conversation.
 */
export const PRICING_KEY = "linkr-agent-pricing-v1";

export function emptyUsage() {
  return { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0, messages: 0 };
}

export function addUsage(total, usage) {
  const next = { ...emptyUsage(), ...total };
  if (!usage) return next;
  const number = (value) => (Number.isFinite(Number(value)) ? Number(value) : 0);
  next.input += number(usage.input);
  next.output += number(usage.output);
  next.cacheRead += number(usage.cacheRead);
  next.cacheWrite += number(usage.cacheWrite);
  next.totalTokens += number(usage.totalTokens) ||
    number(usage.input) + number(usage.output) + number(usage.cacheRead) + number(usage.cacheWrite);
  next.messages += 1;
  return next;
}

/* Prices are quoted per million tokens, the way providers list them. */
export function parsePricing({ input, output } = {}) {
  const rate = (value) => {
    if (value === "" || value === null || value === undefined) return 0;
    const number = Number(value);
    if (!Number.isFinite(number) || number < 0 || number > 100000) throw new Error("pricing");
    return number;
  };
  return { input: rate(input), output: rate(output) };
}

export function loadPricing(storage) {
  try { return parsePricing(JSON.parse(storage.getItem(PRICING_KEY) || "{}")); }
  catch { return { input: 0, output: 0 }; }
}

export function savePricing(storage, pricing) {
  const normalized = parsePricing(pricing);
  storage.setItem(PRICING_KEY, JSON.stringify(normalized));
  return normalized;
}

export function estimateCost(usage, pricing) {
  const rates = parsePricing(pricing || {});
  if (!rates.input && !rates.output) return null;
  const input = (usage?.input || 0) / 1e6 * rates.input;
  const output = (usage?.output || 0) / 1e6 * rates.output;
  return { input, output, total: input + output };
}

export function formatTokens(value) {
  return Number(value || 0).toLocaleString("en-US");
}

/* Costs are small; show enough digits to be useful for a single conversation
 * without pretending to more precision than the listed price has. */
export function formatCost(amount) {
  const value = Number(amount || 0);
  if (!value) return "0";
  if (value < 0.01) return value.toFixed(4);
  if (value < 1) return value.toFixed(3);
  return value.toFixed(2);
}
