// Read-only CLIProxyAPI v8 import. Management credentials never become model
// configuration, and redirects must not forward them to another endpoint.
import { parseModels, validateAgentConfig } from './agent_config.js';

export const CLIPROXY_DEFAULT_URL = 'http://127.0.0.1:8317';
const MAX_RESPONSE_BYTES = 1024 * 1024;
const failure = (code, status) => Object.assign(new Error(code), { code, status });

export function cliproxyAddress(value) {
  let url;
  try { url = new URL(String(value).trim()); } catch { throw failure('address'); }
  if (!['https:', 'http:'].includes(url.protocol) || url.username || url.password || url.search || url.hash) {
    throw failure('address');
  }
  const base = url.href.replace(/\/+$/, '').replace(/\/(?:v1|v8\/management|management\.html)$/, '');
  return { base, endpoint: `${base}/v1`, management: `${base}/v8/management/config/access/api-keys` };
}

function validKey(value) {
  return typeof value === 'string' && value.trim().length > 0 && value.length <= 4096 &&
    !/[\x00-\x1f\x7f]/.test(value);
}

async function requestJson(url, key, { fetchImpl = globalThis.fetch, signal, timeoutMs = 15000 } = {}) {
  const controller = new AbortController();
  const abort = () => controller.abort();
  let timedOut = false;
  const timer = setTimeout(() => { timedOut = true; controller.abort(); }, timeoutMs);
  signal?.addEventListener('abort', abort, { once: true });
  if (signal?.aborted) abort();
  try {
    const response = await fetchImpl(url, {
      method: 'GET', headers: { Authorization: `Bearer ${key}`, Accept: 'application/json' },
      credentials: 'omit', redirect: 'error', cache: 'no-store', signal: controller.signal,
    });
    if (!response.ok) {
      // Never echo response bodies: upstream errors may contain credentials.
      throw failure(response.status === 401 ? 'unauthorized' : response.status === 403 ? 'forbidden' :
        response.status === 404 ? 'unavailable' : 'http', response.status);
    }
    if (Number(response.headers.get('content-length')) > MAX_RESPONSE_BYTES) {
      await response.body?.cancel();
      throw failure('response');
    }
    const reader = response.body?.getReader();
    if (!reader) throw failure('response');
    const decoder = new TextDecoder();
    let size = 0, body = '';
    try {
      while (true) {
        const { value, done } = await reader.read();
        if (done) break;
        size += value.byteLength;
        if (size > MAX_RESPONSE_BYTES) { await reader.cancel(); throw failure('response'); }
        body += decoder.decode(value, { stream: true });
      }
    } finally { reader.releaseLock(); }
    try { return JSON.parse(body + decoder.decode()); } catch { throw failure('response'); }
  } catch (error) {
    if (signal?.aborted) throw failure('cancelled');
    if (timedOut) throw failure('timeout');
    if (error?.code && ['unauthorized', 'forbidden', 'unavailable', 'http', 'response'].includes(error.code)) throw error;
    throw failure('network');
  } finally {
    clearTimeout(timer);
    signal?.removeEventListener('abort', abort);
  }
}

export async function fetchCLIProxyKeys(address, managementKey, options) {
  const { management } = cliproxyAddress(address);
  if (!validKey(managementKey)) throw failure('key');
  const data = await requestJson(management, managementKey, options);
  if (!Array.isArray(data) || data.length > 512 || !data.every(validKey)) throw failure('response');
  const keys = [...new Set(data)];
  if (!keys.length) throw failure('emptyKeys');
  return keys;
}

export async function fetchCLIProxyModels(address, clientKey, options) {
  const { endpoint } = cliproxyAddress(address);
  if (!validKey(clientKey)) throw failure('key');
  const data = await requestJson(`${endpoint}/models`, clientKey, options);
  try {
    const models = parseModels(data, 'openai-completions');
    if (!models.length) throw failure('emptyModels');
    return models;
  } catch (error) {
    throw failure(error?.code === 'models-empty' || error?.code === 'emptyModels' ? 'emptyModels' : 'response');
  }
}

export function cliproxyConfig(address, clientKey, model) {
  // Explicit fields discard old provider headers and never include management keys.
  return validateAgentConfig({ endpoint: cliproxyAddress(address).endpoint,
    apiKey: clientKey, model, provider: 'openai-completions' });
}
