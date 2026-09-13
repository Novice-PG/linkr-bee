/* Conversation continuity across a reload.
 *
 * Only the panel's own transcript is stored, keyed by device identity: the
 * display lines the user saw, and the model messages the runtime needs to
 * continue. Restored history is history, not evidence — the panel marks it as
 * such and the runtime compacts it like an interrupted run, so the assistant
 * still has to re-read the device before acting.
 *
 * Storage is bounded three times: a cap on messages per device, a cap on the
 * serialized size of one record, and a cap on how many devices are remembered.
 * A diagnostic conversation can otherwise grow without limit, and localStorage
 * is a shared, small quota.
 */
export const SESSION_KEY = "linkr-agent-session-v1";
export const SESSION_MESSAGES = 40;
export const SESSION_DISPLAY = 60;
export const SESSION_ENTRY_CHARS = 2000;
export const SESSION_MAX_BYTES = 96000;
export const SESSION_DEVICES = 8;

function trimDisplay(display) {
  return (Array.isArray(display) ? display : [])
    .filter((entry) => entry && (entry.role === "user" || entry.role === "assistant") && typeof entry.text === "string" && entry.text.trim())
    .map((entry) => ({ role: entry.role, text: entry.text.slice(0, SESSION_ENTRY_CHARS) }))
    .slice(-SESSION_DISPLAY);
}

function size(messages) {
  try { return JSON.stringify(messages).length; } catch { return Infinity; }
}

export function saveSession(storage, deviceKey, { display = [], messages = [] } = {}) {
  if (!deviceKey) return null;
  let kept = Array.isArray(messages) ? messages.slice(-SESSION_MESSAGES) : [];
  // Drop the oldest turns until the record fits, so a long log excerpt cannot
  // make the session unstorable.
  while (kept.length > 1 && size(kept) > SESSION_MAX_BYTES) kept = kept.slice(1);
  const session = size(kept) > SESSION_MAX_BYTES || !kept.length && !display.length
    ? null
    : { display: trimDisplay(display), messages: kept, updatedAt: Date.now() };
  let data = {};
  try { data = JSON.parse(storage.getItem(SESSION_KEY) || "{}") || {}; } catch { data = {}; }
  if (session) data[deviceKey] = session;
  else delete data[deviceKey];
  // One record per board seen, oldest first: a bench that cycles through boards
  // must not fill the origin's quota with conversations nobody will reopen.
  const keys = Object.keys(data);
  if (keys.length > SESSION_DEVICES) {
    keys.map((key) => [key, Number(data[key]?.updatedAt) || 0])
      .sort((a, b) => a[1] - b[1])
      .slice(0, keys.length - SESSION_DEVICES)
      .forEach(([key]) => { delete data[key]; });
  }
  try { storage.setItem(SESSION_KEY, JSON.stringify(data)); } catch { return null; }
  return session;
}

export function loadSession(storage, deviceKey) {
  if (!deviceKey) return null;
  let data;
  try { data = JSON.parse(storage.getItem(SESSION_KEY) || "{}"); } catch { return null; }
  const session = data && typeof data === "object" ? data[deviceKey] : null;
  if (!session || typeof session !== "object") return null;
  const messages = Array.isArray(session.messages) ? session.messages.slice(-SESSION_MESSAGES) : [];
  const display = trimDisplay(session.display);
  if (!messages.length && !display.length) return null;
  return { display, messages, updatedAt: Number(session.updatedAt) || 0 };
}

export function clearSession(storage, deviceKey) {
  if (!deviceKey) return;
  let data;
  try { data = JSON.parse(storage.getItem(SESSION_KEY) || "{}"); } catch { return; }
  if (!data || typeof data !== "object") return;
  delete data[deviceKey];
  try { storage.setItem(SESSION_KEY, JSON.stringify(data)); } catch { /* nothing to free */ }
}
