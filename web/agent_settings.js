import { TARGET_DIRECTORY_SETUP } from './target_binding.js';
import { available } from "./agent_runtime.js";
import {
  clearAgentConfig,
  endpointSecurity,
  formatAgentHeaders,
  loadAgentConfig,
  parseAgentHeaders,
  saveAgentConfig,
  validateAgentConfig,
} from "./agent_config.js";
import { loadPricing, savePricing } from "./agent_usage.js";

const labels = {
  bindingUuid:["Bee 保存的目标机 UUID", "Bound target UUID"], observedUuid:["本次读取的目标机 UUID", "Observed target UUID"],
  bindingTitle:["目标机绑定", "Target identity binding"],
  bindingHint:["优先使用已有的系统 ID，其次是用户 ID。首次绑定优先写入 /var/lib/linkr/device-id，需要权限时尝试 sudo；请在串口输入密码。sudo 不可用或失败时改用 ~/.local/share/linkr/device-id（随登录用户变化）。输入密码后请再次点击绑定，核实后才保存到 Bee Flash。", "Existing system IDs take priority, then existing user IDs. New IDs prefer /var/lib/linkr/device-id using sudo when needed; enter the password in the terminal. If sudo is unavailable or fails, use ~/.local/share/linkr/device-id (per user). After entering a password, click Bind again to verify before saving to Bee Flash."],
  bindingPath:["目标机保存路径", "Target save path"],
  bindingSudo:["请在串口终端输入目标机 sudo 密码（不会显示字符）。完成并返回 Shell 提示符后，再点击“绑定当前目标机”核实并保存。若不使用 sudo，可在密码提示处按 Ctrl+D，等待命令回退到用户目录。", "Enter the target sudo password directly in the terminal (characters stay hidden). Once the shell prompt returns, click Bind current target again to verify and save. To decline sudo, press Ctrl+D at its password prompt and wait for the user-directory fallback."],
  bindingVerify:["核实绑定", "Verify binding"], bindingBind:["绑定当前目标机", "Bind current target"],
  bindingClear:["解除 Bee 绑定", "Unbind Bee"], bindingRegenerate:["重新生成并绑定", "Regenerate and bind"],
  bindingRegenerateHint:["允许替换目标机现有 ID；旧档案保留，但不再自动关联。", "Allow replacing the target ID. Previous records remain but will no longer match automatically."],
  bindingWorking:["正在核实目标机与 Bee；若串口提示 sudo 密码，请在终端输入，其他时候请勿输入命令或切换连接…", "Checking target and Bee. Enter sudo credentials in the terminal if prompted; otherwise do not send commands or switch connections…"],
  bindingVerified:["目标机 ID 已核实，与 Bee Flash 绑定一致。", "Target UUID verified against Bee Flash binding."],
  bindingCleared:["已清除 Bee 绑定；目标机 ID 文件和历史档案保留。", "Bee binding cleared; target ID file and historical records retained."],
  bindingMismatch:["当前目标机与 Bee 保存的绑定不一致。核对目标机后可点击“绑定当前目标机”。", "Target does not match the stored Bee binding. Check the target before binding it."],
  bindingPermission:["目标机当前用户无权写入 /var/lib/linkr。首次绑定可先在串口执行下面的命令，按提示输入目标机 sudo 密码，再点击“绑定当前目标机”：", "The target user cannot write /var/lib/linkr. For initial setup, run the following in the terminal, enter the target sudo password if prompted, then click Bind current target:"],
  bindingError:["操作未完成：", "Operation incomplete: "],
  bindingUnknown:["本次连接尚未核实绑定", "Binding not verified in this connection"],
  title: ["AI 配置", "AI configuration"],
  endpoint: ["API 地址", "API base URL"],
  endpointHint: ["兼容 Chat Completions，例如 https://api.example.com/v1", "Chat Completions compatible, e.g. https://api.example.com/v1"],
  model: ["模型名称", "Model ID"], key: ["API Key", "API key"],
  keyHint: ["无需密钥的本地服务可留空。", "Leave blank for a local service that does not require a key."],
  headers: ["附加请求头", "Extra request headers"],
  headersHint: ["每行一个 `名称: 值`。部分端点需要特定请求头才接受浏览器请求，例如 Anthropic 需要 anthropic-dangerous-direct-browser-access: true。", "One `Name: value` per line. Some endpoints only accept a browser request with a specific header, for example Anthropic needs anthropic-dangerous-direct-browser-access: true."],
  pricing: ["Token 单价（每 100 万，可选）", "Token prices per 1M (optional)"],
  pricingHint: ["填了就按它估算本轮费用，只用于显示，留空则只显示 token 数。", "When set, the conversation shows an estimated cost; leave blank to show token counts only."],
  pricingError: ["单价必须是 0 到 100000 之间的数字。", "Prices must be numbers between 0 and 100000."],
  headersError: ["请求头格式无效：每行一个 `名称: 值`，名称只能包含字母、数字和连字符。", "Invalid headers: use one `Name: value` per line, with names limited to letters, digits and hyphens."],
  save: ["保存配置", "Save configuration"], clear: ["清除配置", "Clear configuration"],
  storage: ["配置和 API Key 保存在当前设备的应用 / 浏览器存储中，刷新或重启后保留。清除配置或应用 / 网站数据后删除。", "Configuration and API key are saved in this device's app / browser storage across reloads and restarts. Clear the configuration or app / site data to remove them."],
  notice: ["提问后，助手会把所需串口日志发送到你配置的模型服务。", "When you ask a question, the assistant sends the requested serial logs to your configured model service."],
  saved: ["配置已保存到当前设备。", "Configuration saved on this device."],
  cleared: ["已清除当前设备上的 AI 配置。", "AI configuration cleared from this device."],
  dirty: ["修改尚未保存。", "Changes have not been saved."],
  plaintextKeyWarning: ["警告：该地址使用明文 http 且非本机回环，API Key 与串口日志会在网络中明文传输；请改用 https，或把服务放在 localhost。", "Warning: this endpoint uses plain http and is not loopback, so the API key and serial logs travel the network in cleartext. Use https, or host the service on localhost."],
  plaintextKeyConfirm: ["该地址使用明文 http 且非本机回环，API Key 会以明文发送。仍要保存吗？", "This endpoint uses plain http and is not loopback, so the API key will be sent in cleartext. Save anyway?"],
  plaintextKeyBlocked: ["已取消保存：明文 http 端点需要确认后才会保存 API Key。", "Save cancelled: a plaintext http endpoint needs confirmation before an API key is stored."],
  readError: ["无法读取本地配置，请重新填写并保存。", "Could not read local configuration. Enter it again and save."],
  saveError: ["保存失败，请检查应用 / 浏览器是否允许本地存储后重试。", "Save failed. Check that app / browser storage is allowed, then retry."],
  clearError: ["清除失败，本地配置仍保留，请重试。", "Clear failed. The saved configuration is still present; retry."],
  endpointError: ["请输入有效的 HTTP(S) API 地址，不要包含账号密码、查询参数或片段。", "Enter an HTTP(S) API base URL without credentials, query parameters or a fragment."],
  modelError: ["请填写模型名称。", "Enter a model ID."],
  busy: ["Agent 正在运行，请先返回对话并停止，再修改配置。", "Agent is running. Return to the conversation and stop it before editing configuration."],
};

