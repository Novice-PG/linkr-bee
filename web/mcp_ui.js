/* MCP server settings card + on-phone approval banner.
 *
 * Only the Android build has a native MCP listener (window.LinkrMcp), so this
 * module is inert everywhere else: createMcpUi() is called with a host only
 * when the native bridge exists, and the card stays out of the DOM otherwise.
 *
 * The card is the only place that knows how the server is exposed to the user:
 * endpoint, bearer token, access policy and the audit of external calls. The
 * approval banner is deliberately outside the settings panel — an external AI
 * can ask for a write while the user is looking at the terminal, and a request
 * that nobody sees would otherwise block until it times out.
 */
import { claudeConfigSnippet, redactToolArguments } from "./mcp_server.js";

const labels = {
  title: ["MCP 服务器", "MCP server"],
  enable: ["允许局域网内的 AI 客户端连接", "Allow AI clients on this network"],
  enableHint: ["Claude Desktop、Cursor、Codex 等 MCP 客户端可调用本机的串口与配件管理能力。需要保持 App 运行并连接 Linkr Bee；App 被系统杀掉后服务停止。", "MCP clients such as Claude Desktop, Cursor or Codex can use this device's serial console and accessory management. Keep the app running and Linkr Bee connected; the service stops if Android kills the app."],
  warning: ["拿到访问令牌的人都能向串口发送输入。请只在可信网络启用，并定期更换令牌；不要把这个地址暴露到公网。", "Anyone holding the access token can send serial input. Enable this only on a trusted network, rotate the token, and never expose the address to the internet."],
  access: ["访问权限", "Access"],
  accessHint: ["只读模式拒绝所有修改设备的工具；WiFi 扫描不保存配置，但仍需本机确认。读写模式下每次写入同样需要确认。「无需确认」会跳过手机确认，直接执行所有工具。", "Read-only refuses every device-changing tool; a WiFi scan stores nothing and still needs on-device approval. In read-write mode every write also needs approval. No-confirmation mode skips the phone prompt and runs every tool directly."],
  accessConfirm: ["读写：写入需在本机确认", "Read and write: writes need on-device approval"],
  accessAuto: ["读写：无需本机确认（危险）", "Read and write: no on-device approval (unsafe)"],
  accessRead: ["只读：不改设备（扫描仍需确认）", "Read-only: no changes (scans still need approval)"],
  accessAutoWarning: ["写入确认已关闭：任何拿到访问令牌的客户端都可以直接向串口发送输入、修改桥接设置，手机不会再弹出确认。请只在完全可信的网络与客户端上使用；更安全的选项是「读写：写入需在本机确认」。", "Write approval is off: any client holding the token can send serial input and change bridge settings with no prompt on the phone. Use only with fully trusted networks and clients; the approval mode is the safer choice."],
  accessAutoConfirm: ["关闭写入确认后，拿到访问令牌的客户端可以在没有手机确认的情况下执行串口写入和配置修改。确定启用「无需确认」？", "With approval off, any client holding the token can send serial input and change settings without a confirmation on the phone. Enable the no-confirmation mode?"],
  port: ["端口", "Port"],
  portHint: ["1024–65535。修改端口会重启服务。", "1024–65535. Changing the port restarts the service."],
  bind: ["监听地址", "Listen on"],
  bindLan: ["所有网络接口（同网段可访问）", "All interfaces (reachable on the LAN)"],
  bindLoopback: ["仅本机回环", "Loopback only"],
  endpoint: ["MCP 地址", "MCP endpoint"],
  token: ["访问令牌", "Access token"],
  tokenHint: ["令牌只保存在这台手机上，不会上传。复制到 AI 客户端的配置里；重新生成后旧令牌立即失效。", "The token stays on this phone and is never uploaded. Copy it into the AI client configuration; regenerating invalidates the old token immediately."],
  showToken: ["显示令牌", "Show token"],
  hideToken: ["隐藏令牌", "Hide token"],
  copy: ["复制", "Copy"],
  copied: ["已复制。", "Copied."],
  copyFailed: ["复制失败，请手动选择文本。", "Copy failed; select the text manually."],
  regenerate: ["重新生成令牌", "Regenerate token"],
  regenerateConfirm: ["重新生成后旧令牌立即失效，已连接的客户端需要更新配置。继续？", "The old token stops working immediately and connected clients need the new one. Continue?"],
  regenerated: ["已生成新令牌，旧令牌已失效。", "New token generated; the old one is invalid."],
  clientConfig: ["在 AI 客户端中接入", "Connect an AI client"],
  clientConfigHint: ["把下面的配置加入 Claude Desktop 的 claude_desktop_config.json（或 Cursor 的 MCP 设置）。任何支持 Streamable HTTP 传输的 MCP 客户端都可以用这个地址和令牌。", "Add this to Claude Desktop's claude_desktop_config.json (or Cursor's MCP settings). Any MCP client that supports the Streamable HTTP transport can use this URL and token."],
  audit: ["最近调用", "Recent calls"],
  auditEmpty: ["还没有外部调用。", "No external calls yet."],
  auditHint: ["只保留最近 50 次调用，参数中的口令与令牌已打码。", "Only the latest 50 calls are kept; passwords and tokens in arguments are masked."],
  running: ["运行中", "Running"],
  idle: ["已启用但未运行：通知栏“停止”后请关闭再启用，或重启 App。", "Enabled but not running: after the notification's Stop action, toggle it off and on again, or restart the app."],
  stopped: ["未启用", "Disabled"],
  stopping: ["正在停止…", "Stopping…"],
  foreground: ["App 在前台", "App in the foreground"],
  background: ["App 在后台：写入类操作需要在手机上确认，可能失败。", "App in the background: writes need on-device approval and may fail."],
  notConnected: ["当前未连接 Linkr Bee；串口与配件工具会返回明确错误。", "Linkr Bee is not connected; serial and accessory tools will return an explicit error."],
  errorPrefix: ["服务错误：", "Service error: "],
  portError: ["端口必须是 1024 到 65535 之间的整数。", "Port must be an integer between 1024 and 65535."],
  approvalTitle: ["AI 请求执行操作", "An AI client requests approval"],
  approvalArgs: ["参数", "Arguments"],
  approvalHint: ["该操作会通过当前连接执行。核对参数后允许或拒绝；拒绝的原因会返回给 AI。", "This runs over the current connection. Check the arguments before allowing; a denial is reported back to the AI."],
  approvalAllow: ["允许执行", "Allow"],
  approvalDeny: ["拒绝", "Deny"],
  approvalTimeout: ["等待超时，操作未执行。", "Timed out; nothing was executed."],
  approvalDenied: ["已拒绝，操作未执行。", "Denied; nothing was executed."],
  approvalBackground: ["App 不在前台，写入操作需要在手机上确认，因此未执行。", "The app is not in the foreground, so the write was not executed."],
  approvalBusy: ["已有另一个等待确认的请求。", "Another approval is already pending."],
  approvalAccess: ["当前为只读模式。", "The server is in read-only mode."],
  tool_get_device_status: ["读取设备状态", "Read device status"],
  tool_read_serial_log: ["读取串口日志", "Read serial log"],
  tool_search_serial_log: ["查找串口日志", "Search serial log"],
  tool_get_accessory_diagnostics: ["读取配件诊断", "Read accessory diagnostics"],
  tool_wifi_scan: ["扫描附近 WiFi", "Scan nearby WiFi"],
  tool_send_serial_input: ["发送串口输入", "Send serial input"],
  tool_set_uart_config: ["修改桥接串口参数", "Change bridge UART settings"],
  tool_set_wifi: ["配置配件 WiFi", "Configure accessory WiFi"],
  tool_set_webdav: ["配置日志上传", "Configure log upload"],
};

