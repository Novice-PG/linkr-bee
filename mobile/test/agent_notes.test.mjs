import assert from "node:assert/strict";
import test from "node:test";
import { createNoteStore, NOTE_LIMIT, NOTES_KEY } from "../../web/agent_notes.js";

function storage() {
  const values = new Map();
  return { getItem: (key) => values.get(key) ?? null,
    setItem: (key, value) => values.set(key, value), removeItem: (key) => values.delete(key) };
}

test("notes are kept per device and survive a reload", () => {
  const store = storage();
  const notes = createNoteStore(store);
  const first = notes.add("target:abc", { text: "Bootloader needs raw Enter", evidence: "U-Boot prompt at 115200" });
  assert.equal(first.duplicate, false);
  notes.add("target:abc", { text: "wget present, curl missing", evidence: "probe_tools exit 0" });
  notes.add("target:other", { text: "Console is 57600 8N1" });

  const reloaded = createNoteStore(store);
  assert.deepEqual(reloaded.list("target:abc").map((note) => note.text),
    ["Bootloader needs raw Enter", "wget present, curl missing"]);
  assert.deepEqual(reloaded.list("target:other").map((note) => note.text), ["Console is 57600 8N1"]);
  assert.deepEqual(reloaded.list(""), []);
});

test("a repeated note is reported instead of stored twice", () => {
  const notes = createNoteStore(storage());
  notes.add("target:abc", { text: "RTC battery is dead" });
  const again = notes.add("target:abc", { text: "RTC battery is dead" });
  assert.equal(again.duplicate, true);
  assert.equal(notes.list("target:abc").length, 1);
});

test("notes are redacted, bounded and validated", () => {
  const store = storage();
  const notes = createNoteStore(store);
  // The shared redaction covers the keyword:value forms; the tool description
  // and the system prompt forbid writing credentials in any other form.
  const note = notes.add("target:abc", {
    text: "login uses password=hunter2 then run diag",
    evidence: "read from http://user:pw@host/x?token=abc",
  });
  assert.doesNotMatch(note.text, /hunter2/);
  assert.match(note.text, /password=\[redacted\]/);
  assert.doesNotMatch(note.evidence, /token=abc/);
  assert.doesNotMatch(note.evidence, /user:pw/);
  assert.throws(() => notes.add("target:abc", { text: "   " }), /empty/i);
  assert.throws(() => notes.add("", { text: "no device" }), /identity/);
  assert.throws(() => notes.add("target:abc", { text: "x".repeat(601) }), /600/);
});

test("the per-device cap drops the oldest note", () => {
  const notes = createNoteStore(storage());
  for (let index = 0; index < NOTE_LIMIT + 3; index++) {
    notes.add("target:abc", { text: `fact ${index}` });
  }
  const kept = notes.list("target:abc");
  assert.equal(kept.length, NOTE_LIMIT);
  assert.equal(kept.at(-1).text, `fact ${NOTE_LIMIT + 2}`);
  assert.equal(kept.some((note) => note.text === "fact 0"), false);
});

test("notes can be removed individually or for a device", () => {
  const notes = createNoteStore(storage());
  const one = notes.add("target:abc", { text: "first" });
  notes.add("target:abc", { text: "second" });
  notes.remove("target:abc", one.id);
  assert.deepEqual(notes.list("target:abc").map((note) => note.text), ["second"]);
  notes.clear("target:abc");
  assert.deepEqual(notes.list("target:abc"), []);
});

test("a corrupt record is ignored instead of breaking the panel", () => {
  const store = storage();
  store.setItem(NOTES_KEY, "{not json");
  const notes = createNoteStore(store);
  assert.deepEqual(notes.list("target:abc"), []);
  notes.add("target:abc", { text: "still works" });
  assert.equal(notes.list("target:abc").length, 1);
});
