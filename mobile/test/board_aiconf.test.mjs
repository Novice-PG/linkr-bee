import assert from "node:assert/strict";
import test from "node:test";
import {
  AICONF_BEGIN_COMMAND,
  AICONF_CLEAR_COMMAND,
  AICONF_END_COMMAND,
  AICONF_MAX_BYTES,
  AICONF_QUERY_COMMAND,
  aiconfGetCommand,
  aiconfHexChunks,
  aiconfPutCommand,
  assembleAiconf,
  parseAiconfChunk,
  parseAiconfPayload,
  parseAiconfStatus,
  serializeAiconf,
} from "../../web/board_aiconf.js";

const config = {
  endpoint: "https://api.example.com/v1",
  model: "demo-model",
  provider: "openai-completions",
  reasoning: "off",
  contextWindow: 0,
  maxTokens: 0,
  apiKey: "sk-test-123",
  headers: { "x-extra": "yes" },
};

test("AI configuration commands use the board protocol", () => {
  assert.equal(AICONF_QUERY_COMMAND, "@y?");
  assert.equal(AICONF_BEGIN_COMMAND, "@y begin");
  assert.equal(AICONF_END_COMMAND, "@y end");
  assert.equal(AICONF_CLEAR_COMMAND, "@y clear");
  assert.equal(aiconfGetCommand(), "@y get 0");
  assert.equal(aiconfGetCommand({ offset: 48 }), "@y get 48");
  assert.equal(aiconfPutCommand("616263"), "@y put 616263");
  assert.throws(() => aiconfGetCommand({ offset: -1 }), /offset/);
});

test("the blob keeps secrets only when asked", () => {
  const publicRecord = JSON.parse(serializeAiconf(config));
  assert.deepEqual(publicRecord, {
    v: 1,
    endpoint: config.endpoint,
    model: config.model,
    provider: config.provider,
    reasoning: config.reasoning,
    contextWindow: 0,
    maxTokens: 0,
  });
  const secretRecord = JSON.parse(serializeAiconf(config, { includeSecrets: true }));
  assert.equal(secretRecord.apiKey, "sk-test-123");
  assert.deepEqual(secretRecord.headers, { "x-extra": "yes" });
});

test("the blob parses back with and without secrets", () => {
  const publicRecord = parseAiconfPayload(serializeAiconf(config));
  assert.equal(publicRecord.apiKey, "");
  assert.equal(publicRecord.hasSecrets, false);
  assert.equal(publicRecord.endpoint, config.endpoint);
  const secretRecord = parseAiconfPayload(serializeAiconf(config, { includeSecrets: true }));
  assert.equal(secretRecord.apiKey, "sk-test-123");
  assert.equal(secretRecord.hasSecrets, true);
  assert.throws(() => parseAiconfPayload("not json"), /valid JSON/);
  assert.throws(() => parseAiconfPayload('{"v":9}'), /unsupported version/);
});

test("status and chunk replies are parsed", () => {
  assert.deepEqual(parseAiconfStatus("OK aiconf bytes=140 max=960\r\n"), { bytes: 140, max: 960 });
  assert.deepEqual(parseAiconfStatus("OK aiconf bytes=0 max=960"), { bytes: 0, max: 960 });
  assert.equal(parseAiconfStatus("ERR aiconf=empty"), null);

  const chunk = parseAiconfChunk("OK aiconf off=0 hex=68656c6c6f total=5 done=yes\r\n");
  assert.equal(chunk.offset, 0);
  assert.equal(chunk.total, 5);
  assert.equal(chunk.done, true);
  assert.equal(chunk.chunk, "hello");
  assert.equal(parseAiconfChunk("OK aiconf off=3 hex=6c6f total=5 done=yes").chunk, "lo");
  assert.equal(parseAiconfChunk("ERR aiconf=empty"), null);
});

test("upload chunks split on UTF-8 bytes and reassemble", () => {
  const text = serializeAiconf({ ...config, model: "模型-ß" }, { includeSecrets: false });
  const chunks = aiconfHexChunks(text, 7);
  assert.ok(chunks.length > 1);
  const rebuilt = chunks.map((hex, index) => ({
    offset: chunks.slice(0, index).reduce((sum, part) => sum + part.length / 2, 0),
    hex,
    total: new TextEncoder().encode(text).length,
    done: false,
  }));
  assert.equal(assembleAiconf(rebuilt, rebuilt.at(-1).total), text);

  const single = aiconfHexChunks("{}");
  assert.deepEqual(single, ["7b7d"]);
  assert.equal(assembleAiconf([{ offset: 0, hex: "7b7d", chunk: "{}" }], 2), "{}");
  assert.throws(() => assembleAiconf([], 2), /incomplete/);
  assert.throws(() => assembleAiconf([
    { offset: 1, hex: "61", chunk: "a" },
  ], 1), /out of order/);
  assert.ok(AICONF_MAX_BYTES >= 240);
});
