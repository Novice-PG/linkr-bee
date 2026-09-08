import { available, createSerialAgent } from "./agent_runtime.js";
import { createDeviceExecutor } from "./device_executor.js";

const labels = {
  title: ["Agent", "Agent"], settingsButton: ["设置", "Settings"],
  enterMode: ["进入 Agent 模式", "Enter Agent mode"], exitMode: ["退出 Agent 模式", "Exit Agent mode"],
  showTerminal: ["终端已收起 · 查看终端", "Terminal collapsed · Show terminal"],
  mode: ["执行档位", "Execution mode"],
  modeCaption: ["档位", "Mode"],
  shiftMode: ["切换执行档位", "Change execution mode"],
  closePicker: ["关闭档位选择", "Close mode selector"],
  fullAuto: ["Full Auto", "Full Auto"], auto: ["Auto · 推荐", "Auto · Recommended"], manual: ["手动", "Manual"],
  "help-full-auto": ["命令直接执行，无需确认。", "Commands execute directly, without confirmation."],
  "help-auto": ["识别到 Shell 提示符时自动执行低风险查询，其余输入需确认。", "Low-risk queries run at a recognized shell prompt; other input needs approval."],
  "help-manual": ["AI 提出命令建议；点击发送后才会输入到被控机。", "AI proposes commands; click Send to enter them on the target."],
  modeChanged: ["档位已切换，对话已保留。本轮已停止，待确认输入已取消；已发送的输入无法撤回。请继续提问。", "Mode changed; conversation retained. This run stopped and pending input was cancelled; sent input cannot be recalled. Ask again to continue."],
  automatic: ["自动发送到当前被控机（控制字符已转义）：", "Automatically sending to the target (control characters escaped):"],
  send: ["提问", "Ask"], stop: ["停止", "Stop"], clear: ["新对话", "New chat"], close: ["退出", "Exit"],
  question: ["描述你遇到的问题", "Describe the problem"],
  empty: ["例如：根据当前串口日志，分析系统为什么启动失败。", "For example: why did boot fail, based on the current serial log?"],
  thinking: ["正在分析…", "Analyzing…"], user: ["你", "You"], assistant: ["助手", "Assistant"],
  approve: ["发送", "Send"], reject: ["拒绝", "Reject"], approval: ["确认发送到当前被控机（控制字符已转义）：", "Send to the current target? (Control characters are escaped.)"],
  stopped: ["已停止。已发送的串口输入无法撤回；如需中断被控机上的程序，请在终端发送 Ctrl-C。", "Stopped. Already sent input cannot be recalled; use Ctrl-C in the terminal to interrupt the target program."],
  changed: ["设备连接已改变，助手已停止。新连接将使用独立的日志和对话。", "Connection changed; assistant stopped. A new connection uses separate logs and conversation."],
  limit: ["已达到本轮排查步数上限，可以补充信息后继续提问。", "Diagnostic step limit reached. Add information or ask a follow-up."],
  error: ["请求失败：", "Request failed: "],
  working: ["处理中…", "Working…"], noOutput: ["暂无新的串口输出。", "No new serial output."],
  delivered: ["输入已发送，尚未确认命令执行结果。", "Input sent; command completion is not yet confirmed."],
  "wait-settled": ["输出暂时稳定，仍需核对执行结果。", "Output settled; the execution result still needs verification."],
  "wait-streaming": ["等待时间已到，设备仍在输出。", "Wait deadline reached; output is still streaming."],
  "wait-no-output": ["等待时间已到，尚无新的输出。", "Wait deadline reached without new output."],
  deliveryUnknown: ["传输结果不确定，可能已发送部分输入，请先查看终端。", "Delivery is uncertain; some input may have been sent. Inspect the terminal first."],
  consoleHint: ["串口状态推测", "Console hint"],
  "console-shell": ["Shell 提示符", "Shell prompt"], "console-login": ["等待登录", "Login prompt"],
  "console-password": ["等待密码", "Password prompt"], "console-bootloader": ["引导程序", "Bootloader"],
  "console-panic": ["内核崩溃", "Kernel panic"], "console-unknown": ["暂无法判断", "Unknown"],
  "execution-approved": ["已确认", "Approved"], "execution-sending": ["发送中", "Sending"],
  "execution-sent": ["已发送", "Sent"], "execution-denied": ["已拒绝", "Denied"],
  "execution-cancelled": ["已取消", "Cancelled"], "execution-failed": ["发送未完成", "Send incomplete"],
  "observation-no-output": ["尚未收到后续输出。", "No subsequent output yet."],
  "observation-output-observed": ["已收到后续输出，结果待核实。", "Subsequent output received; verify the result."],
  "observation-prompt-returned": ["检测到 Shell 提示符返回，仍需核对输出。", "A shell prompt returned; the output still needs verification."],
  "observation-interrupted": ["连接或终端输入已变化，已停止关联后续输出。", "Connection or terminal input changed; subsequent output is no longer attributed to this action."],
  evidence: ["后续串口输出：", "Subsequent serial output:"], truncated: ["输出已截断。", "Output truncated."],
  connected: ["已连接", "Connected"], disconnected: ["未连接", "Disconnected"],
  read_serial_log: ["读取串口日志", "Read serial log"],
  get_device_status: ["读取设备状态", "Read device status"],
  send_serial_input: ["请求发送串口输入", "Request serial input"],
  wait_for_serial_output: ["等待串口输出", "Wait for serial output"],
  inspect_serial_execution: ["核查执行结果", "Inspect execution"],
};

