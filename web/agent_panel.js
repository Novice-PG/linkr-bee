import { bindingReply, targetIdentityCommand, observedTargetId, observedTargetPath } from "./target_binding.js";
import { monitorSerialExecution } from "./serial_observation.js";
import { createTaskStore, deviceIdentity } from "./agent_tasks.js";
import { available, createSerialAgent } from "./agent_runtime.js";
import { createDeviceExecutor } from "./device_executor.js";
import { requestComputerDownload } from "./local_download.js";
import { renderAssistantMarkdown } from "./agent_markdown.js";

const labels = {
  history: ["任务记录与设备", "Tasks and device"], refreshProfile: ["探测设备", "Probe device"],
  noProfile: ["尚未探测设备能力", "Device capabilities not probed"], staleProfile: ["档案已过期，请重新探测", "Profile stale; probe again"],
  forgetTasks: ["清除记录", "Clear records"], recover: ["核实上次任务", "Verify previous task"],
  storageError: ["任务记录无法保存，请检查浏览器存储空间。", "Cannot save tasks; check browser storage."],
  taskNote: ["摘要保存在此浏览器，可能含设备信息。恢复时重新核实，不自动重发命令。", "Summaries are stored in this browser and may contain device information. Recovery verifies current state without replaying commands."],
  probe_device_profile: ["探测设备档案", "Probe device profile"],
  probe_tools: ["检查所需工具", "Check required tools"],
  update_task_plan: ["任务计划与验证记录", "Task plan and verification"],
  planAssessment: ["AI 记录的进度，请结合执行日志核实", "Progress recorded by AI; verify against execution logs"],
  steer: ["补充本轮", "Add to current task"], followUp: ["排队下一步", "Queue next step"],
  queueHelp: ["补充在当前工具结束后生效；排队在本轮结束后处理。停止会清空待处理消息。", "Add after the current tool finishes, or queue after this task. Stop clears pending messages."],
  clearQueue: ["清空待处理", "Clear pending"],
  "console-sudo-password": ["等待 sudo 密码，请在终端输入", "Sudo password; enter in terminal"],
  "console-confirmation": ["等待交互确认", "Awaiting confirmation"], "console-pager": ["分页器等待输入", "Pager awaiting input"],
  copy: ["复制", "Copy"], copied: ["已复制", "Copied"], copyFailed: ["复制失败，请手动选择", "Copy failed; select manually"],
  logs: ["执行日志", "Execution log"],
  title: ["Agent", "Agent"], settingsButton: ["设置", "Settings"],
  enterMode: ["进入 Agent 模式", "Enter Agent mode"], exitMode: ["退出 Agent 模式", "Exit Agent mode"],
  showTerminal: ["终端已收起 · 查看终端", "Terminal collapsed · Show terminal"],
  mode: ["执行档位", "Execution mode"],
  modeCaption: ["档位", "Mode"],
  shiftMode: ["切换执行档位", "Change execution mode"],
  closePicker: ["关闭档位选择", "Close mode selector"],
  fullAuto: ["Full Auto", "Full Auto"], auto: ["Auto · 推荐", "Auto · Recommended"], manual: ["手动", "Manual"],
  "help-full-auto": ["命令直接执行；识别出的破坏性或不可逆命令（递归/强制删除、磁盘与文件系统工具、dd、刷写与引导工具、下载内容管道进 shell、提权、递归改权限）仍需确认。该检测不覆盖脚本或间接执行中的所有操作。", "Commands run without confirmation. Recognized destructive or irreversible commands — recursive/forced deletes, disk and filesystem tools, dd, flashing and bootloader tools, downloaded content piped into a shell, privilege escalation, recursive permission changes — still need your approval. Detection does not cover every operation inside scripts or indirect execution."],
  "help-auto": ["识别到 Shell 提示符时自动执行低风险查询，其余输入需确认；破坏性命令在任何档位都需确认。", "Low-risk queries run at a recognized shell prompt; other input needs approval. Destructive commands need approval in every mode."],
  "help-manual": ["AI 提出命令建议；点击发送后才会输入到被控机。", "AI proposes commands; click Send to enter them on the target."],
  modeChanged: ["档位已切换，对话已保留。本轮已停止，待确认输入已取消；已发送的输入无法撤回。请继续提问。", "Mode changed; conversation retained. This run stopped and pending input was cancelled; sent input cannot be recalled. Ask again to continue."],
  automatic: ["自动发送到当前被控机（控制字符已转义）：", "Automatically sending to the target (control characters escaped):"],
  send: ["发送", "Send"], stop: ["停止", "Stop"], clear: ["新对话", "New chat"], close: ["退出", "Exit"],
  shortcuts: ["聚焦：{focus} · 发送：{send} · Enter 换行", "Focus: {focus} · Send: {send} · Enter for newline"],
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
  verificationNeeded: ["命令已结束，目标结果仍需验证", "Command completed; verify the intended result"],
  evidence: ["后续串口输出：", "Subsequent serial output:"], truncated: ["输出已截断。", "Output truncated."],
  connected: ["已连接", "Connected"], disconnected: ["未连接", "Disconnected"],
  probe_download_tools: ["探测目标机下载工具", "Probe download tools"],
  download_to_target: ["下载到目标机", "Download to target"],
  download_to_computer: ["下载到当前电脑 / 手机", "Download to this computer / phone"],
  run_shell_command: ["执行 Shell 命令", "Run shell command"],
  monitor_serial_execution: ["持续观察执行", "Monitor execution"],
  read_serial_log: ["读取串口日志", "Read serial log"],
  read_web_page: ["读取网页", "Read web page"],
  search_serial_log: ["查找串口日志", "Find serial evidence"],
  noMatches: ["当前范围内没有匹配内容", "No matches in this window"],
  readRange: ["读取范围", "Read range"],
  get_device_status: ["读取设备状态", "Read device status"],
  send_serial_input: ["请求发送串口输入", "Request serial input"],
  wait_for_serial_output: ["等待串口输出", "Wait for serial output"],
  inspect_serial_execution: ["核查执行结果", "Inspect execution"],
};