export function createAgentSettings({ section, tab, getLang, onChange, bindingAction }) {
  if (!available) return null;
  section.hidden = tab.hidden = false;
  section.closest(".controls").classList.add("has-agent-settings");
  const $ = (id) => section.querySelector(`#${id}`);
  const inputs = { endpoint: $("agentEndpoint"), model: $("agentModel"), apiKey: $("agentApiKey") };
  const headersInput = $("agentHeaders");
  const priceInputs = { input: $("agentPriceInput"), output: $("agentPriceOutput") };
  const text = (key) => labels[key]?.[getLang().startsWith("zh") ? 0 : 1] || key;
  let config = null;
  let status = "";
  let error = false;
  let busy = false;
  function renderStatus() {
    // The plaintext warning describes the saved configuration, so it stays
    // visible next to whatever status the last action produced.
    const exposed = endpointSecurity(config?.endpoint).exposesKey;
    const el = $("agentSettingsStatus");
    el.textContent = [
      busy ? text("busy") : status ? text(status) : "",
      exposed ? text("plaintextKeyWarning") : "",
    ].filter(Boolean).join(" ");
    el.classList.toggle("field-error", (!busy && error) || exposed);
  }
  function showStatus(next, failed = false) { status = next; error = failed; renderStatus(); }
  function fill() {
    for (const [name, input] of Object.entries(inputs)) input.value = config?.[name] || "";
    headersInput.value = formatAgentHeaders(config?.headers);
    const pricing = loadPricing(localStorage);
    for (const [name, input] of Object.entries(priceInputs)) input.value = pricing[name] ? String(pricing[name]) : "";
  }
  function resetErrors() {
    for (const input of Object.values(inputs)) input.removeAttribute("aria-invalid");
    headersInput.removeAttribute("aria-invalid");
    for (const input of Object.values(priceInputs)) input.removeAttribute("aria-invalid");
  }
  try { config = loadAgentConfig(localStorage); } catch { showStatus("readError", true); }
  fill();
  $("agentSettingsForm").addEventListener("submit", (event) => {
    event.preventDefault();
    if (busy) return;
    resetErrors();
    const draft = Object.fromEntries(Object.entries(inputs).map(([name, input]) => [name, input.value]));
    try { draft.headers = parseAgentHeaders(headersInput.value); } catch {
      headersInput.setAttribute("aria-invalid", "true");
      showStatus("headersError", true);
      headersInput.focus();
      return;
    }
    try { validateAgentConfig(draft); } catch (error) {
      if (error.message === "headers") {
        headersInput.setAttribute("aria-invalid", "true");
        showStatus("headersError", true);
        headersInput.focus();
        return;
      }
      const name = error.message === "model" ? "model" : "endpoint";
      inputs[name].setAttribute("aria-invalid", "true");
      showStatus(`${name}Error`, true);
      inputs[name].focus();
      return;
    }
    const previous = config;
    // A credential must not reach a cleartext remote endpoint by accident.
    const security = endpointSecurity(draft.endpoint);
    if (security.exposesKey && draft.apiKey && !window.confirm(text("plaintextKeyConfirm"))) {
      inputs.endpoint.setAttribute("aria-invalid", "true");
      showStatus("plaintextKeyBlocked", true);
      inputs.endpoint.focus();
      return;
    }
    try { config = saveAgentConfig(localStorage, draft); } catch {
      showStatus("saveError", true);
      return;
    }
    /* Prices are display-only, so they are stored apart from the model
     * configuration: editing them must not restart the conversation. */
    try { savePricing(localStorage, { input: priceInputs.input.value, output: priceInputs.output.value }); }
    catch {
      priceInputs.input.setAttribute("aria-invalid", "true");
      showStatus("pricingError", true);
      priceInputs.input.focus();
      return;
    }
    fill();
    showStatus("saved");
    // Re-saving the same normalized settings must not silently reset the visible
    // diagnostic conversation. A changed provider, model or key starts fresh.
    if (!previous || Object.keys(inputs).some((name) => previous[name] !== config[name])) onChange?.();
  });
  $("agentSettingsClear").addEventListener("click", () => {
    if (busy) return;
    try { clearAgentConfig(localStorage); } catch { showStatus("clearError", true); return; }
    config = null;
    fill();
    resetErrors();
    showStatus("cleared");
    onChange?.();
  });
  for (const input of Object.values(inputs)) input.addEventListener("input", () => {
    resetErrors();
    showStatus("dirty");
  });
  for (const [id, action] of [["targetVerify","verify"],["targetBind","bind"],["targetUnbind","clear"],["targetRegenerate","regenerate"]]) {
    $(id).addEventListener("click", async () => {
      if (busy || (action === "regenerate" && !$("targetRegenerateConfirm").checked)) return;
      $("targetBindingStatus").textContent = text("bindingWorking");
      $("targetBindingIdentity").textContent = "";
      try {
        const result = await bindingAction(action);
        $("targetBindingIdentity").textContent = [result.targetId && `${text("bindingUuid")}: ${result.targetId}`, result.observedId && `${text("observedUuid")}: ${result.observedId}`, result.targetPath && `${text("bindingPath")}: ${result.targetPath}`].filter(Boolean).join("\n");
        $("targetBindingStatus").textContent = text(result.status === "cleared" ? "bindingCleared" : result.status === "mismatch" ? "bindingMismatch" : "bindingVerified");
      } catch(error) { $("targetBindingStatus").textContent = error.code === "TARGET_SUDO_INPUT" ? text("bindingSudo") : error.code === "TARGET_PERMISSION_DENIED" ? text("bindingPermission") + "\n" + TARGET_DIRECTORY_SETUP : text("bindingError") + error.message; }
      // The regenerate confirmation is single-use and is dropped after every
      // target action, not just regenerate: an armed "replace the target ID"
      // state must never survive into a later, unrelated click.
      finally { $("targetRegenerateConfirm").checked = false; $("targetRegenerate").disabled = true; }
    });
  }
  $("targetRegenerateConfirm").addEventListener("change", () => { $("targetRegenerate").disabled = !$("targetRegenerateConfirm").checked; });
  function refreshLang() {
    for (const el of section.querySelectorAll("[data-ai-setting]")) el.textContent = text(el.dataset.aiSetting);
    renderStatus();
  }
  refreshLang();
  return {
    refreshLang,
    connectionChanged() { $("targetBindingIdentity").textContent = ""; $("targetBindingStatus").textContent = text("bindingUnknown"); },
    getConfig: () => config && { ...config },
    setBusy(value) { busy = value; $("agentSettingsFields").disabled = value; $("targetBindingFields").disabled = value; renderStatus(); },
  };
}
