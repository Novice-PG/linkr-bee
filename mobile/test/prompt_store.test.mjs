import assert from "node:assert/strict";
import test from "node:test";
import {
  PROMPT_CLEAR_COMMAND,
  PROMPT_MAX_BYTES,
  PROMPT_QUERY_COMMAND,
  assemblePrompt,
  normalizePromptText,
  parsePromptChunk,
  parsePromptStatus,
  promptFits,
  promptGetCommand,
  promptSetCommand,
} from "../../web/prompt_store.js";

test("prompt commands use the single-prompt board protocol", () => {
  assert.equal(PROMPT_QUERY_COMMAND, "@p?");
  assert.equal(PROMPT_CLEAR_COMMAND, "@p clear");
  assert.equal(promptGetCommand(), "@p get");
  assert.equal(promptGetCommand({ offset: 96 }), "@p get 96");
  assert.throws(() => promptGetCommand({ offset: -1 }), /offset/);
});

test("prompt text is normalised, capped and hex encoded", () => {
  assert.equal(normalizePromptText("  a\r\nb  "), "a\nb");
  assert.equal(promptFits("a".repeat(PROMPT_MAX_BYTES)), true);
  assert.equal(promptFits("a".repeat(PROMPT_MAX_BYTES + 1)), false);
  assert.equal(promptSetCommand({ text: "你好" }), "@p set e4bda0e5a5bd");
  assert.throws(() => promptSetCommand({ text: "   " }), /empty/);
  assert.throws(() => promptSetCommand({ text: "a".repeat(241) }), /longer/);
});

test("prompt status and chunk replies are parsed", () => {
  assert.deepEqual(parsePromptStatus("OK prompt=140\r\n"), { bytes: 140 });
  assert.deepEqual(parsePromptStatus("OK prompt=0"), { bytes: 0 });
  assert.equal(parsePromptStatus("ERR prompt=empty"), null);

  const chunk = parsePromptChunk("OK p off=0 hex=68656c6c6f total=5 done=yes\r\n");
  assert.equal(chunk.offset, 0);
  assert.equal(chunk.total, 5);
  assert.equal(chunk.done, true);
  assert.equal(chunk.chunk, "hello");
  assert.equal(parsePromptChunk("OK p off=5 hex= total=5 done=yes").chunk, "");
  assert.equal(parsePromptChunk("ERR prompt=empty"), null);
});

test("chunks assemble in order and refuse incomplete reads", () => {
  const chunks = [
    { offset: 0, hex: "68656c6c6f", chunk: "hello" },
    { offset: 5, hex: "20776f726c64", chunk: " world" },
  ];
  assert.equal(assemblePrompt(chunks, 11), "hello world");
  assert.equal(assemblePrompt([...chunks].reverse(), 11), "hello world");
  assert.throws(() => assemblePrompt([chunks[1]], 11), /out of order/);
  assert.throws(() => assemblePrompt(chunks, 20), /incomplete/);
});


test("UTF-8 is decoded after all byte chunks arrive", () => {
  for (const text of ["a" + "中".repeat(20), "x".repeat(47) + "🙂中文", "中文🙂".repeat(20)]) {
    const bytes = new TextEncoder().encode(text);
    const chunks = [];
    for (let offset = 0; offset < bytes.length; offset += 48) {
      const hex = Buffer.from(bytes.slice(offset, offset + 48)).toString("hex");
      chunks.push(parsePromptChunk(`OK p off=${offset} hex=${hex} total=${bytes.length} done=${offset + 48 >= bytes.length ? "yes" : "no"}`));
    }
    assert.equal(assemblePrompt(chunks, bytes.length), text);
  }
});
