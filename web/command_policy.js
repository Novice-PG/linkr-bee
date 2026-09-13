/* Per-target command policy, written by the user and stored next to the device
 * it applies to.
 *
 * Two lists, both matched against what would actually be typed on the wire:
 *
 * - alwaysAsk: the command always needs an explicit approval, in every
 *   execution mode including Full Auto. It can only tighten the guard, so a
 *   mistake here costs one click.
 * - allow: these exact commands may be sent unattended in Auto mode, the same
 *   way the built-in low-risk query list works. It cannot bypass a destructive
 *   guard, a pending input line, or a mode that asks by design.
 *
 * Entries stay on this device: they are never part of a model request and never
 * appear in an exported report.
 */
export const POLICY_KEY = "linkr-agent-command-policy-v1";
export const POLICY_ENTRY_LIMIT = 20;
export const POLICY_ENTRY_MAX_CHARS = 200;

function normalizeEntry(value) {
  const text = String(value ?? "").trim();
  if (!text) return "";
  if (text.length > POLICY_ENTRY_MAX_CHARS) throw new Error(`A command policy entry is limited to ${POLICY_ENTRY_MAX_CHARS} characters.`);
  if (/[\x00-\x1f\x7f]/.test(text)) throw new Error("A command policy entry must not contain control characters.");
  return text;
}

export function normalizePolicy({ alwaysAsk = [], allow = [] } = {}) {
  const clean = (list) => {
    const entries = [];
    const seen = new Set();
    for (const value of Array.isArray(list) ? list : []) {
      const text = normalizeEntry(value);
      const key = text.toLowerCase();
      if (!text || seen.has(key)) continue;
      seen.add(key);
      entries.push(text);
    }
    if (entries.length > POLICY_ENTRY_LIMIT) throw new Error(`A command policy list holds at most ${POLICY_ENTRY_LIMIT} entries.`);
    return entries;
  };
  return { alwaysAsk: clean(alwaysAsk), allow: clean(allow) };
}

/* One entry per line, which is how the panel edits both lists. */
export function parsePolicyList(text) {
  return normalizePolicy({ alwaysAsk: String(text ?? "").split(/\r?\n/) }).alwaysAsk;
}

export function formatPolicyList(entries) {
  return (Array.isArray(entries) ? entries : []).join("\n");
}

export function policyMatches(entries, text) {
  const value = String(text ?? "").toLowerCase();
  return (Array.isArray(entries) ? entries : []).some((entry) => entry && value.includes(String(entry).toLowerCase()));
}

export function isAlwaysAsk(entries, ...texts) {
  return texts.some((text) => policyMatches(entries, text));
}

export function isPreApproved(entries, command) {
  return (Array.isArray(entries) ? entries : []).some((entry) => entry === String(command ?? ""));
}

export function createCommandPolicyStore(storage) {
  const read = () => {
    try {
      const data = JSON.parse(storage.getItem(POLICY_KEY) || "{}");
      return data && typeof data === "object" && !Array.isArray(data) ? data : {};
    } catch { return {}; }
  };
  return {
    get(deviceKey) {
      if (!deviceKey) return { alwaysAsk: [], allow: [] };
      try { return normalizePolicy(read()[deviceKey] || {}); }
      catch { return { alwaysAsk: [], allow: [] }; }
    },
    save(deviceKey, policy) {
      if (!deviceKey) throw new Error("This target has no identity yet, so a command policy cannot be stored.");
      const normalized = normalizePolicy(policy);
      const data = read();
      const empty = !normalized.alwaysAsk.length && !normalized.allow.length;
      if (empty) delete data[deviceKey];
      else data[deviceKey] = normalized;
      storage.setItem(POLICY_KEY, JSON.stringify(data));
      return normalized;
    },
    clear(deviceKey) {
      const data = read();
      delete data[deviceKey];
      storage.setItem(POLICY_KEY, JSON.stringify(data));
    },
  };
}