export function createAgentPanel({ button, workspace, terminal, settings, bindingControl, openSettings, onOpen, onClose, onLayout, focusTerminal, getLang, getStatus, readLog, prepareInput, sendInput }) {
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
    <details class="agent-history" id="agentHistory"><summary data-ai="history"></summary>
      <p data-ai="taskNote"></p><p id="agentProfile"></p>
      <div class="agent-actions"><button class="btn" type="button" id="agentProbe" data-ai="refreshProfile"></button><button class="btn" type="button" id="agentForget" data-ai="forgetTasks"></button></div>
      <div id="agentTasks"></div><p id="agentStorageError" role="status"></p></details>
    <div id="agentMessages" class="agent-messages" role="log" aria-live="polite"><p class="agent-empty" data-ai="empty"></p></div>
    <p id="agentStatus" class="agent-status" role="status"></p>
    <div id="agentQueueControls" class="agent-queue-controls" hidden>
      <p data-ai="queueHelp"></p>
      <div class="agent-actions"><button class="btn" type="button" id="agentSteer" data-ai="steer"></button><button class="btn" type="button" id="agentFollowUp" data-ai="followUp"></button></div>
      <div id="agentQueue" role="status"></div><button class="btn" type="button" id="agentClearQueue" data-ai="clearQueue" hidden></button>
    </div>
    <form id="agentForm" class="agent-form"><label class="agent-question"><span data-ai="question"></span>
      <textarea id="agentQuestion" rows="1" maxlength="4000" required></textarea></label>
      <div class="agent-actions"><button class="btn" id="agentStop" type="button" data-ai="stop" disabled></button>
      <button class="btn btn-primary" id="agentAsk" type="submit"><svg aria-hidden="true" viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round"><path d="M12 19V5m-6 6 6-6 6 6"/></svg></button></div></form>`;
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
  let bindingController = null;
  let bindingState = null;
  let rememberedProfile = null;
  let autoIdentitySession = null;
  const device = createDeviceExecutor({ getStatus: () => ({...getStatus(), targetBinding:bindingState, rememberedProfile}), readLog, prepareInput, sendInput, onRecord: renderExecution });
  let observeTimer = null;
  const taskStore = createTaskStore({ getItem: key => localStorage.getItem(key), setItem: (key,value) => localStorage.setItem(key,value) });
  let currentTask = null, recovery = null;
  let taskTimer = null;
  let historySignature = "";
  function persistTask() {
    if (!currentTask) return;
    try { taskStore.save(currentTask); } catch { $("agentStorageError").textContent = text("storageError"); }
  }
  function scheduleTaskSave() {
    if (taskTimer !== null) return;
    taskTimer = setTimeout(() => { taskTimer = null; persistTask(); }, 500);
  }
  function refreshHistory() {
    const status = device.getStatus(), profile = status.profile;
    $("agentProfile").textContent = profile ? [profile.stale && text("staleProfile"), profile.model || profile.system, profile.os, profile.storage, profile.tools.join(", ")].filter(Boolean).join("\n") : text("noProfile");
    $("agentProbe").disabled = busy || !status.connected;
    $("agentForget").disabled = busy;
    const tasks = taskStore.list(deviceIdentity(status));
    const signature = JSON.stringify([deviceIdentity(status),busy,tasks,getLang()]);
    if (signature === historySignature) return;
    historySignature = signature;
    const list = $("agentTasks"), expanded = new Set([...list.querySelectorAll("details[open]")].map(el => el.dataset.task));
    list.replaceChildren();
    for (const task of tasks) {
      const row = document.createElement("details"), title = document.createElement("summary"), description = document.createElement("p"), restore = document.createElement("button");
      row.dataset.task = task.id; row.open = expanded.has(task.id);
      title.textContent = `${task.status} · ${task.goal}`;
      description.textContent = [task.summary, formatPlan(task.plan)].filter(Boolean).join('\n');
      restore.type = "button"; restore.className = "btn"; restore.textContent = text("recover"); restore.disabled = busy;
      restore.addEventListener("click", () => {
        recovery = {goal:task.goal,summary:task.summary,status:task.status,executions:task.executions,plan:task.plan};
        $("agentQuestion").value = `${text("recover")}: ${task.goal}`;
        $("agentQuestion").focus();
      });
      row.append(title, description, restore); list.append(row);
    }
  }
  $("agentProbe").addEventListener("click", () => {
    recovery = null;
    $("agentQuestion").value = getLang().startsWith("zh") ? "请使用 probe_device_profile 探测当前目标机，观察执行完成后展示档案。" : "Use probe_device_profile to inspect the current target and report the completed profile.";
    $("agentForm").requestSubmit();
  });
  $("agentForget").addEventListener("click", () => {
    try { taskStore.clear(deviceIdentity(device.getStatus())); currentTask = null; recovery = null; refreshHistory(); }
    catch { $("agentStorageError").textContent = text("storageError"); }
  });
  window.addEventListener("pagehide", persistTask);
  setInterval(() => { if (opened && busy && !document.hidden) device.observe(); }, 1000);


  function refreshLang() {
    for (const el of dialog.querySelectorAll("[data-ai]")) el.textContent = text(el.dataset.ai);
    button.title = text(opened ? "exitMode" : "enterMode");
    button.setAttribute("aria-label", button.title);
    terminalPeek.textContent = text("showTerminal");
    const modifier = /Mac|iPhone|iPad/.test(navigator.platform) ? "⌘" : "Ctrl";
    const focusKeys = `${modifier}+Shift+K`, sendKeys = `${modifier}+Enter`;
    $("agentQuestion").placeholder = text("question");
    $("agentQuestion").title = text("shortcuts").replace("{focus}", focusKeys).replace("{send}", sendKeys);
    $("agentQuestion").setAttribute("aria-keyshortcuts", "Control+Shift+K Meta+Shift+K");
    $("agentAsk").title = `${text("send")} (${sendKeys})`;
    $("agentAsk").setAttribute("aria-label", text("send"));
    $("agentAsk").setAttribute("aria-keyshortcuts", "Control+Enter Meta+Enter");
    button.title += ` (${focusKeys})`;
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
    refreshConsole(); refreshHistory();
  }
  function addMessage(role, content = "") {
    messages.querySelector(".agent-empty")?.remove();
    const row = document.createElement("section");
    row.className = `agent-message agent-${role}`;
    const label = document.createElement("b");
    label.textContent = text(role);
    const body = document.createElement("div");
    body.className = "agent-message-text";
    if (role === "assistant") renderAssistantMarkdown(body, content);
    else body.textContent = content;
    row.append(label, body);
    messages.append(row);
    messages.scrollTop = messages.scrollHeight;
    return body;
  }
  function formatPlan(steps) {
    if (!Array.isArray(steps)) return '';
    const labels = getLang().startsWith('zh') ? {pending:'待执行',in_progress:'进行中',completed:'已完成',blocked:'受阻'} :
      {pending:'Pending',in_progress:'In progress',completed:'Completed',blocked:'Blocked'};
    return text('planAssessment')+'\n'+steps.map((step,index)=>`${index+1}. [${labels[step.status] || step.status}] ${step.title}\n${step.verification || ''}${step.nextAction ? '\n→ '+step.nextAction : ''}`).join('\n');
  }
  function setBusy(value) {
    busy = value;
    refreshHistory();
    settings.setBusy(value);
    $("agentAsk").disabled = $("agentNew").disabled = value;
    $("agentStop").disabled = !value;
    $("agentQueueControls").hidden = !value;
    $("agentSteer").disabled = $("agentFollowUp").disabled = true;
    $("agentStatus").textContent = value ? text("thinking") : "";
  }
  // Cancelling a run keeps its history; only explicit context/session resets
  // discard the runner. The active prompt must still settle before another ask.
  function stop(reason, { preserveConversation = true } = {}) {
    if (currentTask && busy) { currentTask.status = "interrupted"; persistTask(); }
    version++;
    runner?.abort();
    $("agentQueue").textContent = '';
    $("agentClearQueue").hidden = true;
    $("agentSteer").disabled = $("agentFollowUp").disabled = true;
    bindingController?.abort();
    device.cancel();
    if (!preserveConversation) { runner = null; fingerprint = ""; }
    activeInputRow = null;
    toolRows.clear();
    if (reason && (busy || reason === "modeChanged")) addMessage("assistant", text(reason));
    // The active prompt's finally block unlocks the UI once Pi settles.
  }
  function renderExecution(record) {
    if (currentTask) {
      currentTask.executions = [...currentTask.executions.filter(e => e.id !== record.id), record];
      scheduleTaskSave();
    }
    let body = executionRows.get(record.id);
    if (!body) {
      body = activeInputRow || addMessage("send_serial_input");
      body.dataset.execution = record.id;
      executionRows.set(record.id, body);
    }
    const waiting = record.state === "awaiting-approval";
    const heading = waiting ? text("approval") : `${record.mode} · ${text(`execution-${record.state}`)}`;
    const details = record.executionStatus === "completed" ? `Exit code: ${record.exitCode} · ${text("verificationNeeded")}` : record.delivery === "sent" ? `${text("delivered")}\n${text(`observation-${record.observation}`)}`
      : record.delivery === "unknown" ? text("deliveryUnknown") : "";
    const expanded = body.querySelector("details")?.open || false;
    body.replaceChildren();
    const status = document.createElement("p");
    if (record.executionStatus === "completed" || record.observationClosed || ["denied","cancelled","failed"].includes(record.state)) body.dataset.finishedAt ||= String(Date.now());
    const elapsed = Math.max(0, Math.floor((Number(body.dataset.finishedAt) || Date.now()) - Date.parse(record.createdAt || new Date().toISOString())) / 1000);
    status.textContent = `${heading} · ${elapsed.toFixed(1)} s\n${details}${record.waitingFor ? "\n" + text(`console-${record.waitingFor}`) : ""}${record.error ? "\n" + record.error : ""}`;
    const command = document.createElement("pre"), copy = document.createElement("button");
    command.textContent = JSON.stringify(record.payload);
    copy.type = "button"; copy.className = "btn"; copy.textContent = text("copy");
    copy.addEventListener("click", async () => { try { await navigator.clipboard.writeText(record.payload); copy.textContent = text("copied"); } catch { copy.textContent = text("copyFailed"); } });
    body.append(status, command, copy);
    if (record.download) {
      const download = document.createElement("p");
      download.textContent = `${text("download_to_target")} · ${record.download.path}\n${record.download.downloader} · ${record.download.status || "in progress"}\n${record.download.bytes ?? "?"} bytes · SHA-256: ${record.download.sha256 || "pending"}\nExpected SHA-256: ${record.download.expectedSha256 || "not provided (computed only)"}`;
      body.append(download);
    }
    if (record.evidence) {
      const log = document.createElement("details"), summary = document.createElement("summary"), output = document.createElement("pre");
      log.open = expanded; summary.textContent = text("logs");
      output.textContent = record.evidence.slice(-4000) + (record.evidenceTruncated ? "\n" + text("truncated") : "");
      log.append(summary,output); body.append(log);
    }
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
    const status = device.getStatus();
    if (bindingState?.verified && status.profile && !status.profile.stale) {
      try { localStorage.setItem(`linkr-target-profile:${bindingState.targetId}`, JSON.stringify(status.profile)); } catch { /* optional history cache */ }
    }
    $("agentConsole").textContent = `${text("consoleHint")} · ${text(`console-${device.getStatus().console.kind}`)}`;
  }
  function maybeVerifyIdentity() {
    const status = device.getStatus();
    if (busy || bindingState?.verified || !status.connected || status.transport !== "ble" ||
        status.inputPending || status.console.kind !== "shell" || autoIdentitySession === status.sessionId) return;
    autoIdentitySession = status.sessionId;
    api.targetBinding("verify", { automatic: true }).catch(() => {
      // Missing IDs, mismatch, interrupted commands and login prompts never
      // authorize historical data, and are not automatically retried.
    });
  }
  function logsChanged() {
    if (observeTimer !== null) return;
    observeTimer = setTimeout(() => {
      observeTimer = null;
      device.observe();
      refreshConsole();
      refreshHistory();
      maybeVerifyIdentity();
    }, 100);
  }
  function clearConversation() {
    currentTask = null; recovery = null;
    device.reset();
    executionRows.clear();
    messages.replaceChildren();
  }
  function onEvent(event, expectedVersion) {
    if (version !== expectedVersion) return;
    if(event.type==='agent_start') $("agentSteer").disabled = $("agentFollowUp").disabled = false;
    if(event.type==='input_queue_changed') {
      $("agentQueue").textContent=event.items.map(item=>`${text(item.kind)}: ${item.text}`).join('\n');
      $("agentClearQueue").hidden=!event.items.length;
    }
    if(event.type==='queued_input_consumed') {
      addMessage('user',event.item.text);
      if(currentTask){currentTask.goal=(currentTask.goal+'\n'+event.item.text).slice(0,4000);persistTask();}
    }
    if (event.type === "message_start" && event.message.role === "assistant") answer = null;
    if (event.type === "message_update" && event.assistantMessageEvent.type === "text_delta") {
      answer ??= addMessage("assistant");
      renderAssistantMarkdown(answer, event.assistantMessageEvent.delta, { append: true });
      if (currentTask) { currentTask.summary = (currentTask.summary + event.assistantMessageEvent.delta).slice(-4000); scheduleTaskSave(); }
      messages.scrollTop = messages.scrollHeight;
    }
    if (event.type === "tool_execution_start") {
      const body = addMessage(event.toolName, text("working"));
      toolRows.set(event.toolCallId, body);
      if (["send_serial_input", "run_shell_command", "probe_device_profile", "probe_download_tools", "download_to_target"].includes(event.toolName)) activeInputRow = body;
    }
    if (event.type === "tool_execution_end") {
      let preview = event.result?.content?.filter((part) => part.type === "text").map((part) => part.text).join("\n") || "";
      if (!event.isError) {
        try {
          const value = JSON.parse(preview);
          if (event.toolName === "update_task_plan") {
            preview = formatPlan(value.steps);
            if (currentTask) { currentTask.plan = value.steps; persistTask(); refreshHistory(); }
          }
          if (event.toolName === "read_web_page") preview = [value.title, value.url, value.text?.slice(0, 1600),
            Number.isInteger(value.offset) && `${text('readRange')}: ${value.offset}–${value.nextOffset} / ${value.totalCharacters}`,
            value.matchFound === false && text('noMatches'),
            (value.truncated || value.text?.length > 1600) && text("truncated")].filter(Boolean).join("\n");
          if (event.toolName === "search_serial_log") preview = [
            `${text('readRange')}: ${value.start}–${value.cursor} / ${value.latestCursor}`,
            value.matches?.length ? value.matches.map(m=>m.excerpt).join('\n…\n') : text('noMatches'),
            (value.truncated || value.moreMatches) && text('truncated'),
          ].filter(Boolean).join('\n');
          if (event.toolName === "read_serial_log" || event.toolName === "wait_for_serial_output") preview = [
            value.waitStatus && text(`wait-${value.waitStatus}`), value.text || text("noOutput"),
          ].filter(Boolean).join("\n");
          if (event.toolName === "send_serial_input") preview = text("delivered");
          if (event.toolName === "get_device_status") preview = [text(value.connected ? "connected" : "disconnected"), value.device, value.transport?.toUpperCase(), value.uart, text(`console-${value.console?.kind || "unknown"}`)].filter(Boolean).join(" · ");
          if (["inspect_serial_execution", "monitor_serial_execution"].includes(event.toolName)) preview = [
            value.executionStatus === "completed" && `Exit code: ${value.exitCode} · ${text("verificationNeeded")}`,
            value.delivery === "sent" ? text("delivered") : text("deliveryUnknown"),
            text(`observation-${value.observation}`), value.waitStatus && text(`wait-${value.waitStatus}`), value.evidence?.slice(-1600),
            (value.evidenceTruncated || value.evidence?.length > 1600) && text("truncated"),
          ].filter(Boolean).join("\n");
        } catch { /* Tool errors can be plain text. */ }
      }
      const body = toolRows.get(event.toolCallId) || addMessage(event.toolName);
      if (!body.dataset.execution && !body.dataset.download) body.textContent = preview.slice(0, 2000) + (preview.length > 2000 ? "…" : "");
      toolRows.delete(event.toolCallId);
      if (["send_serial_input", "run_shell_command", "probe_device_profile", "probe_download_tools", "download_to_target"].includes(event.toolName)) activeInputRow = null;
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
    currentTask = {id:crypto.randomUUID(),deviceKey:deviceIdentity(device.getStatus()),goal:question,summary:"",status:"running",executions:[]};
    persistTask();
    const restored = recovery; recovery = null;
    setBusy(true);
    $("agentQuestion").value = "";
    const runVersion = version;
    const timer = setTimeout(() => stop("stopped"), 900000);
    try {
      // Only explicitly saved configuration is used for model requests.
      const nextFingerprint = JSON.stringify(config);
      if (!runner || fingerprint !== nextFingerprint) {
        runner = await createSerialAgent({ config, device,
          computerDownload: ({id,args,signal}) => {
            const body = toolRows.get(id) || addMessage("download_to_computer");
            body.textContent = ""; body.dataset.download = "computer";
            return requestComputerDownload({container:body,args,signal,lang:getLang()});
          },
          onEvent: (event) => onEvent(event, eventVersion) });
        fingerprint = nextFingerprint;
      }
      if (version !== runVersion) { runner?.abort(); runner = null; return; }
      eventVersion = runVersion;
      addMessage("user", question);
      const outcome = await runner.prompt(question, {recovery:restored});
      if (currentTask?.status === "running") currentTask.status = outcome.limitReached || currentTask.plan?.some(s=>s.status!=='completed') ? "interrupted" : "answered";
      if (outcome.limitReached && version === runVersion) addMessage("assistant", text("limit"));
    } catch (error) {
      if (currentTask) currentTask.status = "failed";
      if (version === runVersion) addMessage("assistant", `${text("error")}${error.message}`);
    } finally {
      persistTask();
      clearTimeout(timer);
      setBusy(false);
    }
  });
  $("agentQuestion").addEventListener("keydown", (event) => {
    if (event.isComposing || event.keyCode === 229) return;
    if (event.key === "Enter" && (event.ctrlKey || event.metaKey) && !event.altKey && !event.shiftKey) {
      event.preventDefault(); event.stopPropagation();
      if (!event.repeat) { if(busy) queueInput('steer'); else $("agentForm").requestSubmit(); }
    }
  });
  function queueInput(kind) {
    const input=$("agentQuestion");
    if(!input.value.trim()) return;
    try {
      if(!runner || !busy) throw new Error(text('stopped'));
      runner.enqueue(input.value,kind); input.value='';
    } catch(error) { $("agentStatus").textContent=error.message; }
  }
  $("agentSteer").addEventListener('click',()=>queueInput('steer'));
  $("agentFollowUp").addEventListener('click',()=>queueInput('followUp'));
  $("agentClearQueue").addEventListener('click',()=>runner?.clearQueue());
  // Capture before xterm so the focus shortcut never becomes target UART input.
  document.addEventListener("keydown", (event) => {
    if (event.isComposing || event.keyCode === 229 || event.altKey || !event.shiftKey || !(event.ctrlKey || event.metaKey) || event.code !== "KeyK") return;
    event.preventDefault(); event.stopImmediatePropagation();
    if (event.repeat) return;
    void (async () => {
      if (!opened) await openPanel();
      if (opened) { showModePicker(false); $("agentQuestion").focus(); }
    })();
  }, true);
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
    stop(undefined, { preserveConversation: false });
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
  const api = {
    async targetBinding(action, { automatic = false } = {}) {
      if (busy) throw new Error("Agent is running; stop it first.");
      const initial = getStatus();
      if (!initial.connected || initial.transport !== "ble") throw new Error("Connect Bee over BLE to manage its Flash binding.");
      const session = initial.sessionId;
      let identityRevision = null;
      const check = () => { bindingController?.signal.throwIfAborted(); if (getStatus().sessionId !== session || !getStatus().connected) throw new Error("Connection changed; verify binding again."); if (identityRevision !== null && getStatus().inputRevision !== identityRevision) throw new Error("Terminal input changed; verify target identity again."); };
      stop(undefined, {preserveConversation:false});
      currentTask = null; device.forgetProfile();
      const controller = bindingController = new AbortController();
      const timer = setTimeout(() => controller.abort(), 35000);
      setBusy(true);
      try {
        const savedId = bindingReply(await bindingControl("@linkr target?"));
        check(); bindingState = {targetId:savedId,verified:false}; rememberedProfile = null;
        if (automatic && !savedId) return {status:"unbound",targetId:null};
        if (action === "clear") {
          const cleared = bindingReply(await bindingControl("@linkr target clear")); check();
          if (cleared !== null) throw new Error("Bee did not confirm removal.");
          bindingState = null;
          return {status:"cleared",targetId:null};
        }
        const marker = `LINKR_ID_${crypto.randomUUID().replaceAll("-", "")}`;
        const candidate = crypto.randomUUID();
        const command = targetIdentityCommand(action, candidate, marker);
        if (automatic && getStatus().inputRevision !== initial.inputRevision) throw new Error("Terminal changed before identity verification.");
        const record = await device.execute({text:command,appendEnter:true,trackExit:true}, controller.signal, {userApproved:true});
        const observed = await monitorSerialExecution({inspect:()=>device.inspectExecution(record.id),signal:controller.signal,check,timeoutMs:30000});
        if (observed.waitStatus === "awaiting-input" || observed.observation === "interrupted") {
          const error = new Error("Complete sudo in the terminal, then bind again to verify the ID.");
          error.code = "TARGET_SUDO_INPUT";
          throw error;
        }
        const targetPath = observedTargetPath(observed, marker);
        const targetId = observedTargetId(observed, marker); identityRevision = observed.sentRevision; check();
        if (action === "regenerate" && targetId !== candidate) throw new Error("Target did not confirm the newly generated UUID; Bee binding was not changed.");
        if (action === "verify" && savedId !== targetId) {
          bindingState = {targetId:savedId,observedId:targetId,verified:false};
          return {...bindingState,status:"mismatch",targetPath};
        }
        if (action !== "verify") {
          const persisted = bindingReply(await bindingControl(`@linkr target=${targetId}`)); check();
          if (persisted !== targetId) throw new Error("Target ID exists, but Bee Flash save was not confirmed. Verify before retrying.");
        }
        bindingState = {targetId,verified:true};
        try {
          const profile = JSON.parse(localStorage.getItem(`linkr-target-profile:${targetId}`));
          rememberedProfile = profile ? {profile,historical:true} : null;
        } catch { rememberedProfile = null; }
        return {...bindingState,status:"verified",targetPath};
      } finally { bindingController = null; clearTimeout(timer); setBusy(false); refreshHistory(); }
    },
    settingsChanged() { stop(undefined, { preserveConversation: false }); },
    refreshLang,
    logsChanged,
    syncLayout,
    isOpen: () => opened,
    isBusy: () => busy,
    hasInputFocus: () => opened && dialog.contains(document.activeElement) && document.activeElement.matches("input:not([type=radio]), textarea"),
    logsCleared() { stop("stopped", { preserveConversation: false }); clearConversation(); refreshConsole(); },
    connectionChanged(connected) {
      bindingState = null; rememberedProfile = null;
      autoIdentitySession = null;
      stop("changed", { preserveConversation: false });
      if (connected) clearConversation();
      else device.observe();
      refreshConsole(); refreshHistory();
      if (connected) logsChanged();
    },
  };
  return api;
}