const TOOL_LABEL_KEYS = {
  get_device_status: "tool_get_device_status",
  read_serial_log: "tool_read_serial_log",
  search_serial_log: "tool_search_serial_log",
  get_accessory_diagnostics: "tool_get_accessory_diagnostics",
  wifi_scan: "tool_wifi_scan",
  send_serial_input: "tool_send_serial_input",
  set_uart_config: "tool_set_uart_config",
  set_wifi: "tool_set_wifi",
  set_webdav: "tool_set_webdav",
};

const APPROVAL_TIMEOUT_MS = 90000;
const REFRESH_INTERVAL_MS = 3000;

export function createMcpUi({ mount, getLang, host }) {
  if (!mount || !host) return null;
  const langIndex = () => (String(getLang?.() || "en").toLowerCase().startsWith("zh") ? 0 : 1);
  const text = (key) => labels[key]?.[langIndex()] || key;
  const toolText = (name) => text(TOOL_LABEL_KEYS[name] || name);

  const section = document.createElement("section");
  section.id = "mcpSettings";
  section.className = "card agent-settings mcp-settings";
  section.dataset.settingsPage = "ai";
  section.hidden = true;
  section.setAttribute("aria-labelledby", "mcpSettingsTitle");
  section.innerHTML = `
    <div class="card-title"><span class="dot"></span><span id="mcpSettingsTitle" data-mcp="title"></span></div>
    <form id="mcpSettingsForm" novalidate>
      <div class="agent-settings-scroll">
        <label class="check"><input id="mcpEnabled" type="checkbox"><span data-mcp="enable"></span></label>
        <p class="field-hint" data-mcp="enableHint"></p>
        <p class="field-hint mcp-warning" data-mcp="warning"></p>
        <label class="field"><span class="field-label" data-mcp="access"></span>
          <select id="mcpAccess">
            <option value="confirm" data-mcp-option="accessConfirm"></option>
            <option value="auto" data-mcp-option="accessAuto"></option>
            <option value="read" data-mcp-option="accessRead"></option>
          </select>
          <span class="field-hint" data-mcp="accessHint"></span>
        </label>
        <p id="mcpAutoWarning" class="field-hint mcp-warning" data-mcp="accessAutoWarning" hidden></p>
        <label class="field"><span class="field-label" data-mcp="port"></span>
          <input id="mcpPort" type="number" inputmode="numeric" min="1024" max="65535" step="1">
          <span class="field-hint" data-mcp="portHint"></span>
        </label>
        <label class="field"><span class="field-label" data-mcp="bind"></span>
          <select id="mcpBind">
            <option value="lan" data-mcp-option="bindLan"></option>
            <option value="loopback" data-mcp-option="bindLoopback"></option>
          </select>
        </label>
        <div id="mcpEndpointBlock" hidden>
          <label class="field"><span class="field-label" data-mcp="endpoint"></span>
            <span class="password-row">
              <input id="mcpUrl" type="text" readonly spellcheck="false">
              <button id="mcpCopyUrl" class="btn btn-soft" type="button" data-mcp="copy"></button>
            </span>
          </label>
          <label class="field"><span class="field-label" data-mcp="token"></span>
            <span class="password-row">
              <input id="mcpToken" type="password" readonly spellcheck="false" autocomplete="off">
              <button id="mcpTokenToggle" class="btn btn-soft" type="button" data-mcp="showToken"></button>
              <button id="mcpCopyToken" class="btn btn-soft" type="button" data-mcp="copy"></button>
            </span>
            <span class="field-hint" data-mcp="tokenHint"></span>
          </label>
          <div class="agent-actions">
            <button id="mcpRegenerate" class="btn" type="button" data-mcp="regenerate"></button>
          </div>
        </div>
        <details id="mcpClientConfig" class="mcp-client-config">
          <summary data-mcp="clientConfig"></summary>
          <p class="field-hint" data-mcp="clientConfigHint"></p>
          <pre id="mcpSnippet" class="mcp-snippet"></pre>
          <div class="agent-actions"><button id="mcpCopySnippet" class="btn" type="button" data-mcp="copy"></button></div>
        </details>
        <details id="mcpAudit" class="mcp-audit">
          <summary data-mcp="audit"></summary>
          <p class="field-hint" data-mcp="auditHint"></p>
          <div id="mcpAuditList" class="mcp-audit-list" role="status"></div>
        </details>
        <p id="mcpStatus" class="field-hint" role="status"></p>
      </div>
    </form>`;

  // Appended in the settings panel rather than in index.html: the section must
  // not exist on builds without the native MCP listener (web, iOS, HarmonyOS).
  const anchor = mount.querySelector("#agentSettings");
  if (anchor) anchor.after(section);
  else mount.appendChild(section);

  const $ = (id) => section.querySelector(`#${id}`);
  const elements = {
    enabled: $("mcpEnabled"), access: $("mcpAccess"),
    port: $("mcpPort"), bind: $("mcpBind"), endpoint: $("mcpEndpointBlock"),
    url: $("mcpUrl"), token: $("mcpToken"), tokenToggle: $("mcpTokenToggle"),
    copyUrl: $("mcpCopyUrl"), copyToken: $("mcpCopyToken"), regenerate: $("mcpRegenerate"),
    snippet: $("mcpSnippet"), copySnippet: $("mcpCopySnippet"), status: $("mcpStatus"),
    auditList: $("mcpAuditList"), clientConfig: $("mcpClientConfig"), audit: $("mcpAudit"),
    autoWarning: $("mcpAutoWarning"),
  };

  const banner = document.createElement("div");
  banner.id = "mcpApproval";
  banner.className = "mcp-approval";
  banner.hidden = true;
  banner.setAttribute("role", "alertdialog");
  banner.setAttribute("aria-live", "assertive");
  banner.innerHTML = `
    <p class="mcp-approval-title"></p>
    <p class="mcp-approval-tool"></p>
    <p class="mcp-approval-args-label" data-mcp="approvalArgs"></p>
    <pre class="mcp-approval-args"></pre>
    <p class="field-hint" data-mcp="approvalHint"></p>
    <div class="agent-actions">
      <button type="button" class="btn" data-mcp="approvalDeny"></button>
      <button type="button" class="btn btn-primary" data-mcp="approvalAllow"></button>
    </div>`;
  document.body.append(banner);
  const bannerParts = {
    title: banner.querySelector(".mcp-approval-title"),
    tool: banner.querySelector(".mcp-approval-tool"),
    argsLabel: banner.querySelector(".mcp-approval-args-label"),
    args: banner.querySelector(".mcp-approval-args"),
    deny: banner.querySelector('[data-mcp="approvalDeny"]'),
    allow: banner.querySelector('[data-mcp="approvalAllow"]'),
  };

  let config = host.getConfig();
  let state = host.getState();
  let busy = false;
  let pendingApproval = null;
  let refreshInFlight = false;
  let refreshTimer = null;

  function applyLabels() {
    section.querySelectorAll("[data-mcp]").forEach((node) => {
      const key = node.getAttribute("data-mcp");
      if (labels[key]) node.textContent = text(key);
    });
    section.querySelectorAll("[data-mcp-option]").forEach((node) => {
      const key = node.getAttribute("data-mcp-option");
      if (labels[key]) node.textContent = text(key);
    });
    bannerParts.argsLabel.textContent = text("approvalArgs");
    bannerParts.deny.textContent = text("approvalDeny");
    bannerParts.allow.textContent = text("approvalAllow");
    banner.querySelector(".mcp-approval-title").textContent = text("approvalTitle");
  }

  function setStatus(message, isError = false) {
    elements.status.textContent = message || "";
    elements.status.classList.toggle("field-error", Boolean(isError));
  }

  function tokenVisible() {
    return elements.token.type === "text";
  }

  function render() {
    const lang = langIndex();
    section.hidden = false;
    elements.enabled.checked = Boolean(config.enabled);
    if (document.activeElement !== elements.port) elements.port.value = String(config.port);
    elements.access.value = config.access;
    elements.access.classList.toggle("is-danger", config.access === "auto");
    elements.autoWarning.hidden = config.access !== "auto";
    elements.bind.value = config.bind;
    elements.access.disabled = busy;
    elements.port.disabled = busy;
    elements.bind.disabled = busy;
    elements.enabled.disabled = busy;
    elements.tokenToggle.textContent = tokenVisible() ? text("hideToken") : text("showToken");
    elements.tokenToggle.setAttribute("aria-pressed", String(tokenVisible()));

    const endpointReady = Boolean(config.enabled && (state.running || state.token));
    elements.endpoint.hidden = !endpointReady;
    const url = state.url || (config.bind === "loopback"
      ? `http://127.0.0.1:${config.port}/mcp`
      : `http://<phone-ip>:${config.port}/mcp`);
    if (document.activeElement !== elements.url) elements.url.value = url;
    elements.token.value = state.token || "";
    elements.regenerate.disabled = busy || !state.running;
    const snippet = claudeConfigSnippet({ url: state.url || url, token: state.token || "" });
    elements.snippet.textContent = snippet;

    if (!config.enabled) setStatus(text("stopped"));
    else if (state.lastError) setStatus(`${text("errorPrefix")}${state.lastError}`, true);
    else if (state.running) {
      const foreground = state.foreground ? text("foreground") : text("background");
      setStatus(`${text("running")} · ${state.url || url} · ${foreground}`, !state.foreground);
    } else setStatus(text("idle"), true);

    renderAudit(lang);
  }

  function renderAudit() {
    const entries = host.getAudit?.() || [];
    elements.auditList.textContent = "";
    if (!entries.length) {
      const empty = document.createElement("p");
      empty.className = "field-hint";
      empty.textContent = text("auditEmpty");
      elements.auditList.append(empty);
      return;
    }
    for (const entry of entries) {
      const row = document.createElement("div");
      row.className = "mcp-audit-row";
      const head = document.createElement("p");
      head.className = "mcp-audit-head";
      const time = entry.at ? new Date(entry.at) : null;
      const stamp = time && !Number.isNaN(time.getTime()) ? time.toLocaleTimeString() : "";
      head.textContent = `${stamp} · ${toolText(entry.tool)}`;
      const meta = document.createElement("p");
      meta.className = "field-hint";
      const parts = [entry.outcome, entry.message].filter(Boolean);
      if (entry.remote) parts.push(entry.remote);
      meta.textContent = parts.join(" · ");
      row.append(head, meta);
      elements.auditList.append(row);
    }
  }

  async function refresh() {
    if (refreshInFlight) return;
    refreshInFlight = true;
    try {
      if (typeof host.refreshState === "function") state = await host.refreshState();
      else state = host.getState();
      config = host.getConfig();
      render();
    } finally {
      refreshInFlight = false;
    }
  }

  function readForm() {
    return {
      port: Number(elements.port.value),
      bind: elements.bind.value,
      access: elements.access.value,
    };
  }

  async function copyText(value) {
    try {
      await navigator.clipboard.writeText(value);
      setStatus(text("copied"));
    } catch {
      // A WebView without clipboard permission still lets the user select text.
      setStatus(text("copyFailed"), true);
    }
  }

  elements.enabled.addEventListener("change", async () => {
    if (busy) return;
    /* Capture the intent before render(): render() mirrors the persisted
     * configuration, which does not have this change yet, so reading the
     * checkbox afterwards would undo the user's click. */
    const wanted = elements.enabled.checked;
    const form = readForm();
    busy = true;
    render();
    try {
      await host.setConfig(form);
      config = host.getConfig();
      if (wanted) await host.enable();
      else await host.disable();
    } catch (error) {
      setStatus(`${text("errorPrefix")}${error?.message || ""}`, true);
    } finally {
      busy = false;
      await refresh();
    }
  });

  for (const input of [elements.port, elements.bind, elements.access]) {
    input.addEventListener("change", async () => {
      if (busy) return;
      /* Same ordering rule as the enable checkbox: read the edited controls
       * first, because render() rewrites them from the saved configuration. */
      const form = readForm();
      /* Turning the approval click off is a security decision, not a routine
       * preference: ask once, and leave the previous mode selected on cancel. */
      if (input === elements.access && form.access === "auto" && config.access !== "auto"
          && !window.confirm(text("accessAutoConfirm"))) {
        render();
        return;
      }
      busy = true;
      render();
      try { await host.setConfig(form); config = host.getConfig(); setStatus(""); }
      catch (error) { setStatus(error?.message || text("portError"), true); }
      finally { busy = false; await refresh(); }
    });
  }

  elements.tokenToggle.addEventListener("click", () => {
    elements.token.type = tokenVisible() ? "password" : "text";
    render();
  });
  elements.copyUrl.addEventListener("click", () => copyText(elements.url.value));
  elements.copyToken.addEventListener("click", () => copyText(elements.token.value));
  elements.copySnippet.addEventListener("click", () => copyText(elements.snippet.textContent || ""));

  elements.regenerate.addEventListener("click", async () => {
    if (busy || !state.running) return;
    if (!window.confirm(text("regenerateConfirm"))) return;
    busy = true;
    render();
    try {
      if (typeof host.regenerateToken === "function") await host.regenerateToken();
      await refresh();
      setStatus(text("regenerated"));
    } catch (error) {
      setStatus(`${text("errorPrefix")}${error?.message || ""}`, true);
    } finally {
      busy = false;
      await refresh();
    }
  });

  function hideBanner() {
    banner.hidden = true;
    bannerParts.tool.textContent = "";
    bannerParts.args.textContent = "";
  }

  function showBanner(request) {
    bannerParts.title.textContent = text("approvalTitle");
    bannerParts.tool.textContent = toolText(request.tool);
    const redacted = redactToolArguments(request.tool, request.args || {});
    let pretty;
    try { pretty = JSON.stringify(redacted, null, 2); } catch { pretty = String(redacted); }
    bannerParts.args.textContent = pretty;
    banner.hidden = false;
    bannerParts.allow.focus({ preventScroll: true });
  }

  function requestApproval(request) {
    if (pendingApproval) {
      return Promise.resolve({ approved: false, reason: text("approvalBusy") });
    }
    if (typeof document !== "undefined" && document.visibilityState !== "visible") {
      return Promise.resolve({ approved: false, reason: text("approvalBackground") });
    }
    return new Promise((resolve) => {
      const timeoutMs = Math.min(120000, Math.max(5000, Number(request?.timeoutMs) || APPROVAL_TIMEOUT_MS));
      const timer = setTimeout(() => settle(false, text("approvalTimeout")), timeoutMs);
      const pending = { request, resolve, timer, settle: null };
      pendingApproval = pending;
      pending.settle = (approved, reason) => {
        if (pendingApproval !== pending) return;
        pendingApproval = null;
        clearTimeout(timer);
        hideBanner();
        resolve({ approved, reason });
      };
      const settle = pending.settle;
      showBanner(request || {});
    });
  }

  function settlePending(approved, reason) {
    pendingApproval?.settle?.(approved, reason);
  }

  bannerParts.allow.addEventListener("click", () => settlePending(true));
  bannerParts.deny.addEventListener("click", () => settlePending(false, text("approvalDenied")));
  banner.addEventListener("keydown", (event) => {
    if (event.key === "Escape") {
      event.preventDefault();
      settlePending(false, text("approvalDenied"));
    }
  });

  applyLabels();
  render();
  // Keep the state line (foreground/background, active requests) honest while
  // the user has the settings panel open; a bridge event refreshes it too.
  refreshTimer = setInterval(() => {
    if (config.enabled && !document.hidden && !busy) refresh().catch(() => {});
  }, REFRESH_INTERVAL_MS);

  return {
    element: section,
    refresh,
    refreshLang() { applyLabels(); render(); },
    requestApproval,
    dispose() {
      if (refreshTimer) clearInterval(refreshTimer);
      refreshTimer = null;
      settlePending(false, text("approvalDenied"));
    },
  };
}
