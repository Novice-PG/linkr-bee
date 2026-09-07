export const AGENT_CONFIG_KEY = "linkr-agent-model";

export function validateAgentConfig({ endpoint, model, apiKey = "" } = {}) {
  let url;
  try { url = new URL(endpoint); } catch { throw new Error("endpoint"); }
  if (!["https:", "http:"].includes(url.protocol) || url.username || url.password || url.search || url.hash) {
    throw new Error("endpoint");
  }
  if (typeof model !== "string" || !model.trim()) throw new Error("model");
  if (typeof apiKey !== "string") throw new Error("apiKey");
  return { endpoint: url.href.replace(/\/$/, ""), model: model.trim(), apiKey };
}

// Shared by browser, Capacitor and ArkWeb. Storage errors are surfaced by the UI;
// never report a successful save or clear when the persistent write failed.
export function loadAgentConfig(storage) {
  const raw = storage.getItem(AGENT_CONFIG_KEY);
  if (!raw) return null;
  try { return validateAgentConfig(JSON.parse(raw)); } catch { return null; }
}

export function saveAgentConfig(storage, config) {
  const normalized = validateAgentConfig(config);
  storage.setItem(AGENT_CONFIG_KEY, JSON.stringify(normalized));
  return normalized;
}

export function clearAgentConfig(storage) {
  storage.removeItem(AGENT_CONFIG_KEY);
}
