/* Durable notes about a target, written by the assistant and kept per device.
 *
 * The observed profile and the tool capabilities live in memory and are
 * discarded on reconnect; a note is the opposite: something the assistant
 * verified once and wants on hand the next time this target is connected
 * ("bootloader needs raw Enter", "curl is missing; wget is present"). Notes are
 * free text, so they are redacted like task summaries and capped per device;
 * credentials, hypotheses and transient state must never be stored here.
 */
import { redactTaskText } from "./agent_tasks.js";

export const NOTES_KEY = "linkr-agent-notes-v1";
export const NOTE_LIMIT = 12;
export const NOTE_MAX_CHARS = 600;
export const NOTE_EVIDENCE_MAX_CHARS = 200;

export function createNoteStore(storage) {
  const read = () => {
    try {
      const data = JSON.parse(storage.getItem(NOTES_KEY) || "[]");
      return Array.isArray(data)
        ? data.filter((note) => note && typeof note.id === "string" && typeof note.deviceKey === "string" && typeof note.text === "string")
        : [];
    } catch { return []; }
  };
  const write = (notes) => storage.setItem(NOTES_KEY, JSON.stringify(notes));
  const forDevice = (notes, deviceKey) => notes.filter((note) => note.deviceKey === deviceKey);

  return {
    list(deviceKey) {
      if (!deviceKey) return [];
      return forDevice(read(), deviceKey)
        .map((note) => ({ id: note.id, text: note.text, evidence: note.evidence || "", createdAt: note.createdAt || 0 }))
        .sort((left, right) => left.createdAt - right.createdAt);
    },
    add(deviceKey, { text, evidence = "" } = {}) {
      if (typeof deviceKey !== "string" || !deviceKey) throw new Error("This target has no identity yet, so a note cannot be stored.");
      const clean = redactTaskText(String(text ?? "").trim()).slice(0, NOTE_MAX_CHARS).trim();
      if (!clean) throw new Error("The note is empty after redaction.");
      if (String(text ?? "").trim().length > NOTE_MAX_CHARS) throw new Error(`A note is limited to ${NOTE_MAX_CHARS} characters.`);
      const notes = read();
      const existing = forDevice(notes, deviceKey);
      if (existing.some((note) => note.text === clean)) {
        return { ...existing.find((note) => note.text === clean), duplicate: true };
      }
      const note = {
        id: `note-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`,
        deviceKey, text: clean,
        evidence: redactTaskText(String(evidence ?? "").trim()).slice(0, NOTE_EVIDENCE_MAX_CHARS),
        createdAt: Date.now(),
      };
      // Oldest first out: the newest observation about a target is the one that
      // matters, and the cap keeps the record bounded without user action.
      let kept = notes;
      if (existing.length >= NOTE_LIMIT) {
        const overflow = existing.length - NOTE_LIMIT + 1;
        const dropped = new Set(existing.slice(0, overflow).map((item) => item.id));
        kept = notes.filter((item) => !dropped.has(item.id));
      }
      write(kept.concat(note).slice(-(NOTE_LIMIT * 4)));
      return { ...note, duplicate: false };
    },
    remove(deviceKey, id) {
      write(read().filter((note) => !(note.deviceKey === deviceKey && note.id === id)));
    },
    clear(deviceKey) {
      write(read().filter((note) => note.deviceKey !== deviceKey));
    },
  };
}
