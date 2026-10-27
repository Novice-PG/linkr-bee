import assert from 'node:assert/strict';
import test from 'node:test';
import { cliproxyAddress, cliproxyConfig, fetchCLIProxyKeys, fetchCLIProxyModels } from '../../web/cliproxy_import.js';
import { saveAgentConfig } from '../../web/agent_config.js';
const json = (data, status = 200) => new Response(JSON.stringify(data), { status });

test('normalizes service, model and management URLs while preserving reverse proxy prefixes', () => {
  for (const suffix of ['', '/', '/v1/', '/management.html', '/v8/management']) {
    assert.deepEqual(cliproxyAddress(`https://proxy.test/cpa${suffix}`), {
      base: 'https://proxy.test/cpa', endpoint: 'https://proxy.test/cpa/v1',
      management: 'https://proxy.test/cpa/v8/management/config/access/api-keys',
    });
  }
  for (const value of ['', 'file:///tmp/a', 'https://u:p@proxy.test', 'https://proxy.test?key=secret', 'https://proxy.test/#secret']) {
    assert.throws(() => cliproxyAddress(value), { code: 'address' });
  }
});

test('management key only authenticates the narrow config read; client key authenticates models and storage', async () => {
  const calls = [];
  const fetchImpl = async (url, options) => {
    calls.push({ url, ...options });
    return url.endsWith('/api-keys') ? json(['client-one', 'client-one']) : json({ data: [{ id: 'm2' }, { id: 'm1' }] });
  };
  const [key] = await fetchCLIProxyKeys('https://proxy.test', 'management-secret', { fetchImpl });
  const models = await fetchCLIProxyModels('https://proxy.test', key, { fetchImpl });
  assert.deepEqual(models, ['m1', 'm2']);
  assert.deepEqual(calls.map(c => [c.url, c.headers.Authorization]), [
    ['https://proxy.test/v8/management/config/access/api-keys', 'Bearer management-secret'],
    ['https://proxy.test/v1/models', 'Bearer client-one'],
  ]);
  for (const call of calls) {
    assert.equal(call.method, 'GET'); assert.equal(call.credentials, 'omit');
    assert.equal(call.redirect, 'error'); assert.equal(call.cache, 'no-store');
  }
  let stored;
  saveAgentConfig({ setItem(k, v) { stored = v; } }, cliproxyConfig('https://proxy.test', key, models[0]));
  assert.ok(!stored.includes('management-secret'));
  assert.deepEqual(JSON.parse(stored).headers, {});
  assert.equal(JSON.parse(stored).apiKey, 'client-one');
});

test('management auth failures do not retry or expose response bodies', async () => {
  for (const [status, code] of [[401, 'unauthorized'], [403, 'forbidden'], [404, 'unavailable'], [429, 'http'], [500, 'http']]) {
    let calls = 0;
    await assert.rejects(fetchCLIProxyKeys('https://proxy.test', 'management-secret', { fetchImpl: async () => {
      calls++; return json({ error: 'management-secret' }, status);
    } }), (error) => error.code === code && !error.message.includes('secret'));
    assert.equal(calls, 1);
  }
});

test('rejects malformed, empty and oversized responses, including streamed bodies', async () => {
  for (const [data, code] of [[[], 'emptyKeys'], [{ 'api-keys': ['a'] }, 'response'], [[''], 'response'], [['bad\r\nkey'], 'response'], [Array(513).fill('a'), 'response']]) {
    await assert.rejects(fetchCLIProxyKeys('https://proxy.test', 'secret', { fetchImpl: async () => json(data) }), { code });
  }
  for (const response of [new Response('<html>error</html>'), new Response(' '.repeat(1024 * 1024 + 1))]) {
    await assert.rejects(fetchCLIProxyKeys('https://proxy.test', 'secret', { fetchImpl: async () => response }), { code: 'response' });
  }
  await assert.rejects(fetchCLIProxyModels('https://proxy.test', 'client', { fetchImpl: async () => json({ data: [] }) }), { code: 'emptyModels' });
});

test('timeout, cancellation and network errors are bounded and sanitized', async () => {
  const hanging = async (_url, { signal }) => new Promise((resolve, reject) => {
    if (signal.aborted) reject(new Error('secret abort'));
    else signal.addEventListener('abort', () => reject(new Error('secret abort')), { once: true });
  });
  await assert.rejects(fetchCLIProxyKeys('https://proxy.test', 'secret', { fetchImpl: hanging, timeoutMs: 5 }), { code: 'timeout' });
  const controller = new AbortController();
  const pending = fetchCLIProxyKeys('https://proxy.test', 'secret', { fetchImpl: hanging, signal: controller.signal });
  controller.abort();
  await assert.rejects(pending, { code: 'cancelled' });
  await assert.rejects(fetchCLIProxyKeys('https://proxy.test', 'secret', { fetchImpl: async () => { throw new Error('secret'); } }), { message: 'network' });
});
