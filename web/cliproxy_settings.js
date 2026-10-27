import { endpointSecurity } from './agent_config.js';
import { CLIPROXY_DEFAULT_URL, cliproxyAddress, cliproxyConfig, fetchCLIProxyKeys, fetchCLIProxyModels } from './cliproxy_import.js';

const labels = {
  title: ['从 CLIProxyAPI 一键导入', 'Import from CLIProxyAPI'],
  hint: ['填写服务地址和管理密钥，自动读取客户端 Key 与模型。只有一个选项时直接保存，否则选择后保存。会替换当前 AI 配置。', 'Enter the server URL and management key to read client keys and models. A single choice is saved automatically; otherwise choose and save. This replaces the current AI configuration.'],
  address: ['CLIProxyAPI 服务地址', 'CLIProxyAPI server URL'],
  addressHint: ['手机连接电脑上的服务时，请填写电脑的局域网地址。', 'On a phone, use the computer’s LAN address to reach its service.'],
  management: ['临时管理密钥', 'Temporary management key'],
  secretHint: ['仅用于本次读取，读取后清空，不保存到 Bee 配置。不是上游账号的 OAuth token。', 'Used only for this read, then cleared. Never saved in Bee configuration. This is not an upstream account’s OAuth token.'],
  connect: ['连接并导入', 'Connect and import'],
  cancel: ['取消导入', 'Cancel import'],
  client: ['选择客户端 API Key', 'Choose a client API key'],
  model: ['选择模型', 'Choose a model'],
  choose: ['请选择…', 'Choose…'],
  save: ['保存导入配置', 'Save imported configuration'],
  readingKeys: ['正在读取客户端 Key…', 'Reading client keys…'],
  readingModels: ['正在用客户端 Key 验证连接并读取模型…', 'Checking the client key and reading models…'],
  chooseKey: ['读取成功，请选择要用于日常调用的 Key。', 'Keys loaded. Choose one for daily model calls.'],
  chooseModel: ['模型列表已读取，请选择模型并保存。尚未验证实际对话。', 'Model list loaded. Choose a model and save. Chat has not been tested yet.'],
  saved: ['已导入并保存。客户端 Key 已通过模型列表验证，尚未验证实际对话。', 'Imported and saved. The client key passed model-list verification; chat has not been tested yet.'],
  cancelled: ['已取消导入，原配置保持不变。', 'Import cancelled. Previous configuration kept.'],
  addressError: ['请输入有效的 HTTP(S) 服务地址，不要包含账号密码、查询参数或片段。', 'Enter an HTTP(S) server URL without credentials, query parameters or a fragment.'],
  keyError: ['请输入有效的管理密钥。', 'Enter a valid management key.'],
  unauthorized: ['密钥被拒绝。请核对密钥后重试，避免连续失败导致服务端临时封禁。', 'Key rejected. Check it before retrying to avoid a temporary server lockout.'],
  forbidden: ['访问被拒绝。请核对密钥及远程管理访问设置。', 'Access denied. Check the key and remote management access settings.'],
  unavailable: ['接口不可用。请确认已启用 v8 管理 API；也可取消后手动填写客户端 Key。', 'Endpoint unavailable. Check that the v8 management API is enabled, or cancel and enter a client key manually.'],
  http: ['服务返回 HTTP {status}，原配置保持不变。', 'Server returned HTTP {status}. Previous configuration kept.'],
  network: ['无法连接。请检查地址、网络、HTTPS 和跨域设置，然后重试。', 'Cannot connect. Check the URL, network, HTTPS and CORS settings, then retry.'],
  timeout: ['请求超时，请检查服务是否可达后重试。', 'Request timed out. Check that the server is reachable, then retry.'],
  response: ['服务响应格式不正确或过大，未导入任何配置。', 'The response is invalid or too large. No configuration was imported.'],
  emptyKeys: ['服务尚未配置客户端 API Key，请先在 CLIProxyAPI 中创建。', 'No client API keys configured. Create one in CLIProxyAPI first.'],
  emptyModels: ['该客户端 Key 没有可用模型，请检查 CLIProxyAPI 上游账号和模型配置。', 'No models available for this client key. Check the upstream accounts and model configuration in CLIProxyAPI.'],
  storage: ['保存失败，请检查本地存储权限后重试。原配置未更改。', 'Save failed. Check local storage permissions and retry. Previous configuration kept.'],
  insecure: ['该服务使用非本机的明文 HTTP，管理密钥将通过网络明文发送。仍要连接并导入吗？', 'This remote service uses unencrypted HTTP. The management key will travel over the network in plaintext. Connect and import anyway?'],
};

