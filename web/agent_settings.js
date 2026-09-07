import { available } from "./agent_runtime.js";
import { loadAgentConfig, saveAgentConfig, clearAgentConfig, validateAgentConfig } from "./agent_config.js";

const labels = {
  title: ["AI 配置", "AI configuration"],
  endpoint: ["API 地址", "API base URL"],
  endpointHint: ["兼容 Chat Completions，例如 https://api.example.com/v1", "Chat Completions compatible, e.g. https://api.example.com/v1"],
  model: ["模型名称", "Model ID"], key: ["API Key", "API key"],
  keyHint: ["无需密钥的本地服务可留空。", "Leave blank for a local service that does not require a key."],
  save: ["保存配置", "Save configuration"], clear: ["清除配置", "Clear configuration"],
  storage: ["配置和 API Key 保存在当前设备的应用 / 浏览器存储中，刷新或重启后保留。清除配置或应用 / 网站数据后删除。", "Configuration and API key are saved in this device's app / browser storage across reloads and restarts. Clear the configuration or app / site data to remove them."],
  notice: ["提问后，助手会把所需串口日志发送到你配置的模型服务。", "When you ask a question, the assistant sends the requested serial logs to your configured model service."],
  saved: ["配置已保存到当前设备。", "Configuration saved on this device."],
  cleared: ["已清除当前设备上的 AI 配置。", "AI configuration cleared from this device."],
  dirty: ["修改尚未保存。", "Changes have not been saved."],
  readError: ["无法读取本地配置，请重新填写并保存。", "Could not read local configuration. Enter it again and save."],
  saveError: ["保存失败，请检查应用 / 浏览器是否允许本地存储后重试。", "Save failed. Check that app / browser storage is allowed, then retry."],
  clearError: ["清除失败，本地配置仍保留，请重试。", "Clear failed. The saved configuration is still present; retry."],
  endpointError: ["请输入有效的 HTTP(S) API 地址，不要包含账号密码、查询参数或片段。", "Enter an HTTP(S) API base URL without credentials, query parameters or a fragment."],
  modelError: ["请填写模型名称。", "Enter a model ID."],
  busy: ["Agent 正在运行，请先返回对话并停止，再修改配置。", "Agent is running. Return to the conversation and stop it before editing configuration."],
};

export function createAgentSettings({ section, tab, getLang, onChange }) {
  if (!available) return null;
  section.hidden = tab.hidden = false;
  section.closest(".controls").classList.add("has-agent-settings");
  const $ = (id) => section.querySelector(`#${id}`);
  const inputs = { endpoint: $("agentEndpoint"), model: $("agentModel"), apiKey: $("agentApiKey") };
  const text = (key) => labels[key]?.[getLang().startsWith("zh") ? 0 : 1] || key;
  let config = null;
  let status = "";
  let error = false;
  let busy = false;
  function renderStatus() {
    $("agentSettingsStatus").textContent = text(busy ? "busy" : status);
    $("agentSettingsStatus").classList.toggle("field-error", !busy && error);
  }
  function showStatus(next, failed = false) { status = next; error = failed; renderStatus(); }
  function fill() {
    for (const [name, input] of Object.entries(inputs)) input.value = config?.[name] || "";
  }
  function resetErrors() {
    for (const input of Object.values(inputs)) input.removeAttribute("aria-invalid");
  }
  try { config = loadAgentConfig(localStorage); } catch { showStatus("readError", true); }
  fill();
  $("agentSettingsForm").addEventListener("submit", (event) => {
    event.preventDefault();
    if (busy) return;
    resetErrors();
    const draft = Object.fromEntries(Object.entries(inputs).map(([name, input]) => [name, input.value]));
    try { validateAgentConfig(draft); } catch (error) {
      const name = error.message === "model" ? "model" : "endpoint";
      inputs[name].setAttribute("aria-invalid", "true");
      showStatus(`${name}Error`, true);
      inputs[name].focus();
      return;
    }
    try { config = saveAgentConfig(localStorage, draft); } catch {
      showStatus("saveError", true);
      return;
    }
    fill();
    showStatus("saved");
    onChange?.();
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
  function refreshLang() {
    for (const el of section.querySelectorAll("[data-ai-setting]")) el.textContent = text(el.dataset.aiSetting);
    renderStatus();
  }
  refreshLang();
  return {
    refreshLang,
    getConfig: () => config && { ...config },
    setBusy(value) { busy = value; $("agentSettingsFields").disabled = value; renderStatus(); },
  };
}
