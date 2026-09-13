import assert from "node:assert/strict";
import test from "node:test";
import {
  SESSION_DEVICES,
  SESSION_DISPLAY,
  SESSION_ENTRY_CHARS,
  SESSION_KEY,
  SESSION_MESSAGES,
  clearSession,
  loadSession,
  saveSession,
} from "../../web/agent_session.js";

function storage() {
  const values = new Map();
  return { getItem: (key) => values.get(key) ?? null,
    setItem: (key, value) => values.set(key, value), removeItem: (key) => values.delete(key) };
}
const message = (index) => ({ role: "user", content: [{ type: "text", text: `message ${index}` }], timestamp: index });

test("a session round-trips the display and the model messages", () => {
  const store = storage();
  saveSession(store, "target:abc", {
    display: [{ role: "user", text: "why did boot fail?" }, { role: "assistant", text: "root fs missing" }],
    messages: [message(1), message(2)],
  });
  const session = loadSession(store, "target:abc");
  assert.deepEqual(session.display, [{ role: "user", text: "why did boot fail?" }, { role: "assistant", text: "root fs missing" }]);
  assert.equal(session.messages.length, 2);
  assert.equal(loadSession(store, "target:other"), null);
  assert.equal(loadSession(store, ""), null);
});

test("storage stays bounded in entries, in entry length and in total size", () => {
  const store = storage();
  saveSession(store, "target:abc", {
    display: Array.from({ length: SESSION_DISPLAY + 10 }, (_, index) => ({ role: "user", text: `line ${index}` })),
    messages: Array.from({ length: SESSION_MESSAGES + 10 }, (_, index) => message(index)),
  });
  let session = loadSession(store, "target:abc");
  assert.equal(session.display.length, SESSION_DISPLAY);
  assert.equal(session.display.at(-1).text, `line ${SESSION_DISPLAY + 9}`);
  assert.equal(session.messages.length, SESSION_MESSAGES);

  // A long excerpt is cut at the entry cap, and the oldest turns are dropped
  // until the record fits the byte budget.
  saveSession(store, "target:abc", {
    display: [{ role: "assistant", text: "x".repeat(SESSION_ENTRY_CHARS + 500) }],
    messages: Array.from({ length: SESSION_MESSAGES }, (_, index) => ({
      role: "toolResult", content: [{ type: "text", text: `${index}:` + "y".repeat(8000) }],
    })),
  });
  session = loadSession(store, "target:abc");
  assert.equal(session.display[0].text.length, SESSION_ENTRY_CHARS);
  assert.ok(JSON.stringify(session.messages).length <= 96000, String(JSON.stringify(session.messages).length));
  // The newest turn survives the trimming.
  assert.match(JSON.stringify(session.messages.at(-1)), /39:/);
});

test("only the most recently used devices are remembered", () => {
  const store = storage();
  const realNow = Date.now;
  try {
    // The loop would otherwise finish inside one millisecond, leaving the order
    // of "most recent" to chance.
    for (let index = 0; index < SESSION_DEVICES + 3; index++) {
      Date.now = () => 1000 + index;
      saveSession(store, `target:${index}`, {
        display: [{ role: "user", text: `board ${index}` }], messages: [message(index)],
      });
    }
  } finally { Date.now = realNow; }
  const kept = Object.keys(JSON.parse(store.getItem(SESSION_KEY)));
  assert.equal(kept.length, SESSION_DEVICES);
  assert.ok(!kept.includes("target:0"), kept.join(","));
  // The board used last is the one that must survive.
  assert.ok(loadSession(store, `target:${SESSION_DEVICES + 2}`));
});

test("an empty session removes the record instead of storing a stub", () => {
  const store = storage();
  saveSession(store, "target:abc", { display: [{ role: "user", text: "hi" }], messages: [message(1)] });
  saveSession(store, "target:abc", { display: [], messages: [] });
  assert.equal(loadSession(store, "target:abc"), null);
  assert.equal(JSON.parse(store.getItem(SESSION_KEY))["target:abc"], undefined);
});

test("clearing one device leaves the others alone, and corrupt storage is ignored", () => {
  const store = storage();
  saveSession(store, "target:abc", { display: [{ role: "user", text: "a" }], messages: [message(1)] });
  saveSession(store, "target:other", { display: [{ role: "user", text: "b" }], messages: [message(2)] });
  clearSession(store, "target:abc");
  assert.equal(loadSession(store, "target:abc"), null);
  assert.equal(loadSession(store, "target:other").display[0].text, "b");

  store.setItem(SESSION_KEY, "{not json");
  assert.equal(loadSession(store, "target:other"), null);
  assert.doesNotThrow(() => saveSession(store, "target:abc", { display: [{ role: "user", text: "c" }], messages: [] }));
  assert.equal(loadSession(store, "target:abc").display[0].text, "c");
});

test("display entries are filtered to what the panel can render", () => {
  const store = storage();
  saveSession(store, "target:abc", {
    display: [{ role: "system", text: "internal" }, { role: "user", text: "   " }, { role: "assistant", text: "kept" }, null],
    messages: [message(1)],
  });
  assert.deepEqual(loadSession(store, "target:abc").display, [{ role: "assistant", text: "kept" }]);
});
