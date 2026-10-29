/*
 * Linux Chromium does not expose Web Bluetooth by default. When the API is
 * missing (or the page is not a secure context), show an actionable guide
 * instead of leaving the disabled Connect button unexplained.
 *
 * This is intentionally self-contained: it only runs in the web shell and
 * never touches the native Android/HarmonyOS transport.
 */
const nativeTransport =
  Boolean(globalThis.LinkrNativeBle) ||
  globalThis.LinkrBleTransport?.platform?.() !== "web";
const mobileLike = /Android|HarmonyOS|iPhone|iPad/i.test(navigator.userAgent);
const linuxDesktop = /Linux/i.test(navigator.userAgent) && !mobileLike;

if (!nativeTransport && !mobileLike && linuxDesktop && (!navigator.bluetooth || !window.isSecureContext)) {
  mountGuidance();
}

function mountGuidance() {
  const connection = document.querySelector('[data-settings-page="connection"]');
  const transport = connection?.querySelector(".transport-switch");
  if (!connection || !transport) return;

  let dialog = null;
  const trigger = document.createElement("div");
  trigger.className = "ble-guidance";
  trigger.setAttribute("role", "note");
  trigger.innerHTML = `
    <span class="ble-guidance-mark" aria-hidden="true">!</span>
    <span class="ble-guidance-text"></span>
    <button class="btn btn-soft ble-guidance-open" type="button"></button>`;
  transport.insertAdjacentElement("afterend", trigger);

  const triggerText = trigger.querySelector(".ble-guidance-text");
  const openButton = trigger.querySelector(".ble-guidance-open");

  function refreshTrigger() {
    const secure = window.isSecureContext;
    const zh = isChinese();
    triggerText.textContent = secure
      ? (zh
        ? "当前 Linux Chromium 还没有开放 Web Bluetooth。"
        : "Web Bluetooth is not enabled in this Linux Chromium.")
      : (zh
        ? "当前页面不是安全上下文，Web Bluetooth 已被浏览器禁用。"
        : "This page is not a secure context, so Web Bluetooth is disabled.");
    openButton.textContent = zh ? "查看修复步骤" : "How to fix";
    trigger.hidden = !bleModeActive();
  }

  function bleModeActive() {
    const button = document.getElementById("bleModeBtn");
    return !button || button.getAttribute("aria-pressed") === "true";
  }

  function buildDialog() {
    dialog = document.createElement("dialog");
    dialog.className = "ble-guidance-dialog";
    dialog.setAttribute("aria-labelledby", "bleGuidanceTitle");
    document.body.append(dialog);

    dialog.addEventListener("click", async (event) => {
      const copy = event.target.closest("[data-copy]");
      if (copy) {
        try {
          await navigator.clipboard.writeText(copy.dataset.copy);
          setStatus((isChinese() ? "已复制：" : "Copied: ") + copy.dataset.copy);
        } catch {
          setStatus(isChinese() ? "复制失败，请手动选择文本。" : "Copy failed; select the text manually.", true);
        }
        return;
      }
      if (event.target.closest("[data-close]")) {
        dialog.close();
        return;
      }
      if (event.target.closest("[data-lan]")) {
        dialog.close();
        document.getElementById("lanModeBtn")?.click();
        setTimeout(() => document.getElementById("wsHostInput")?.focus(), 0);
      }
    });
    dialog.addEventListener("keydown", (event) => {
      if (event.key === "Escape") dialog.close();
    });
    return dialog;
  }

  function renderDialog() {
    if (!dialog) buildDialog();
    const zh = isChinese();
    const secure = window.isSecureContext;
    const lead = secure
      ? (zh
        ? "Linux Chromium 默认关闭 Web Bluetooth。按顺序检查以下设置，完成后刷新本页。"
        : "Linux Chromium disables Web Bluetooth by default. Check these in order, then reload this page.")
      : (zh
        ? "当前地址不是安全上下文，Web Bluetooth 会被浏览器禁用。先换成 localhost 或 HTTPS，再检查下面的 Chromium 开关。"
        : "This address is not a secure context, so Web Bluetooth is blocked by the browser. Open it via localhost or HTTPS, then check the Chromium flags below.");
    dialog.innerHTML = `
      <div class="ble-guidance-dialog-head">
        <h2 id="bleGuidanceTitle">${zh ? "启用 Web Bluetooth" : "Enable Web Bluetooth"}</h2>
        <button class="icon-btn ble-guidance-close" type="button" data-close aria-label="${zh ? "关闭" : "Close"}">×</button>
      </div>
      <p class="ble-guidance-lead">${lead}</p>
      <ol class="ble-guidance-steps">
        <li>
          <strong>${zh ? "打开 Chromium 的 Web Bluetooth 开关" : "Turn on Chromium's Web Bluetooth flags"}</strong>
          <p>${zh ? "在地址栏打开下面的地址，把 Web Bluetooth 设为 Enabled，然后完全重启 Chromium。" : "Open the address below, set Web Bluetooth to Enabled, then fully restart Chromium."}</p>
          <div class="ble-guidance-copy-row">
            <code>chrome://flags/#enable-web-bluetooth</code>
            <button class="btn btn-soft ble-guidance-copy" type="button" data-copy="chrome://flags/#enable-web-bluetooth">${zh ? "复制" : "Copy"}</button>
          </div>
          <p class="ble-guidance-hint">${zh ? "建议同时启用：#enable-web-bluetooth-new-permissions-backend、#enable-web-bluetooth-confirm-pairing-support。" : "Recommended: also enable #enable-web-bluetooth-new-permissions-backend and #enable-web-bluetooth-confirm-pairing-support."}</p>
          <div class="ble-guidance-copy-row">
            <code>#enable-web-bluetooth-new-permissions-backend</code>
            <button class="btn btn-soft ble-guidance-copy" type="button" data-copy="chrome://flags/#enable-web-bluetooth-new-permissions-backend">${zh ? "复制" : "Copy"}</button>
          </div>
          <div class="ble-guidance-copy-row">
            <code>#enable-web-bluetooth-confirm-pairing-support</code>
            <button class="btn btn-soft ble-guidance-copy" type="button" data-copy="chrome://flags/#enable-web-bluetooth-confirm-pairing-support">${zh ? "复制" : "Copy"}</button>
          </div>
        </li>
        <li>
          <strong>${zh ? "用 localhost 或 HTTPS 打开本页" : "Open this page via localhost or HTTPS"}</strong>
          <p>${zh ? "Web Bluetooth 只允许安全上下文；http://局域网IP 会被禁用。若页面在另一台机器上，可用 SSH 端口转发到本机 localhost。" : "Web Bluetooth only works in a secure context; plain HTTP on a LAN IP is blocked. If the page runs on another machine, forward its port to localhost via SSH."}</p>
          <div class="ble-guidance-copy-row">
            <code>ssh -L 8090:127.0.0.1:8090 user@host</code>
            <button class="btn btn-soft ble-guidance-copy" type="button" data-copy="ssh -L 8090:127.0.0.1:8090 user@host">${zh ? "复制" : "Copy"}</button>
          </div>
        </li>
        <li>
          <strong>${zh ? "确认 BlueZ 和适配器可用" : "Confirm BlueZ and the adapter"}</strong>
          <p>${zh ? "系统需要运行 BlueZ，且 Chromium 能看到蓝牙适配器。打开 chrome://bluetooth-internals 检查；Snap/Flatpak 版 Chromium 可能无法访问 BlueZ。" : "BlueZ must be running and Chromium must see the adapter. Check chrome://bluetooth-internals; Snap/Flatpak Chromium builds may be unable to reach BlueZ."}</p>
        </li>
      </ol>
      <p class="ble-guidance-fallback">${zh ? "暂时不想改浏览器？改用 LAN 模式：先在手机 App 配置板子的 WiFi，再在本页用 WebSocket 连接。" : "Do not want to change the browser? Use LAN mode: configure the board's WiFi from the phone app, then connect over WebSocket here."}</p>
      <div class="ble-guidance-actions">
        <button class="btn btn-soft" type="button" data-lan>${zh ? "改用 LAN 模式" : "Use LAN mode"}</button>
        <button class="btn btn-primary" type="button" data-close>${zh ? "关闭" : "Close"}</button>
      </div>
      <p class="ble-guidance-status field-hint" role="status" aria-live="polite"></p>`;
    const lanButton = document.getElementById("lanModeBtn");
    dialog.querySelector("[data-lan]").hidden = !lanButton || lanButton.disabled;
  }

  function setStatus(message, isError = false) {
    const status = dialog?.querySelector(".ble-guidance-status");
    if (!status) return;
    status.textContent = message;
    status.classList.toggle("field-error", isError);
  }

  openButton.addEventListener("click", () => {
    renderDialog();
    if (typeof dialog.showModal === "function") dialog.showModal();
    else dialog.setAttribute("open", "");
  });
  for (const id of ["bleModeBtn", "lanModeBtn", "langButton", "drawerLangButton"]) {
    document.getElementById(id)?.addEventListener("click", () => setTimeout(refreshTrigger, 0));
  }
  refreshTrigger();
}

function isChinese() {
  return String(document.documentElement.lang || localStorage.getItem("linkr-lang") || navigator.language || "")
    .toLowerCase()
    .startsWith("zh");
}