export function createCLIProxySettings({ section, getLang, onImport, onPending }) {
  const $ = (id) => section.querySelector(`#cliproxy${id}`);
  const root = $('Import'), address = $('Address'), secret = $('Secret');
  const client = $('Client'), model = $('Model');
  let keys = [], models = [], source = '', controller = null, generation = 0, busy = false;
  let status = '', failed = false, replacements = {};
  const text = (key) => labels[key]?.[getLang().startsWith('zh') ? 0 : 1] || key;
  function message(key, error = false, values = {}) {
    status = key; failed = error; replacements = values;
    let result = text(key);
    for (const [name, value] of Object.entries(values)) result = result.replace(`{${name}}`, String(value));
    $('Status').textContent = result;
    $('Status').classList.toggle('field-error', error);
  }
  function options(select, entries) {
    select.replaceChildren();
    const placeholder = new Option(text('choose'), '');
    select.add(placeholder);
    entries.forEach((label, index) => select.add(new Option(label, String(index))));
  }
  function controls() {
    const loading = Boolean(controller);
    $('Fields').disabled = busy;
    $('Connect').disabled = loading;
    address.disabled = secret.disabled = loading;
    client.disabled = model.disabled = loading;
    $('Save').disabled = loading || !model.value || !client.value;
    $('Cancel').hidden = !loading && !keys.length;
    root.setAttribute('aria-busy', String(loading));
    onPending(loading || keys.length > 0);
  }
  function reset() {
    generation++;
    controller?.abort(); controller = null;
    keys = []; models = []; source = ''; secret.value = '';
    client.replaceChildren(); model.replaceChildren();
    $('Choices').hidden = true;
    $('ClientField').hidden = $('ModelField').hidden = true;
    controls();
  }
  function report(error) {
    const code = error?.code;
    message(code === 'address' ? 'addressError' : code === 'key' ? 'keyError' :
      labels[code] ? code : 'response', true, { status: error?.status ?? '?' });
  }
  function save() {
    if (busy || controller || !client.value || !model.value) return;
    try {
      onImport(cliproxyConfig(source, keys[Number(client.value)], models[Number(model.value)]));
    } catch { message('storage', true); return; }
    reset(); message('saved');
  }
  async function readModels() {
    if (!client.value) return;
    const epoch = ++generation;
    controller?.abort(); controller = new AbortController();
    models = []; model.replaceChildren(); $('ModelField').hidden = true;
    message('readingModels'); controls();
    try {
      const result = await fetchCLIProxyModels(source, keys[Number(client.value)], { signal: controller.signal });
      if (epoch !== generation || busy) return;
      models = result;
      options(model, models);
      $('ModelField').hidden = false;
      $('Choices').hidden = false;
      if (models.length === 1) model.value = '0';
      message('chooseModel');
      controller = null; controls();
      if (models.length === 1 && keys.length === 1) save();
      else model.focus();
    } catch (error) {
      if (epoch !== generation) return;
      reset(); report(error);
    } finally {
      if (epoch === generation) { controller = null; controls(); }
    }
  }
  async function connect() {
    if (busy || controller) return;
    let target;
    try { target = cliproxyAddress(address.value); }
    catch (error) { report(error); address.focus(); return; }
    if (!secret.value.trim()) { message('keyError', true); secret.focus(); return; }
    if (endpointSecurity(target.base).exposesKey && !window.confirm(text('insecure'))) return;
    const managementKey = secret.value;
    reset(); source = target.base;
    const epoch = generation;
    controller = new AbortController();
    message('readingKeys'); controls();
    try {
      const result = await fetchCLIProxyKeys(source, managementKey, { signal: controller.signal });
      if (epoch !== generation || busy) return;
      keys = result;
      // Option values are indexes, never credentials. Only a short suffix is shown.
      options(client, keys.map((key, index) => `Key ${index + 1} · ••••${key.length > 8 ? key.slice(-4) : ''}`));
      $('Choices').hidden = false;
      $('ClientField').hidden = keys.length === 1;
      controller = null; controls();
      if (keys.length === 1) { client.value = '0'; await readModels(); }
      else { message('chooseKey'); client.focus(); }
    } catch (error) {
      if (epoch !== generation) return;
      reset(); report(error);
    } finally {
      if (epoch === generation) { controller = null; controls(); }
    }
  }
  address.value = CLIPROXY_DEFAULT_URL;
  $('Connect').addEventListener('click', connect);
  $('Cancel').addEventListener('click', () => { reset(); message('cancelled'); });
  $('Save').addEventListener('click', save);
  client.addEventListener('change', () => {
    models = []; model.replaceChildren(); $('ModelField').hidden = true;
    if (client.value) readModels(); else controls();
  });
  model.addEventListener('change', controls);
  address.addEventListener('input', () => { reset(); message(''); });
  secret.addEventListener('input', () => {
    // Editing the temporary secret invalidates a previous choice, but keep the new input.
    const value = secret.value; reset(); secret.value = value; message('');
  });
  root.addEventListener('keydown', (event) => {
    if (event.key === 'Enter' && !event.isComposing && [address, secret].includes(event.target)) {
      event.preventDefault(); connect();
    }
  });
  root.addEventListener('toggle', () => { if (!root.open) { reset(); message(''); } });
  return {
    refreshLang() {
      for (const element of root.querySelectorAll('[data-cliproxy]')) element.textContent = text(element.dataset.cliproxy);
      for (const select of [client, model]) if (select.options.length) select.options[0].textContent = text('choose');
      message(status, failed, replacements);
    },
    setBusy(value) { busy = value; if (busy) { reset(); message(''); } controls(); },
  };
}
