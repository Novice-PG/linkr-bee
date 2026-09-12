export const AGENT_CONFIG_KEY = "linkr-agent-model";

/* Plain http sends the API key and the serial logs in cleartext. A local model
 * server reached over loopback never leaves the machine, so it stays a normal
 * configuration; anything else is reported so the UI can ask for consent before
 * a credential is stored and transmitted. */
export function endpointSecurity(endpoint) {
  let url;
  try {
    url = new URL(String(endpoint ?? ""));
  } catch {
    return { plaintext: false, loopback: false, exposesKey: false };
  }
  const host = url.hostname.replace(/^\[|\]$/g, "").toLowerCase();
  const loopback =
    host === "localhost" ||
    host === "::1" ||
    host.endsWith(".localhost") ||
    /^127\./.test(host);
  const plaintext = url.protocol === "http:";
  return { plaintext, loopback, exposesKey: plaintext && !loopback };
}

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

/* A request that never reached the model arrives as an opaque transport error:
 * the host is unreachable, the browser blocked the cross-origin request (CORS),
 * or the device is offline. Engines word it differently, and the OpenAI SDK
 * wraps it as "Connection error." / APIConnectionError, so both are recognised.
 * Reported separately because the fix is browser-side, not model-side. */
export function isEndpointUnreachable(error) {
  if (String(error?.name || "") === "APIConnectionError") return true;
  const message = String(error?.message || error || "");
  return /failed to fetch|fetch failed|networkerror|load failed|network request failed|connection error/i.test(message);
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