export function createAgentPanel({ button, workspace, terminal, settings, openSettings, onOpen, onClose, onLayout, focusTerminal, getLang, getStatus, readLog, prepareInput, sendInput }) {
  if (!available) return null;
  button.hidden = false;
  const text = (key) => labels[key]?.[getLang().startsWith("zh") ? 0 : 1] || key;
  const dialog = document.createElement("section");
  dialog.className = "agent-panel window-surface";
  dialog.id = "agentPanel";
  dialog.hidden = true;
  dialog.setAttribute("role", "region");
  dialog.setAttribute("aria-labelledby", "agentTitle");
  // This template is constant; model responses and UART content use textContent.
  dialog.innerHTML = `
    <header class="agent-header window-header"><div class="agent-heading"><strong id="agentTitle" data-ai="title"></strong>
      <button type="button" class="agent-gear" id="agentModeButton" aria-controls="agentModePicker" aria-expanded="false">
        <span class="agent-gear-track" aria-hidden="true"><i></i><i></i><i></i><b></b></span>
        <span class="agent-gear-label"><span class="agent-gear-caption" data-ai="modeCaption"></span><span id="agentActiveMode" aria-live="polite"></span></span>
        <svg class="agent-gear-chevron" aria-hidden="true" viewBox="0 0 16 16" width="12" height="12" fill="none" stroke="currentColor" stroke-width="1.5"><path d="m4 6 4 4 4-4"/></svg>
      </button></div>
      <div class="agent-actions"><button type="button" class="btn" id="agentSettingsButton" data-ai="settingsButton" aria-controls="controlsPanel"></button>
      <button type="button" class="btn" id="agentNew"><svg aria-hidden="true" viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.75"><path d="M12 5v14M5 12h14"/></svg><span data-ai="clear"></span></button>
      <button type="button" class="btn" id="agentClose" data-ai="close"></button></div></header>
    <fieldset class="agent-mode-picker window-surface" id="agentModePicker" hidden aria-label="Execution mode">
      <legend class="agent-mode-legend" data-ai="mode"></legend>
      <div class="agent-mode-title window-header"><strong data-ai="shiftMode"></strong><button type="button" class="icon-btn" id="agentModeClose"><svg aria-hidden="true" viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.75"><path d="m6 6 12 12M6 18 18 6"/></svg></button></div>
      <div class="agent-mode-options">
        <label><input type="radio" name="agentMode" value="manual"><span class="agent-mode-option"><b class="agent-mode-gear" aria-hidden="true">M</b><span><strong data-ai="manual"></strong><small data-ai="help-manual"></small></span></span></label>
        <label><input type="radio" name="agentMode" value="auto" checked><span class="agent-mode-option"><b class="agent-mode-gear" aria-hidden="true">A</b><span><strong data-ai="auto"></strong><small data-ai="help-auto"></small></span></span></label>
        <label><input type="radio" name="agentMode" value="full-auto"><span class="agent-mode-option"><b class="agent-mode-gear" aria-hidden="true">F</b><span><strong data-ai="fullAuto"></strong><small data-ai="help-full-auto"></small></span></span></label>
      </div>
    </fieldset>
    <p id="agentConsole" class="agent-console"></p>
    <div id="agentMessages" class="agent-messages" role="log" aria-live="polite"><p class="agent-empty" data-ai="empty"></p></div>
    <p id="agentStatus" class="agent-status" role="status"></p>
    <form id="agentForm" class="agent-form"><label class="agent-question"><span data-ai="question"></span>
      <textarea id="agentQuestion" rows="1" maxlength="4000" required></textarea></label>
      <div class="agent-actions"><button class="btn" id="agentStop" type="button" data-ai="stop" disabled></button>
      <button class="btn btn-primary" id="agentAsk" type="submit" data-ai="send"></button></div></form>`;
  const terminalPeek = document.createElement("button");
  terminalPeek.type = "button";
  terminalPeek.className = "agent-terminal-peek";
  terminalPeek.id = "agentShowTerminal";
  terminalPeek.hidden = true;
  terminalPeek.setAttribute("aria-controls", terminal.id);
  workspace.append(terminalPeek, dialog);
  button.setAttribute("aria-controls", dialog.id);
  button.setAttribute("aria-pressed", "false");
  const $ = (id) => dialog.querySelector(`#${id}`);
  const messages = $("agentMessages");
  const modeButton = $("agentModeButton");
  const modePicker = $("agentModePicker");
  const modeOptions = [...dialog.querySelectorAll('[name="agentMode"]')];
  let runner = null;
  let opened = false;
  let opening = false;
  let keyboardOpen = false;
  let editingAgent = false;
  let layoutRaf = 0;
  // Session-only preference: a fresh app starts in recommended Auto mode.
  let executionMode = "auto";
  let fingerprint = "";
  let busy = false;
  let version = 0;
  let eventVersion = 0;
  let answer = null;
  let activeInputRow = null;
  const toolRows = new Map();
  const executionRows = new Map();
  const device = createDeviceExecutor({ getStatus, readLog, prepareInput, sendInput, onRecord: renderExecution });
  let observeTimer = null;

  function refreshLang() {
    for (const el of dialog.querySelectorAll("[data-ai]")) el.textContent = text(el.dataset.ai);
    button.title = text(opened ? "exitMode" : "enterMode");
    button.setAttribute("aria-label", button.title);
    terminalPeek.textContent = text("showTerminal");
    $("agentQuestion").placeholder = text("question");
    const modeLabel = executionMode === "auto" ? "Auto" : text(executionMode === "full-auto" ? "fullAuto" : executionMode);
    $("agentActiveMode").textContent = modeLabel;
    modeButton.setAttribute("aria-label", `${text("shiftMode")} · ${modeLabel}`);
    modeButton.title = `${text("shiftMode")} · ${text(`help-${executionMode}`)}`;
    modeButton.style.setProperty("--gear-index", modeOptions.findIndex((option) => option.value === executionMode));
    modePicker.setAttribute("aria-label", text("mode"));
    $("agentModeClose").title = text("closePicker");
    $("agentModeClose").setAttribute("aria-label", text("closePicker"));
    $("agentNew").title = text("clear");
    $("agentNew").setAttribute("aria-label", text("clear"));
    refreshConsole();
  }
  function addMessage(role, content = "") {
    messages.querySelector(".agent-empty")?.remove();
    const row = document.createElement("section");
    row.className = `agent-message agent-${role}`;
    const label = document.createElement("b");
    label.textContent = text(role);
    const body = document.createElement("div");
    body.className = "agent-message-text";
    body.textContent = content;
    row.append(label, body);
    messages.append(row);
    messages.scrollTop = messages.scrollHeight;
    return body;
  }
  function setBusy(value) {
    busy = value;
    settings.setBusy(value);
    $("agentAsk").disabled = $("agentNew").disabled = value;
    $("agentStop").disabled = !value;
    $("agentStatus").textContent = value ? text("thinking") : "";
  }
  function stop(reason, { preserveConversation = false } = {}) {
    version++;
    runner?.abort();
    device.cancel();
    if (!preserveConversation) { runner = null; fingerprint = ""; }
    activeInputRow = null;
    toolRows.clear();
    if (reason && (busy || preserveConversation)) addMessage("assistant", text(reason));
    // The active prompt's finally block unlocks the UI once Pi settles.
  }
  function renderExecution(record) {
    let body = executionRows.get(record.id);
    if (!body) {
      body = activeInputRow || addMessage("send_serial_input");
      body.dataset.execution = record.id;
      executionRows.set(record.id, body);
    }
    const waiting = record.state === "awaiting-approval";
    const heading = waiting ? text("approval") : `${record.mode} · ${text(`execution-${record.state}`)}`;
    const details = record.delivery === "sent" ? `${text("delivered")}\n${text(`observation-${record.observation}`)}`
      : record.delivery === "unknown" ? text("deliveryUnknown") : "";
    body.textContent = [heading, JSON.stringify(record.payload), details, record.error,
      record.evidence && `${text("evidence")}\n${record.evidence.length > 900 ? "…\n" : ""}${record.evidence.slice(-900)}`,
      (record.evidenceTruncated || record.evidence.length > 900) && text("truncated")].filter(Boolean).join("\n");
    let actions = body.parentElement.querySelector(".agent-actions");
    if (waiting && !actions) {
      actions = document.createElement("div");
      actions.className = "agent-actions";
      for (const [label, className, act] of [
        ["approve", "btn btn-primary", () => device.approve(record.id)],
        ["reject", "btn", () => device.reject(record.id)],
      ]) {
        const button = document.createElement("button");
        button.type = "button";
        button.className = className;
        button.textContent = text(label);
        button.addEventListener("click", act);
        actions.append(button);
      }
      body.after(actions);
    }
    actions?.querySelectorAll("button").forEach((button) => { button.disabled = !waiting; });
    refreshConsole();
  }
  function refreshConsole() {
    $("agentConsole").textContent = `${text("consoleHint")} · ${text(`console-${device.getStatus().console.kind}`)}`;
  }
  function logsChanged() {
    if (observeTimer !== null) return;
    observeTimer = setTimeout(() => {
      observeTimer = null;
      device.observe();
      refreshConsole();
    }, 100);
  }
  function clearConversation() {
    device.reset();
    executionRows.clear();
    messages.replaceChildren();
  }
  function onEvent(event, expectedVersion) {
    if (version !== expectedVersion) return;
    if (event.type === "message_start" && event.message.role === "assistant") answer = null;
    if (event.type === "message_update" && event.assistantMessageEvent.type === "text_delta") {
      answer ??= addMessage("assistant");
      answer.textContent += event.assistantMessageEvent.delta;
      messages.scrollTop = messages.scrollHeight;
    }
    if (event.type === "tool_execution_start") {
      const body = addMessage(event.toolName, text("working"));
      toolRows.set(event.toolCallId, body);
      if (event.toolName === "send_serial_input") activeInputRow = body;
    }
    if (event.type === "tool_execution_end") {
      let preview = event.result?.content?.filter((part) => part.type === "text").map((part) => part.text).join("\n") || "";
      if (!event.isError) {
        try {
          const value = JSON.parse(preview);
          if (event.toolName === "read_serial_log" || event.toolName === "wait_for_serial_output") preview = [
            value.waitStatus && text(`wait-${value.waitStatus}`), value.text || text("noOutput"),
          ].filter(Boolean).join("\n");
          if (event.toolName === "send_serial_input") preview = text("delivered");
          if (event.toolName === "get_device_status") preview = [text(value.connected ? "connected" : "disconnected"), value.device, value.transport?.toUpperCase(), value.uart, text(`console-${value.console?.kind || "unknown"}`)].filter(Boolean).join(" · ");
          if (event.toolName === "inspect_serial_execution") preview = [
            value.delivery === "sent" ? text("delivered") : text("deliveryUnknown"),
            text(`observation-${value.observation}`), value.waitStatus && text(`wait-${value.waitStatus}`), value.evidence?.slice(-1600),
            (value.evidenceTruncated || value.evidence?.length > 1600) && text("truncated"),
          ].filter(Boolean).join("\n");
        } catch { /* Tool errors can be plain text. */ }
      }
      const body = toolRows.get(event.toolCallId) || addMessage(event.toolName);
      if (!body.dataset.execution) body.textContent = preview.slice(0, 2000) + (preview.length > 2000 ? "…" : "");
      toolRows.delete(event.toolCallId);
      if (event.toolName === "send_serial_input") activeInputRow = null;
    }
  }
  $("agentForm").addEventListener("submit", async (event) => {
    event.preventDefault();
    const question = $("agentQuestion").value.trim();
    if (busy || !question) return;
    const config = settings.getConfig();
    if (!config) {
      openSettings();
      return;
    }
    setBusy(true);
    const runVersion = version;
    const timer = setTimeout(() => stop("stopped"), 180000);
    try {
      // Only explicitly saved configuration is used for model requests.
      const nextFingerprint = JSON.stringify(config);
      if (!runner || fingerprint !== nextFingerprint) {
        runner = await createSerialAgent({ config, device,
          onEvent: (event) => onEvent(event, eventVersion) });
        fingerprint = nextFingerprint;
      }
      if (version !== runVersion) { runner?.abort(); runner = null; return; }
      eventVersion = runVersion;
      $("agentQuestion").value = "";
      addMessage("user", question);
      const outcome = await runner.prompt(question);
      if (outcome.limitReached && version === runVersion) addMessage("assistant", text("limit"));
    } catch (error) {
      if (version === runVersion) addMessage("assistant", `${text("error")}${error.message}`);
    } finally {
      clearTimeout(timer);
      setBusy(false);
    }
  });
  $("agentStop").addEventListener("click", () => stop("stopped"));
  function positionModePicker() {
    if (modePicker.hidden) return;
    const viewport = window.visualViewport;
    const top = viewport?.offsetTop || 0;
    const left = viewport?.offsetLeft || 0;
    const height = viewport?.height || window.innerHeight;
    const width = viewport?.width || window.innerWidth;
    const anchor = modeButton.getBoundingClientRect();
    modePicker.style.width = `${Math.min(328, width - 16)}px`;
    modePicker.style.maxHeight = `${height - 16}px`;
    const box = modePicker.getBoundingClientRect();
    modePicker.style.left = `${Math.max(left + 8, Math.min(anchor.left, left + width - box.width - 8))}px`;
    modePicker.style.top = `${Math.max(top + 8, Math.min(anchor.bottom + 6, top + height - box.height - 8))}px`;
  }
  function showModePicker(show, restoreFocus = false) {
    modePicker.hidden = !show;
    modeButton.setAttribute("aria-expanded", String(show));
    if (show) {
      positionModePicker();
      modeOptions.find((option) => option.checked)?.focus({ preventScroll: true });
    } else if (restoreFocus) modeButton.focus({ preventScroll: true });
  }
  modeButton.addEventListener("click", () => showModePicker(modePicker.hidden));
  $("agentModeClose").addEventListener("click", () => showModePicker(false, true));
  for (const option of modeOptions) {
    option.addEventListener("click", () => {
      if (option.value !== executionMode) {
        stop("modeChanged", { preserveConversation: true });
        device.setMode(option.value);
        executionMode = device.mode;
        refreshLang();
      }
      showModePicker(false, true);
    });
  }
  modePicker.addEventListener("keydown", (event) => {
    const index = modeOptions.indexOf(document.activeElement);
    if (index < 0) return;
    let next = index;
    if (event.key === "ArrowUp" || event.key === "ArrowLeft") next = Math.max(0, index - 1);
    else if (event.key === "ArrowDown" || event.key === "ArrowRight") next = Math.min(modeOptions.length - 1, index + 1);
    else if (event.key === "Home") next = 0;
    else if (event.key === "End") next = modeOptions.length - 1;
    else if (event.key === "Enter") { event.preventDefault(); modeOptions[index].click(); return; }
    else return;
    // Navigation previews another gear; only Enter/Space or a tap engages it.
    event.preventDefault();
    modeOptions[next].focus({ preventScroll: true });
    modeOptions[next].closest("label").scrollIntoView({ block: "nearest" });
  });
  document.addEventListener("pointerdown", (event) => {
    if (!modePicker.hidden && !modePicker.contains(event.target) && !modeButton.contains(event.target)) showModePicker(false);
  });
  document.addEventListener("focusin", (event) => {
    if (!modePicker.hidden && !modePicker.contains(event.target) && !modeButton.contains(event.target)) showModePicker(false);
  });
  $("agentNew").addEventListener("click", () => {
    stop();
    clearConversation();
  });
  function closePanel() {
    if (!opened) return;
    stop("stopped");
    showModePicker(false);
    opened = false;
    dialog.hidden = true;
    terminalPeek.hidden = true;
    document.documentElement.classList.remove("agent-mode", "agent-keyboard-chat", "agent-keyboard-terminal");
    onClose?.();
    button.setAttribute("aria-pressed", "false");
    refreshLang();
    button.focus({ preventScroll: true });
    onLayout?.();
  }
  function syncLayout(options = {}) {
    if (options.keyboardOpen !== undefined) keyboardOpen = options.keyboardOpen;
    if (layoutRaf) return;
    layoutRaf = requestAnimationFrame(() => {
      layoutRaf = 0;
      if (!opened) return;
      const active = document.activeElement;
      if (terminal.contains(active)) editingAgent = false;
      else if (dialog.contains(active) && active.matches("input:not([type=radio]), textarea")) editingAgent = true;
      const collapsed = keyboardOpen && editingAgent;
      const root = document.documentElement;
      root.style.setProperty("--agent-viewport-top", `${window.visualViewport?.offsetTop || 0}px`);
      root.classList.toggle("agent-keyboard-chat", collapsed);
      root.classList.toggle("agent-keyboard-terminal", keyboardOpen && !editingAgent);
      terminalPeek.hidden = !collapsed;
      workspace.dataset.agentLayout = workspace.clientWidth >= 960 ? "columns" : "rows";
      dialog.classList.toggle("agent-tight", dialog.clientHeight < 260);
      dialog.classList.toggle("agent-narrow", dialog.clientWidth < 520);
      positionModePicker();
      onLayout?.();
    });
  }
  async function openPanel() {
    if (opening) return;
    if (opened) { closePanel(); return; }
    opening = true;
    opened = true;
    try {
      await onOpen?.();
      if (!opened) return;
      dialog.hidden = false;
      document.documentElement.classList.add("agent-mode");
      button.setAttribute("aria-pressed", "true");
      refreshLang();
      syncLayout();
      // Do not summon the keyboard on entry; both panes should be visible.
      $("agentClose").focus({ preventScroll: true });
    } catch (error) {
      closePanel();
      addMessage("assistant", `${text("error")}${error.message}`);
    } finally { opening = false; }
  }
  $("agentClose").addEventListener("click", closePanel);
  $("agentSettingsButton").addEventListener("click", openSettings);
  terminalPeek.addEventListener("click", () => { editingAgent = false; focusTerminal(); syncLayout(); });
  // Mobile app switches must cancel active work, but an idle conversation should
  // still match the visible history when the user returns with more information.
  document.addEventListener("visibilitychange", () => { if (document.hidden && busy) stop("stopped"); });
  // Escape in xterm remains a UART key. Escape within the assistant exits it.
  dialog.addEventListener("keydown", (event) => {
    // Escape belongs to the IME while the user is choosing/cancelling a candidate.
    if (event.isComposing || event.keyCode === 229) return;
    if (event.key === "Escape") {
      event.preventDefault();
      if (!modePicker.hidden) { event.stopPropagation(); showModePicker(false, true); }
      else closePanel();
    }
  });
  document.addEventListener("focusin", () => syncLayout());
  window.visualViewport?.addEventListener("resize", () => syncLayout());
  window.visualViewport?.addEventListener("scroll", () => syncLayout());
  window.addEventListener("resize", () => syncLayout());
  if (globalThis.ResizeObserver) {
    const observer = new ResizeObserver(() => syncLayout());
    observer.observe(workspace);
    observer.observe(dialog);
  }
  button.addEventListener("click", openPanel);
  refreshLang();
  return {
    settingsChanged() { stop(); },
    refreshLang,
    logsChanged,
    syncLayout,
    isOpen: () => opened,
    hasInputFocus: () => opened && dialog.contains(document.activeElement) && document.activeElement.matches("input:not([type=radio]), textarea"),
    logsCleared() { stop("stopped"); clearConversation(); refreshConsole(); },
    connectionChanged(connected) {
      stop("changed");
      if (connected) clearConversation();
      else device.observe();
      refreshConsole();
    },
  };
}
