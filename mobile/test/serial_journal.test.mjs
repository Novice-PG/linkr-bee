import assert from "node:assert/strict";
import test from "node:test";
import { SerialJournal } from "../../web/serial_journal.js";

const encode = (text) => new TextEncoder().encode(text);

test("paged ANSI logs match whole reads without changing raw cursor offsets", () => {
  const journal = new SerialJournal();
  const raw = "OK\x1b[31mERROR\x1b[0m\r\n";
  journal.append(encode(raw));
  for (const limit of [1, 2, 3, 7]) {
    let after = 0, text = "";
    while (after < raw.length) {
      const page = journal.read({ after, limit });
      assert.equal(page.start, after);
      text += page.text;
      after = page.cursor;
    }
    assert.equal(after, raw.length);
    assert.equal(text, "OKERROR\r\n");
    assert.equal(text, journal.read().text);
  }
});

test("incremental reads never expose split CSI, OSC or string-control payloads", () => {
  const journal = new SerialJournal();
  const raw = "启动\x1b[31m失败\x1b[0m\x1b]0;HIDDEN_TITLE\x07\x1bPPRIVATE_DATA\x1b\\\r\n";
  let after = 0, text = "";
  for (const byte of encode(raw)) {
    journal.append(Uint8Array.of(byte));
    const page = journal.read({ after });
    text += page.text;
    after = page.cursor;
  }
  assert.equal(text, "启动失败\r\n");
  assert.equal(after, raw.length);
});

test("evicting a string-control prefix cannot turn its suffix into device evidence", () => {
  const journal = new SerialJournal(8);
  journal.append(encode("\x1b]0;"));
  journal.append(encode("PRIVATE_TITLE_LONGER_THAN_RING"));
  assert.equal(journal.read({ after: 0 }).text, "");
  journal.append(encode("\x1b\\OK"));
  const page = journal.read({ after: 0 });
  assert.equal(page.text, "OK");
  assert.equal(page.truncated, true);
});

test("a reset clears an unfinished control sequence along with old device data", () => {
  const journal = new SerialJournal();
  journal.append(encode("\x1b]unfinished"));
  journal.reset();
  journal.append(encode("NEW_DEVICE"));
  assert.equal(journal.read().text, "NEW_DEVICE");
  assert.equal(journal.read().latestCursor, 10);
});

test("cancelled controls resume visible output and embedded line controls remain intact", () => {
  const journal = new SerialJournal();
  journal.append(encode("\x1b]cancelled\x18OK\x1b[31\nmERROR\x1b[0m"));
  assert.equal(journal.read().text, "OK\nERROR");
});

test("control-string termination and embedded BEL agree with terminal parsing", () => {
  for (const [raw, expected] of [
    ["BEFORE\x1bPqPAYLOAD\x07PRIVATE\x1b\\AFTER", "BEFOREAFTER"],
    ["BEFORE\x1b\x07[31mERROR\x1b[0mAFTER", "BEFOREERRORAFTER"],
    ["BEFORE\x1b]unfinished\x1b[31mERROR\x1b[0mAFTER", "BEFOREERRORAFTER"],
  ]) {
    const journal = new SerialJournal();
    for (const byte of encode(raw)) journal.append(Uint8Array.of(byte));
    assert.equal(journal.read().text, expected);
    assert.equal(journal.read().latestCursor, raw.length);
  }
});
