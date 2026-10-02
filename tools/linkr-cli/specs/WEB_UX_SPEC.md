<subagent sessionID="ses_f053023adffejUZd80zrwhCxiP" state="completed">
All source files re-verified. Below is the complete inventory.

---

# Linkr Bee Web Terminal — Complete UX + Feature Inventory

Source of truth: `/home/rock/Documents/linkr-bee/web/index.html`, `/home/rock/Documents/linkr-bee/web/app.js`, `/home/rock/Documents/linkr-bee/web/*.js`, `/home/rock/Documents/linkr-bee/docs/DEVELOPMENT.md`.
All strings are given as exact literals. `en` = English, `zh` = Chinese (both shipped in every string table).

---

## 1. UI Surface Inventory

### 1.1 Page shell

| Item | Value |
|---|---|
| `<html>` defaults | `lang="zh-CN" data-theme="dark"`, `<title>Linkr Bee Terminal</title>` |
| theme-color meta | `id="themeColorMeta"`; dark `#090d14`, light `#eef2f7` |
| CSS | `./style.css?v=20260928-connect-entry`, `./window.css?v=20260906-agent-sidebar-2`, `./vendor/xterm/xterm.css` |
| Scripts | `./native-bootstrap.js` (module), `./vendor/xterm/xterm.js`, `./vendor/addon-fit/addon-fit.js`, `./ble_transport.js?v=20260901-mobile1`, `./app.js?v=20260910-agent-task-plan` |
| Root grid | `main.app-shell` → `header.topbar` + `aside#controlsPanel.controls.window-surface` + `section.workpanel` + `div#drawerBackdrop.drawer-backdrop` |
| Toast host | `div#toastHost.toast-host[aria-live=polite]` |

**Boot notice (inline, dependency-free script):** `#bootNotice` / `#bootNoticeText` / `#bootNoticeReload`, `graceMs = 6000`, poll 200 ms (1000 ms after deadline), ready test `.terminal-output .xterm`.
- en: `The terminal did not start: something failed to load with the page. Reloading usually fixes it; if it keeps happening, check the network or a proxy.` / button `Reload`
- zh: `终端没有启动：页面加载时有文件没能取到。刷新一次通常就好；如果反复出现，检查网络或代理。` / button `重新加载`

### 1.2 Topbar

| id | Element | Notes / default |
|---|---|---|
| `#supportText` | `.support-text` | default `Checking Web Bluetooth…`; values: `Chrome/Chromium Web Bluetooth over HTTPS or localhost`, `Native Bluetooth LE transport`, `Web Bluetooth unavailable in this browser`, `LAN mode: reach the device's WebSocket over your network`, `Bundled xterm.js failed to load; check the web assets` |
| `#mobileConnectBtn` | `.btn.btn-primary.mobile-connect` | spinner + dual icons `.connect-icon-ble`/`.connect-icon-lan` + `.btn-label` `Connect` |
| `#panelToggle` | `.icon-btn` | aria `Toggle control panel`, controls `#controlsPanel`, `aria-expanded="true"` |
| `#themeButton` | `.icon-btn` | `.btn-label` shows `Light` (dark theme) / `Dark` (light theme) |
| `#langButton` | `.icon-btn` | `.btn-label` shows `中` when lang=en, `EN` when lang=zh |
| `#statusDot` | `.status-dot` | gets `.connected` |
| `#statusText` | | `Disconnected` / `Connected` (i18n keys `disconnected`/`connected`) |
| `#deviceName` | `.device-name` | BLE: device name or id; LAN: WS host |

### 1.3 Settings panel (`aside#controlsPanel`)

- `data-settings-active="connection"`; drawer head: `#drawerThemeButton`, `#drawerLangButton`, `#drawerClose` (aria `Close control panel`).
- Tabs `nav.settings-tabs` `aria-label="Settings sections"`:

| `data-settings-target` | Label en / zh |
|---|---|
| `connection` | `Connect` / `连接` |
| `terminal` | `Serial` / `串口` |
| `network` | `Network` / `网络` |
| `ai` | `AI` (hidden until agent available) |

Cards carry `data-settings-page="connection|terminal|network|ai"`; active tab id persisted as `linkr-settings-page`.

**Card: Connection** (`aria-label="Connection controls"`)
- Transport group `aria-label="Transport"`: `#bleModeBtn` `BLE`/`BLE` (pressed), `#lanModeBtn` `LAN` / `局域网`.
- `#wsHostField` (hidden unless LAN): label `Device address`/`设备地址`, `#wsHostInput` placeholder `192.168.1.50 or ws://host/ws` (static HTML default `192.168.1.50`), error `#wsHostError[role=alert]`.
- `#wsTokenField` (hidden unless LAN): label `Access token`/`访问令牌`, `#wsTokenInput` placeholder `32 hex characters`, hint (`wsTokenHint`): `Captured automatically while connected over BLE (from @s?). Leave blank only if LAN auth was disabled with @s token off.`, error `#wsTokenError`.
- `#blePairingHint`: `New host: hold Bee GPIO1 to GND before connecting and accept system pairing. Bonded hosts reconnect without GPIO1.`
- Buttons `.btn-grid`: `#connectButton` (`Connect`; becomes `Reconnect` when BLE device remembered, `Connecting…` while in flight), `#disconnectButton` (`Disconnect`, disabled), `#switchDeviceButton` (`Switch device`, BLE only), `#queryButton` (`Query UART`), `#clearButton` (`Clear`), `#saveButton` (`Save Log`).

**Card: Device Diagnostics** — `<details id="diagnosticsPanel">`, summary `Device Diagnostics`/`设备诊断`; grid `#diagnosticsGrid[aria-live=polite]` with items (`<span>` label, `<strong>` value, default `–`):

| id | label en / zh | rendered value |
|---|---|---|
| `#diagFirmware` | `Firmware`/`固件` | `<version> · Z<zephyr>` or `—` |
| `#diagUptime` | `Uptime`/`运行时间` | `1d 2h` \| `2h 30m` \| `12m 5s` |
| `#diagOwner` | `BLE access`/`BLE 访问` | `open`/`开放` or `scoped`/`受限` + ` · link L<level>`/`链路 L<level>` |
| `#diagUart` | `UART Buffer`/`UART 缓冲` | `<buffer> · drop <n>` |
| `#diagWifi` | `WiFi`/`WiFi` | `<state> · IP <ip> · err <n>` |
| `#diagUpload` | `Upload Queue`/`上传队列` | `<queue> B · HTTP <n> · fail <n>` |

Refresh button `#diagnosticsButton` `Refresh diagnostics`/`刷新诊断` (disabled unless connected over BLE).

**Card: Terminal Settings** (`aria-label="Terminal settings"`)
- UART: `#uartInput` `type=text` **default `115200,8,n,1,n`**, `#setUartButton` `Set`/`设置`.
- Enter key: `#enterSelect` options `raw`(`Raw`/`原始`), `cr`(`CR`), `lf`(`LF`), `crlf`(`CRLF`) — default `raw`.
- Chunk size: `#chunkInput` `type=number min=1 max=244 value=20`.
- Terminal font: `#fontSelect` options `system`(System monospace), `symbols`(System + Nerd Symbols), `jetbrains`(JetBrains Mono Nerd Font), `meslo`(MesloLGS Nerd Font), `firacode`(FiraCode Nerd Font); hint `#fontHint` `Nerd Font options use fonts installed on this device.`; preview `#fontPreview` ` main   󰘧 ~/project`.
- Checks: `#localEchoInput` `Local echo`/`本地回显`, `#debugInput` `Debug I/O`/`调试 I/O` (both unchecked).

**Card: Cheat Sheet** (`aria-label="Command cheat sheet"`)
- `<details class="ref" open>` summary `Linux Commands`/`Linux 命令` → `#cheatList`.
- `<details class="ref">` summary `Shortcuts`/`常用快捷键` → `#shortcutList`.
- Hint `Tip: click a command to send it when connected.`

**Card: WiFi & WebDAV** (`aria-label="WiFi and WebDAV settings"`)
- `#wifiConnectedSummary[hidden]`: `Connected network`/`当前网络` → `#wifiConnectedSsid`, `Device IP`/`设备 IP` → `#wifiDeviceIp` (falls back to `Obtaining address…`/`正在获取地址…`).
- Form (hidden while connected): `#wifiSsidInput` (`Network name`, placeholder `SSID`, `list="wifiSsidList"` datalist), `#wifiPasswordInput` (`Password`, placeholder `Leave blank for an open network`, `type=password`), `#wifiPasswordToggle` (aria `Show password`/`Hide password`), `#wifiNetworkList[aria-live=polite]` (buttons `.wifi-network[data-ssid]` with `.wifi-network-name` + `.wifi-network-rssi` `N dBm`), `#wifiFeedback[role=status]` default `Scan to select a nearby 2.4 GHz network.`
- Actions: `#wifiScanButton` `Scan`, `#wifiSetButton` `Connect WiFi`, `#wifiOffButton` `Off`, `#wifiQueryButton` `WiFi status`.
- WebDAV: `#webdavInput` `type=url` placeholder `http://host/path/ (anonymous)`, `#webdavSetButton` `Set`; `#webdavOffButton` `Off`, `#webdavQueryButton` `WebDAV status`.

**Card: AI configuration** (`section#agentSettings`, `data-settings-page="ai"`, hidden until agent available)

| id | label en / zh | attrs / placeholder |
|---|---|---|
| `#agentEndpoint` | `API base URL`/`API 地址` | `type=url`, placeholder `https://api.example.com/v1`, hint `Chat Completions compatible, e.g. https://api.example.com/v1` |
| `#agentModel` | `Model ID`/`模型名称` | placeholder `model-id` |
| `#agentApiKey` | `API key`/`API Key` | `type=password`, hint `Leave blank for a local service that does not require a key.` |
| `#agentHeaders` | `Extra request headers`/`附加请求头` | `rows=2`, placeholder `anthropic-dangerous-direct-browser-access: true` |
| `#agentProvider` | `API protocol`/`接口协议` | select, hint `Must match the API base URL; each protocol uses a different request format.` |
| `#agentReasoning` | `Reasoning effort`/`推理强度` | select, hint `Only some models support this; off avoids the extra latency and tokens.` |
| `#agentContextWindow` | `Context window in tokens (optional)` | `min=0 max=2000000 step=1 placeholder=0` |
| `#agentMaxTokens` | `Max output tokens (optional)` | `min=0 max=100000 step=1 placeholder=0` |
| `#agentPriceInput`/`#agentPriceOutput` | `Token prices per 1M (optional)` | `type=number min=0 step=0.01`, placeholders `input` / `output` |
| `#agentSettingsSave` | `Save configuration` (`type=submit`) | |
| `#agentSettingsClear` | `Clear configuration` | |
| `#agentSettingsStatus` | `role=status aria-live=polite` | |
| footer hints | `storage`, `notice` | see §7.4 |

Target binding sub-section: `h3#targetBindingTitle` (`Target identity binding`/`目标机绑定`), hint paragraph, `#targetBindingIdentity`, buttons `#targetVerify` (`Verify binding`), `#targetBind` (`Bind current target`), `#targetUnbind` (`Unbind Bee`), checkbox `#targetRegenerateConfirm`, `#targetRegenerate` (`Regenerate and bind`, disabled until checkbox), `#targetBindingStatus[role=status]`.

### 1.4 Work panel / terminal

- `section.workpanel` → `div#terminalWorkspace.terminal-workspace` → `section#terminalCard.terminal-card.window-surface` `aria-label="Interactive serial terminal"`.
- Header `.terminal-head`: title `Serial Terminal`/`串口终端` + `.term-led`; tools:

| id | visible label | aria/title |
|---|---|---|
| `#agentButton` | `AI` (hidden until agent available) | `Serial assistant`; toggles `aria-pressed` |
| `#zoomOutBtn` | `A−` | `Decrease font size` |
| `#zoomInBtn` | `A+` | `Increase font size` |
| `#fullscreenBtn` | expand/collapse icons | `Fullscreen terminal` / `Exit fullscreen`, `aria-pressed` |
| `#autoscrollBtn` | `⤓` | `Auto-scroll`, default `aria-pressed="true"` |
| `#copyBtn` | `⧉` | `Copy selection` |
| | `.term-hint` `xterm.js` | |

- `#terminalOutput.terminal-output[role=application]` (xterm mount), `aria-describedby="terminalInputHint"`.
- Input bar: `#terminalInputHint` default `Connect a device to enable terminal input`; when connected → `Modifiers apply once · Swipe right-hand keys for more`.
- `#terminalKeyBar[role=group]` aria `Terminal keys · swipe for more`, five groups:

| group | buttons (`data-terminal-key` → label) |
|---|---|
| `.terminal-modifiers` | `#focusTerminalButton` (keyboard icon, aria `Keyboard`), `Escape`→`Esc`, `Tab`→`Tab`, `#shiftKeyButton` `Shift` (`data-modifier=shift`), `#controlKeyButton` `Ctrl`, `#altKeyButton` `Alt` (all `aria-pressed="false"`) |
| `.terminal-navigation` | `Home`, `ArrowUp`→`↑` (aria `Up arrow · previous command`), `End`, `ArrowLeft`→`←`, `ArrowDown`→`↓` (aria `Down arrow · next command`), `ArrowRight`→`→` |
| `.terminal-editing` | `#pasteTerminalButton` `Paste`, `PageUp`→`PgUp`, `Enter`, `Ctrl-C` (id `#breakButton`), `PageDown`→`PgDn`, `Backspace`→`⌫` |
| `.terminal-symbols` | `/`, `\|`, `~`, `\`, `` ` ``, `-` |
| `.terminal-shortcuts` | `Ctrl-D`, `Ctrl-Z`, `Ctrl-L`, `Ctrl-A`, `Ctrl-E`, `Ctrl-R` |
| `.terminal-key-pair` | `Ctrl-U`, `Delete`→`Del` |

- Modifier feedback strings: `modifierNext` = `{key}: combine with the next key; tap again to cancel`; `modifierActive` = `{key} active · next key only · tap again to cancel`.
- `#presetChips.presets[aria-label="Quick send"]` — see §4.
- `#drawerBackdrop`.
- Status bar `.statusbar[aria-label="Status"]`: `#rxCount` (`0`) + `RX`/`收`, `#txCount` + `TX`/`发`, `Baud`/`波特率` + `#baudLabel` default `115200`, `#connStateText` default `Disconnected`.

### 1.5 xterm configuration

```js
{ cursorBlink: true, fontFamily: <stack>, fontSize: state.fontSize, lineHeight: 1,
  scrollback: 10000, smoothScrollDuration: 0, tabStopWidth: 8, theme }
```
- fontSize default **13** (11 when HarmonyOS/native host), zoom clamped **10–28** (`#zoomInBtn/#zoomOutBtn` ±1).
- Font stacks: system = `ui-monospace, SFMono-Regular, Menlo, Consolas, "Liberation Mono", monospace`; `symbols` prepends `"Symbols Nerd Font Mono", "Symbols Nerd Font"`; `jetbrains`/`meslo`/`firacode` prepend their own + symbols.
- Themes — dark: bg `#05080d`, fg `#d8f3dc`, brightBlack `#8d9bb0`, cursor `#d8f3dc`, selection `#2b5340`; light: bg `#f8fafc`, fg `#1f6b43`, brightBlack `#5c6a82`, cursor `#1f6b43`, selection `#bfe3cf`.
- Display pipeline: `WRITE_BACKLOG_LIMIT = 512 * 1024` (drop oldest half), `WRITE_FALLBACK_MS = 50`; overflow notice: `\x1b[93m[warn] N bytes of output dropped (terminal backlog)\x1b[0m\r\n`.
- Line prefixes (colored): dim `\x1b[90m`; `[error]`→`\x1b[91m`; `[warn]`/`[disconnected]`→`\x1b[93m`; `[ready]`→`\x1b[92m`.
- Log ring `LOG_CAP_BYTES = 4 * 1024 * 1024`; counters formatted with `toLocaleString()`.

### 1.6 Toasts / dialogs

- `toast(message, kind)` → max **3** visible (`TOAST_LIMIT = 3`), show 2200 ms, fade + removal after 250 ms; error class `toast toast-error`.
- Confirmation dialogs use native `window.confirm`; only preset `reboot` (`confirmReboot` = `Reboot the connected device now?` / `确定立即重启当前连接的设备吗？`) and AI plaintext-key save (`plaintextKeyConfirm`).

### 1.7 Responsive / layout behavior

- Drawer layout when `matchMedia("(pointer: coarse)")` matches **or** host platform is `harmonyos` **or** `max-width: 900px` (`mobileMedia`) → root class `settings-drawer-layout`; panel becomes `role="dialog"` (+`aria-modal` when open), topbar/workpanel get `inert`.
- Desktop collapse: root class `controls-hidden`, persisted as `linkr-sidebar` (`collapsed`|`open`).
- Drawer open/close: `.app-shell.sidebar-open` + `body.drawer-open`; focus moves to `#drawerClose` on open, back to `#panelToggle`/`#agentSettingsButton` on close; backdrop `#drawerBackdrop`; **swipe-right closes** when `dx > 72 && |dx| > 1.2·|dy|`.
- Soft keyboard: root class `soft-keyboard-open`, CSS vars `--terminal-viewport-height`, `--settings-viewport-top`; keyboard threshold `heightLoss > max(120, 0.2 * fullHeight)` and only on coarse/harmony hosts.
- Agent layout split: `workspace.dataset.agentLayout = "columns"` when `clientWidth >= 960` else `"rows"`; panel classes `agent-tight` (<260 px tall), `agent-narrow` (<520 px wide); root classes `agent-mode`, `agent-keyboard-chat`, `agent-keyboard-terminal`, CSS var `--agent-viewport-top`.
- Known breakpoints in `style.css`: 1100, 901–1100, 900, 600, 520, 380 px.

---

## 2. Key Bindings

### 2.1 Global (document-level)

| Key | Behavior |
|---|---|
| `Escape` (capture) | If drawer open → close drawer (preventDefault/stopPropagation). If terminal fallback fullscreen → exit it. Skipped when `event.isComposing` or `keyCode === 229`. |
| `Ctrl`/`Meta` + `Shift` + `K` (capture, `code === 'KeyK'`, no Alt) | Open agent panel if closed, close mode picker, focus `#agentQuestion`. Registered before xterm so it never reaches UART. Ignored on `event.repeat`. |
| `Escape` inside agent panel | Closes mode picker first, otherwise exits the panel. (`Escape` in xterm stays a UART key.) |

### 2.2 Assistant composer

| Key | Behavior |
|---|---|
| `Ctrl`/`Meta` + `Enter` (no Alt/Shift) | Submit (`agentForm.requestSubmit()`); while busy → queue as `steer`. |
| `Enter` | Newline in textarea. |
| Focus shortcut display | `⌘+Shift+K` / `Ctrl+Shift+K`; send `⌘+Enter` / `Ctrl+Enter` (platform via `/Mac\|iPhone\|iPad/` on `navigator.platform`). |
| `aria-keyshortcuts` | textarea: `Control+Shift+K Meta+Shift+K`; ask button: `Control+Enter Meta+Enter`. |
| Mode picker keys | `ArrowUp/ArrowLeft`, `ArrowDown/ArrowRight`, `Home`, `End` (preview only), `Enter`/`Space` (engage). |
| `pagehide` | Persists task + session. |
| `visibilitychange` (hidden) | If busy → `stop("stopped")`. |

### 2.3 Mode-picker / terminal interaction rules

- Terminal accessory keys and modifiers are enabled only while connected (`setConnected`).
- Modifier semantics: armed for **one** key press, then reset; labels `modifierNext`/`modifierActive`.
- `#terminalOutput` `pointerdown` → refocus xterm (if connected).
- `#terminalKeyBar` `mousedown` on a button → `preventDefault()` (keeps focus/IME on iOS).

### 2.4 Byte sequences produced by the key bar (`terminal_keys.js`)

- Cursor keys: `\x1b[A|B|C|D`, Home/End `\x1b[H|F` or `\x1bOH|OF` when application cursor mode; with modifiers `\x1b[1;<mod><A..D|H|F>` where `mod = 1 + shift(1) + alt(2) + ctrl(4)`.
- `Tab` → `\t`; Shift+Tab → `\x1b[Z`; `Escape` → `\x1b`; `Enter` → `\r`; `Backspace` → `\x7f`; `Delete` → `\x1b[3~`; `PageUp` → `\x1b[5~`; `PageDown` → `\x1b[6~`; modified PgUp/PgDn → `\x1b[5;<mod>~` / `\x1b[6;<mod>~`.
- `Ctrl-<A..Z>` → control byte (`controlInput`: `space`→`\x00`, `?`→`\x7f`, `@_` range masked with `&0x1f`).
- Symbol keys `/ | ~ \ ` -` pass through `applyInputModifiers` (shift uses the normal→shifted ASCII map `` `1234567890-=[]\;',./ `` → `~!@#$%^&*()_+{}|:"<>?`); Alt prefixes `\x1b`.

### 2.5 Cheat-sheet shortcut reference (rendered list, exact order)

`Ctrl+C` Interrupt task/中断当前任务 · `Ctrl+L` Clear screen/清屏 · `Ctrl+A` Start of line/光标到行首 · `Ctrl+E` End of line/光标到行尾 · `Ctrl+R` Search history/搜索历史命令 · `Ctrl+Z` Suspend task/挂起任务 · `Ctrl+D` Exit shell/退出终端 · `Ctrl+U` Clear whole line/删除整行 · `Tab` Autocomplete/自动补全 · `↑ / ↓` Command history/浏览历史命令.

---

## 3. Connection Flows

### 3.1 Constants

```js
NUS_SERVICE      = "6e400001-b5a3-f393-e0a9-e50e24dcca9e"
NUS_RX           = "6e400002-b5a3-f393-e0a9-e50e24dcca9e"
NUS_TX           = "6e400003-b5a3-f393-e0a9-e50e24dcca9e"
MGMT_SERVICE     = "4c4b0001-9a7e-4f4e-8b8a-3d6f12a0c001"
MGMT_PROTOCOL    = "4c4b0002-9a7e-4f4e-8b8a-3d6f12a0c001"
MGMT_DEVICE_ID   = "4c4b0003-9a7e-4f4e-8b8a-3d6f12a0c001"
MGMT_COMMAND     = "4c4b0004-9a7e-4f4e-8b8a-3d6f12a0c001"
MGMT_RESPONSE    = "4c4b0005-9a7e-4f4e-8b8a-3d6f12a0c001"
RELIABLE_UART_SERVICE = "4c4b0010-9a7e-4f4e-8b8a-3d6f12a0c001"
RELIABLE_UART_RX      = "4c4b0011-9a7e-4f4e-8b8a-3d6f12a0c001"
RELIABLE_UART_TX      = "4c4b0012-9a7e-4f4e-8b8a-3d6f12a0c001"
RELIABLE_UART_STATE   = "4c4b0013-9a7e-4f4e-8b8a-3d6f12a0c001"
BLE_MAX_ATT_VALUE = 244 · BLE_DEVICE_NAME_PREFIX = "Linkr BLE UART"
MGMT_CAP_WIFI=1<<0, WEBDAV=1<<1, WEBSOCKET=1<<2, DEVICE_ID=1<<3, ASYNC_EVENTS=1<<4, RELIABLE_UART=1<<5
MGMT_API_MAJOR = 1, MGMT_HEADER_SIZE = 12, RELIABLE_UART_HEADER_SIZE = 12
frame magics: MGMT "LK" (0x4c,0x4b), Reliable "LR" (0x4c,0x52)
WIFI_SCAN_CMD = "@w scan" · WIFI_SCAN_PREFIX = "@scan " · WIFI_SCAN_TIMEOUT_MS = 50000
WS_CONNECT_TIMEOUT_MS = 15000 · WS_HANDSHAKE_TIMEOUT_MS = 5000
WS_AUTH_NONE_FRAME = "@ws auth=none" · WS_AUTH_REQUIRED_FRAME = "@ws auth=required" · WS_AUTH_OK_FRAME = "@ws auth=ok"
WIFI_EVENT_FALLBACK_MS = 45000 · info collect timeout 8000 · mgmt response timeout 10000 (tracker)
```

### 3.2 Transport selection

- `#bleModeBtn`/`#lanModeBtn` only work while disconnected; persists `linkr-transport` = `ble`|`ws`.
- Switching clears WS errors, calls `setConnected(false)`, refreshes support text, and (LAN) pre-fills token via `lanTokens.selectHost(host)`.

### 3.3 BLE connect (`connectOnce`, mode `ble`)

1. `setConnecting(true)` → button label `Connecting…`, `.loading`, transport buttons disabled.
2. If no device and not `chooseDevice`: `bleTransport.requestDevice({ filters:[{services:[MGMT_SERVICE]}], optionalServices:[MGMT_SERVICE, NUS_SERVICE, RELIABLE_UART_SERVICE] })`; terminal lines `[scan] requesting Linkr Management v1 device`, `[pairing] <pairingHint>`, `[connect] <name|id>`; store id in `linkr-last-device-id`.
3. `bleTransport.connect(device.id, onDisconnected)` → `nusReady = true`.
4. Read `MGMT_PROTOCOL`: require `byteLength >= 10` and `byte[0] === MGMT_API_MAJOR`, else `Unsupported Linkr Management API version`; `mgmtMaxPayload = u16@2` (0 → `Invalid Linkr Management payload limit`); `mgmtCapabilities = u32@4`; require `MGMT_CAP_DEVICE_ID` (`Device does not advertise Device ID support`) and `MGMT_CAP_RELIABLE_UART` (`Device does not advertise Reliable UART support`).
5. Read `MGMT_DEVICE_ID` (must be 16 bytes → 32 lowercase hex) → `state.deviceId`; pre-fill LAN token via `lanTokens.selectDevice(deviceId)`.
6. Bump `writeGeneration`, retire agent panel, `agentJournal.reset()`, subscribe `MGMT_RESPONSE` (session-guarded).
7. Read `RELIABLE_UART_STATE`: 16 bytes, `byte[0] === 1`, `u16@2` payload limit ≠ 0, `u32@4` tx seq ≠ 0, `u32@8` rx seq ≠ 0; `reliableMaxPayload = clamp(limit, 1, 244-12)`; subscribe `RELIABLE_UART_TX`.
8. `setConnected(true, {preserveJournal:true})`; log `[ready] API v<major>.<minor> device=<id>`; then `requestDeviceState()` = `@i?` + `@w?` (if wifi cap) + `@s?` (if websocket cap — captures the LAN token).
9. Errors: log `[error] <msg>`; if a pairing was attempted also `[pairing] <pairingFailed>` + error toast; always `bleTransport.disconnect(device.id)`; if this was a *restored* device → remove `linkr-last-device-id`, log `[restore] <authorizedDeviceFailed>` + toast.

**Restore on load:** `restoreAuthorizedDevice()` — reads `getDevices([lastDeviceId])`, accepts id match or name prefix `Linkr BLE UART`, logs `[restore] <name>` + toast `Authorized device restored; click Connect to reconnect`; skipped if a session is already active.

### 3.4 LAN / WebSocket connect (`connectWs`)

1. Validate host (`Enter the device address first.`) and token (must be `^[0-9a-f]{32}$` → `The access token must be 32 lowercase hex characters.`); persist `linkr-ws-host`.
2. URL: use as-is if `^wss?://`, else `ws://<host>/ws`; log `[connect] <url>`; `binaryType = "arraybuffer"`.
3. 15 s connect timer → `WebSocket connection timed out after 15 seconds.`
4. On `open`: 5 s handshake timer → `The bridge did not confirm LAN access; check the token or firmware version.`
5. Handshake frames (text, only before `open`): `@ws auth=none` / `@ws auth=ok` → success; `@ws auth=required` → send the raw token (no token → `This bridge requires an access token. Read it with @s? over BLE.`); anything else → `[warn] ignored LAN frame before the access handshake`.
6. Success: `lanTokens.edit(token); lanTokens.save(); setConnected(true); appendLine("[ready]")`.
7. Close before open: if token was sent → `LAN access token rejected. Read the current token with @s? over BLE.` else `WebSocket {url} is unreachable.`; `onerror` → same unreachable message.
8. After open: `ArrayBuffer` frames → `handleIncomingBytes`; string frames → encoded to bytes. Close → `onDisconnected()`.

### 3.5 Disconnect

- WS: `state.ws.close()` (or immediate `onDisconnected()`).
- BLE: `disconnectInFlight` guard; stop notifications on `MGMT_RESPONSE` and `RELIABLE_UART_TX`; `bleTransport.disconnect()`; `onDisconnected()`.
- `onDisconnected()` resets: nus/mgmt/reliable flags, `reliableMaxPayload=20`, `reliableWriteSize=244`, reassemblers, pending mgmt responses (`Disconnected before management response`), `deviceId=""`, `mgmtCapabilities=0`, `mgmtMaxPayload=512`, `ws=null`, scanning/diagnostics/wifi state, rx decoder; logs `[disconnected]` only if a session existed.

### 3.6 `setConnected()` enable matrix

| Control | Enabled when |
|---|---|
| `#connectButton` | not in flight, not connected, `term` ready and (`ws` mode or BLE available) |
| `#mobileConnectBtn` | inverse: enabled while connected (acts as Disconnect) or when connect possible |
| `#disconnectButton` | connected |
| `#switchDeviceButton` | BLE, not in flight, not connected (hidden in LAN) |
| `#queryButton`, `#diagnosticsButton`, `#setUartButton` | connected **and** mode `ble` |
| terminal keys / modifiers / paste / break | connected |
| `#wifiSetButton`, `#wifiOffButton`, `#wifiScanButton` | BLE + `MGMT_CAP_WIFI` + `MGMT_CAP_ASYNC_EVENTS` |
| `#wifiQueryButton` | BLE + `MGMT_CAP_WIFI` |
| WebDAV set/off/query | BLE + `MGMT_CAP_WEBDAV` |
| preset chips + cheat-sheet buttons | connected |

Also on connect: `setAutoScroll(true,true)`, focus xterm, `serialInputRevision++`, `serialInputPending=false`, agent panel `connectionChanged()`, journal reset (unless `preserveJournal`), geometry sync reset.

### 3.7 Data-path details relevant to a client

- Writes chunk by `reliableMaxPayload` (reliable path) or `chunkSize()` = `clamp(#chunkInput, 1, 244)` (legacy NUS); ATT size-rejection auto-fallback to 20-byte chunks with `[warn] BLE write failed; retrying with 20-byte chunks`.
- Reliable frames: `LR`, ver 1, flags 0, `u32` sequence (wrap `0xffffffff → 1`), `u16` payload len, `u16` checksum; ATT write-size candidates `reliableWriteSize → 182 → 128 → 62 → 20`; sequence gap latches `Reliable UART sequence gap: expected X, got Y. Serial output is no longer delivered; disconnect and reconnect the device.`
- Management frames: `LK`, major 1, flags 1, `u32` request id (wrap), `u16` len, `u16` 0; first write must contain the full 12-byte header, chunk `max(20, chunkSize())`; commands longer than `mgmtMaxPayload` rejected.
- Serial session guards exposed to the assistant: `sessionId = writeGeneration`, `inputRevision`, `inputPending`, `uartWriteError`.
- Terminal geometry sync: 180 ms debounce, only with an idle shell prompt visible; command `stty rows <r> cols <c> >/dev/null 2>&1\r`; dims clamped 2–1000; restart detection via `login:` / `Linux version ` lines.

---

## 4. Quick-Send Presets

Container `#presetChips`, aria `Quick send`/`快捷发送命令`. All start `disabled`.

| `data-cmd` | extra attributes |
|---|---|
| `help` | — |
| `version` | — |
| `uname -a` | — |
| `df -h` | — |
| `reboot` | `data-confirm="confirmReboot"` `data-danger="true"` → `window.confirm("Reboot the connected device now?")` |

Click handler: confirm (if `data-confirm`) → `sendText(cmd)` which appends `\n` (normalized by Enter-key mode) and optionally echoes when local echo is on.

Related: cheat-sheet items `#cheatList .ref-item[data-cmd]` send on click (disabled when disconnected).

**Cheat-sheet groups (exact order, `c` = command, `d` = en/zh description):**

- `grpFiles` `Files & Dirs`/`文件与目录`: `ls -l` List in long format/详细列表 · `cd <dir>` Change directory/切换目录 · `pwd` Print working directory/显示当前路径 · `mkdir <dir>` Make directory/创建目录 · `cp -r a b` Copy recursively/递归复制 · `mv a b` Move / rename/移动或重命名 · `rm -rf <dir>` Force remove/强制删除 · `cat <file>` Show file content/查看文件内容 · `grep "x" <f>` Search text/搜索文本 · `find . -name "*.c"` Find files/查找文件
- `grpSys` `System`/`系统信息`: `uname -a` Kernel info/内核信息 · `df -h` Disk usage/磁盘使用 · `free -h` Memory usage/内存使用 · `top` Process monitor/进程监控 · `uptime` System uptime/运行时长
- `grpNet` `Network`/`网络`: `ip a` Network interfaces/网络接口 · `ping <host>` Ping a host/连通测试 · `ssh u@host` Remote login/远程登录 · `scp a u@h:` Secure copy/安全拷贝 · `curl -I <url>` Fetch headers/请求响应头
- `grpPerm` `Permissions & Processes`/`权限与进程`: `chmod 755 <f>` Change mode/修改权限 · `chown u:g <f>` Change owner/修改属主 · `ps aux` List processes/进程列表 · `kill -9 <pid>` Kill process/终止进程 · `sudo <cmd>` Run as root/提权执行

---

## 5. Diagnostics Panel — Commands & Response Parsing

### 5.1 Commands

| UI action | Command | Gate |
|---|---|---|
| Open `<details id="diagnosticsPanel">` while connected / `#diagnosticsButton` | `@i?` | BLE + mgmt ready |
| `#queryButton` (`Query UART`) | `@u?` | BLE |
| `#setUartButton` | `@u=<uartInput value>` | BLE |
| WiFi Set / Off / Query / Scan | `@w=<ssid>,<password>` / `@w off` / `@w?` / `@w scan` | caps as in §3.6 |
| WebDAV Set / Off / Query | `@d=<url>` / `@d off` / `@d?` | `MGMT_CAP_WEBDAV` |
| (auto, on connect) | `@s?` | `MGMT_CAP_WEBSOCKET` |

Assistant-side command constants (`accessory_control.js`): `DIAGNOSTICS_COMMAND="@i?"`, `UART_QUERY_COMMAND="@u?"`, `WIFI_QUERY_COMMAND="@w?"`, `WEBDAV_QUERY_COMMAND="@d?"`.

### 5.2 `@info` stream parsing

- Line handler: `@info <group> key=value …`; group = first token, each later token split at the **first** `=`; stored into `state.diagnostics[group]`; `@info done` → `renderDiagnostics()` + toast `Diagnostics updated`/`诊断信息已更新`.
- Assistant collection `collectAccessoryDiagnostics({signal, timeoutMs = 8000})` accumulates raw lines until `@info done` (timeout/abort resolves with `settled:false`), then `parseInfoGroups(lines)` merges per group.
- Sample (from `docs/DEVELOPMENT.md`):
```text
@info fw version=0.2.0 zephyr=4.4.1
@info sys uptime_ms=123456 owner=0 security=1
@info uart dropped=0 buffer=0/16384
@info wifi state=connected ip=ready error=0
@info upload state=on queue=0 dropped=0 http=201 failures=0 successes=4
@info done
```
- Rendering rules (§1.3 table) plus side effects: `wifi.state` updates `state.wifiStatus.connected/ip`; an empty `#wsHostInput` is auto-filled when `wifi.ip` looks like an IPv4 address.

### 5.3 Reply formats (shared parsers)

| Reply | Parser result |
|---|---|
| `OK uart=115200,8,N,1,none` | `parseUartSettings` → `{baud, dataBits, parity(lower), stopBits, flow(lower)}` |
| `OK wifi=connected,ssid=MyNet,ip=192.168.1.5` / `OK wifi off` | `parseWifiStatus` → `{state, ssid, ip}`; SSID `-` → `""`; IP taken from **last** `,ip=` |
| `OK webdav=on,url=http://host/dav/` / `OK webdav off` | `parseWebdavStatus` → `{state, url}` |
| any | `replyStatus(text)` → `{ok, error}` — first `^ERR` line wins, else `ok` if any `^OK` |
| `OK ws=… token=none\|<32hex>` | captured into `linkr-lan-tokens-v1` and `#wsTokenInput` |
| scan | `@scan result <ssid> [-N dBm] [ch=N] [open\|wep\|wpa\|wpa2\|wpa2-sha256\|wpa3\|eap\|wapi\|unknown]`, `@scan done`, `@scan error`, `^ERR` → finish |
| WiFi status line | `OK wifi=<state>,ssid=<ssid>[,ip=<ip>]`, `OK wifi off` |
| WiFi events | `parseWifiEvent` FINAL phases `ready`/`failed`/`off`, intermediate `queued`/`connecting`/`dhcp` |

WiFi feedback keys: `wifiPhaseQueued` `WiFi request queued…` · `wifiPhaseConnecting` `Connecting to {ssid}…` · `wifiPhaseDhcp` `Connected to {ssid}; obtaining address…` · `wifiPhaseOff` `Disconnecting WiFi…` · `wifiAwaitingAddress` `The bridge reports ready but has no IPv4 address yet; waiting…` · `wifiReady` `WiFi ready: {ssid}` · `wifiConnectFailed` `WiFi provisioning failed (result {code}). Check the password and 2.4 GHz coverage, then retry.` · `wifiEventTimeout` `The bridge sent no completion event; showing the polled status instead.` (+ `[warn] no WiFi completion event; falling back to status polling`).

Refresh cadence after WiFi change: **1500, 3500, 7000 ms** re-running `requestDeviceState()`.

### 5.4 Accessory change polling (assistant path)

`executeAccessoryChange` → send command, check `replyStatus`, then poll with `pollAccessoryStatus({timeoutMs, intervalMs})`:

| action | query | settle predicate | timing |
|---|---|---|---|
| `set-uart` | `@u?` | `@u=<…>` equals the wanted command | 6000 ms / 800 ms |
| `set-wifi` | `@w?` | `state === "connected"` (or `"off"` when off) | 20000 ms / 1500 ms |
| `set-webdav` | `@d?` | always (first parse) | 6000 ms / 1500 ms |

Evidence object: `{command: redactCommand(cmd), reply: redactSecrets(reply), accepted, confirmed/status, settled, applied}`. Redaction: `@w=` → `<ssid>,<redacted>`; `@d=` URL userinfo → `<redacted>`; `token=<32hex>` → `token=<redacted>` (also applied to terminal display/export).

### 5.5 Validation limits (`ACCESSORY_LIMITS`)

`minBaud 300`, `maxBaud 3000000`, `dataBits [5,6,7,8]`, `parity ["n","e","o"]`, `stopBits [1,2]`, `flow ["none","rtscts"]`, `ssidMax 32`, `passwordMax 64`, `webdavUrlMax 256`; SSID must not contain `,` or control chars; WebDAV URL must start `http://` or `https://` and contain no whitespace/control chars.

---

## 6. Log Actions (Save / WebDAV / Journal)

### 6.1 Save Log (`#saveButton` → `saveLog()`)

- Source: `state.logBytes` chunks (received bytes only, capped 4 MiB); if empty, falls back to `redactSecrets(terminalOutput.textContent)`.
- `Blob(channels, {type:"application/octet-stream"})`, download name **`linkr-ble-<ISO timestamp with [:.] → ->.log>`**, e.g. `linkr-ble-2026-10-02T10-11-12-345Z.log`; revoke URL after **60000 ms**; toast `Log saved`/`日志已保存`.

### 6.2 Clear (`#clearButton`)

`term.reset()`, empties `logBytes`/`logSize`, `agentJournal.reset()`, `agentPanel.logsCleared()`, toast `Screen cleared`/`已清屏`.

### 6.3 WebDAV upload (device-side, configured from the panel)

- `@d=<url>` (`Set`) / `@d off` / `@d?`; URL placeholder `http://host/path/ (anonymous)`; toast `WebDAV configured`/`WebDAV 已配置`.
- Firmware PUTs UART RX bytes to `<webdav_url>log-<boot-id>-<sequence>-<uptime>.txt`; anonymous HTTP only (Basic Auth rejected); counters appear in `@info upload`.

### 6.4 Serial journal (assistant evidence window)

- `SerialJournal`, capacity **128 × 1024 chars**, streaming `TextDecoder`, control/escape/OSC/CSI sequences masked to `\0` (only `\t \r \n` survive), surrogate-safe head eviction.
- `read({after, limit = 12000})` → `limit` clamped **1 … 16000**, returns `{text, start, cursor, latestCursor, truncated, updatedAt}`.
- Written on every incoming byte (`handleIncomingBytes`), reset on connect and on Clear; consumed by assistant tools (`read_serial_log`, `search_serial_log`, `wait_for_serial_output`, execution evidence).

### 6.5 Assistant report export (`#agentExport`)

- `buildTaskReport({lang, device, task, tasks, records, notes})` → `text/markdown` blob, filename **`linkr-agent-<ISO with [:.] → ->.md>`**, revoked after **1000 ms**.
- Enabled when any task, note, or execution record exists.

---

## 7. Assistant Panel UX

Toggle: `#agentButton` (`AI`), `aria-controls="agentPanel"`, `aria-pressed`; titles `Enter Agent mode`/`进入 Agent 模式`, `Exit Agent mode`/`退出 Agent 模式`, plus ` (Ctrl|⌘)+Shift+K`.

### 7.1 Structure (injected DOM, ids exact)

```
section#agentPanel.agent-panel.window-surface[role=region][aria-labelledby=agentTitle]
  header.agent-header
    strong#agentTitle "Agent"
    button#agentModeButton[aria-controls=agentModePicker][aria-expanded=false]
       .agent-gear-track (i,i,i,b) · .agent-gear-caption "Mode"/"档位"
       span#agentActiveMode[aria-live=polite] · span#agentModeCountdown.agent-mode-countdown[aria-hidden]
    .agent-actions: #agentSettingsButton "Settings" · #agentNew (＋ icon + "New chat") · #agentClose "Exit"
  fieldset#agentModePicker.agent-mode-picker[hidden][aria-label="Execution mode"]
    legend "Execution mode"
    .agent-mode-title strong "Change execution mode" + #agentModeClose (×)
    radios name="agentMode": value=manual ("M"), value=auto checked ("A"), value=full-auto ("F")
  p#agentConsole.agent-console            ← "Console hint · <kind>"
  details#agentHistory.agent-history
    summary "Tasks and device"
    p (taskNote) · p#agentProfile · div#agentNotes
    details#agentPolicy: summary "Command policy for this device",
        p(policyNote), #agentPolicyAsk textarea rows=2, #agentPolicyAllow textarea rows=2,
        #agentPolicySave "Save policy", #agentPolicyStatus
    .agent-actions: #agentProbe "Probe device" · #agentExport "Export report" · #agentForget "Clear records"
    div#agentTasks · p#agentStorageError[role=status]
  div#agentMessages.agent-messages[role=log][aria-live=polite]  (initial p.agent-empty)
  p#agentStatus.agent-status[role=status]
  p#agentUsage.agent-usage[role=status]
  div#agentQueueControls[hidden]: p(queueHelp), #agentSteer "Add to current task",
        #agentFollowUp "Queue next step", #agentQueue[role=status], #agentClearQueue "Clear pending"[hidden]
  form#agentForm
    label.agent-question span "Describe the problem"
      textarea#agentQuestion rows=1 maxlength=4000 required (placeholder "Describe the problem"/"描述你遇到的问题")
    .agent-actions: #agentStop "Stop"[disabled] · #agentAsk (↑ icon, title "Send (⌘/Ctrl+Enter)")
button#agentShowTerminal.agent-terminal-peek (hidden unless soft keyboard + composing in panel)
```

### 7.2 Execution modes

| value | label en/zh | help text (en) |
|---|---|---|
| `manual` | `Manual` / `手动` | `AI proposes commands; click Send to enter them on the target.` |
| `auto` (default, session-only) | `Auto · Recommended` / `Auto · 推荐` | `Low-risk queries run at a recognized shell prompt; other input needs approval. Destructive commands need approval in every mode.` |
| `full-auto` | `Full Auto` | `Commands run without confirmation. Recognized destructive or irreversible commands — recursive/forced deletes, disk and filesystem tools, dd, flashing and bootloader tools, downloaded content piped into a shell, privilege escalation, recursive permission changes — still need your approval. Detection does not cover every operation inside scripts or indirect execution.` |

- Changing mode while running: `stop("modeChanged")` → message `Mode changed; conversation retained. This run stopped and pending input was cancelled; sent input cannot be recalled. Ask again to continue.`
- Full Auto window `FULL_AUTO_WINDOW_MS = 15 * 60 * 1000`; countdown rendered `M:SS` in `#agentModeCountdown` (1 s tick); on expiry → `Full Auto reached its time limit and reverted to Auto; later commands need approval.`; on reconnect while armed → note `Device reconnected; execution mode changed from Full Auto to Auto. …`; new conversation/connection resets to `auto`.
- Mode picker geometry: width `min(328, viewportWidth - 16)`, min height 96, gap 8, never overlaps the composer.

### 7.3 Approval / policy model

- Destructive guard regexes (command-position scoped) cover: `rm -rf`-style, `find -delete/-exec rm`, `shred`, `mkfs*/fdisk/sfdisk/gdisk/parted/sgdisk/wipefs/blkdiscard`, `dd`, `flash_erase/nandwrite/ubiformat/mtd_debug/flashrom/fw_setenv`, `esptool/openocd/fastboot/rkdeveloptool/dfu-util/stm32flash/avrdude`, `sudo|doas|su`, recursive `chmod/chown/chgrp`, `chattr/setfacl`, plus pipelines `> /dev/(sd|mmcblk|nvme|mtdblock|loop|disk)`, `curl|wget|fetch … | sh`, `base64 -d … | sh`. **Recoverable actions (reboot, poweroff, mount, service restart) are deliberately unguarded.**
- Built-in low-risk query allowlist (exact wire text + exactly one `\r`/`\n`/`\r\n`): `pwd, whoami, id, uptime, date, uname, uname -a, uname -r, uname -m, ls, ls -l, ls -a, ls -la, ls -al, ls -lh, df, df -h, df -T, free, free -h, free -m, lsblk, lsblk -f, dmesg, dmesg -T, ip addr show, ip link show, ip route show, cat /proc/version, cat /proc/cpuinfo, cat /proc/meminfo, cat /proc/uptime, cat /proc/cmdline, cat /etc/os-release`.
- Per-device policy lists (`linkr-agent-command-policy-v1`): `alwaysAsk` (substring match, forces approval in **every** mode) and `allow` (exact match, skips approval only in `auto` when line is clean). 20 entries max/list, 200 chars/entry, no control characters; error texts: `A command policy entry is limited to 200 characters.`, `A command policy entry must not contain control characters.`, `A command policy list holds at most 20 entries.`, `This target has no identity yet, so a command policy cannot be stored.`
- Serial-input approval row: heading `Send to the current target? (Control characters are escaped.)`, buttons **`Send`** / **`Reject`**; rejected error `User rejected this input. Do not request it again.`
- Accessory approval card (§5): note `This changes Linkr Bee itself (not the target), sent over the encrypted Bluetooth management channel:` + summary + **`Allow change`** / **`Reject`** (zh `允许修改` / `拒绝`); reject error `The user rejected this change. Do not retry unless the user asks again.`
- Computer download card: **`Choose location and download`** / `选择保存位置并下载`; status lines `Destination: this computer / phone`, `This browser makes the network request above`, `SHA-256: <hash|No expected hash; compute only>`; progress `<progress>`; fallback link `Save checked file`; **128 MiB** cap (`Browser download limit is 128 MiB; use target download for larger files.` / `Browser download exceeds 128 MiB; use target download.`); SHA mismatch → `SHA-256 mismatch. Expected …; actual …. File was not offered for saving.`; `showSaveFilePicker` when available, otherwise blob + 60000 ms revoke.
- Execution record rows: heading `<mode> · <state>` or approval text; states `proposed|awaiting-approval|approved|sending|sent|denied|cancelled|failed`; labels `Approved/Sending/Sent/Denied/Cancelled/Send incomplete` (zh `已确认/发送中/已发送/已拒绝/已取消/发送未完成`); `Exit code: N · Command completed; verify the intended result`; observations `No subsequent output yet.` / `Subsequent output received; verify the result.` / `A shell prompt returned; the output still needs verification.` / `Connection or terminal input changed; …`; delivery `Input sent; command completion is not yet confirmed.` / `Delivery is uncertain; some input may have been sent. Inspect the terminal first.`; payload shown as `JSON.stringify(record.payload)` in a `<pre>` + `Copy` button; evidence `<details><summary>Execution log</summary>` last 4000 chars + `Output truncated.`; elapsed shown as `N.N s`.
- Console hint line: `Console hint · Shell prompt|Login prompt|Password prompt|Sudo password; enter in terminal|Awaiting confirmation|Pager awaiting input|Bootloader|Kernel panic|Unknown`.
- Stop message: `Stopped. Already sent input cannot be recalled; use Ctrl-C in the terminal to interrupt the target program.` (900000 ms watchdog per run).

### 7.4 AI settings behaviors

- Providers: `openai-completions` (`OpenAI-compatible API`), `anthropic-messages` (`Anthropic Messages API`), `google-generative-ai` (`Google Generative AI`) — default first.
- Reasoning levels: `off`, `low`, `medium`, `high` (labels `Off/Low/Medium/High`, zh `关闭/低/中/高`).
- Defaults: `AGENT_DEFAULT_CONTEXT_WINDOW = 32768`, `AGENT_DEFAULT_MAX_TOKENS = 4096`, `AGENT_FIXED_CONTEXT_TOKENS = 8000`, min useful window = `8000 + maxTokens + 512`; ranges context `[1000, 2000000]`, maxTokens `[1, 100000]` (0 = default).
- Header rules: max **8** lines, `Name: value`, name `^[A-Za-z0-9-]{1,64}$`, value ≤ 256 chars, no control chars.
- Validation messages: `Enter an HTTP(S) API base URL without credentials, query parameters or a fragment.` · `Enter a model ID.` · `Choose an API protocol.` · `Choose a reasoning effort.` · `Context window must be 0, or an integer between 1000 and 2000000.` · `Max output tokens must be 0, or an integer between 1 and 100000.` · `Prices must be numbers between 0 and 100000.` · `Invalid headers: use one \`Name: value\` per line, with names limited to letters, digits and hyphens.`
- Status strings: `Configuration saved on this device.` · `AI configuration cleared from this device.` · `Changes have not been saved.` · `Save failed. Check that app / browser storage is allowed, then retry.` · `Agent is running. Return to the conversation and stop it before editing configuration.` · tight-window warning `Window too small: the assistant's own prompt and tool descriptions already cost about 8000 tokens, and with the max output it needs at least <min>. …`
- Plaintext-key consent: loopback detection (`localhost`, `::1`, `*.localhost`, `127.*`); warning `Warning: this endpoint uses plain http and is not loopback, …`; confirm dialog `This endpoint uses plain http and is not loopback, so the API key will be sent in cleartext. Save anyway?`; cancel → `Save cancelled: a plaintext http endpoint needs confirmation before an API key is stored.`
- Footer hints: storage `Configuration and API key are saved in this device's app / browser storage across reloads and restarts. Clear the configuration or app / site data to remove them.`; notice `When you ask a question, the assistant sends the requested serial logs to your configured model service.`
- Endpoint-unreachable hint: `The model endpoint did not answer a browser request. Check that it is reachable; a direct browser call also requires the endpoint to allow this origin (CORS), and hosted providers usually need a proxy.`

### 7.5 Usage line

`Tokens this conversation <n> · ↑<in> ↓<out>[ ⚡<cacheRead>] · Estimated cost ~$<x>` (cost omitted → `Prices not set`); tokens via `toLocaleString("en-US")`; cost `≥0.01 → 2 dp`, `<0.01 → 4 dp`, `<1 → 3 dp`. Resets with `New chat`.

### 7.6 Tasks / notes / sessions

- Task rows: `<status> · <goal>`, plan rendering with statuses `Pending|In progress|Completed|Blocked` (zh `待执行|进行中|已完成|受阻`) prefixed by `Progress recorded by AI; verify against execution logs`, `Verify previous task` button prefills `Verify previous task: <goal>`.
- Task store: max **20** tasks, plan ≤ 8 steps, ≤ 8 execution summaries, text redacted + capped 2000 chars; statuses `running|answered|interrupted|failed|pending|in_progress|completed|blocked`.
- Notes: `Device notes (kept across reconnects)`, max **12**/device, 600 chars text, 200 chars evidence, `Delete` button, storage error `Cannot save tasks; check browser storage.`
- Session restore note: `Restored the previous conversation. The history is unverified: re-read the device state before acting.`
- Auto probe button pre-fills `Use probe_device_profile to inspect the current target and report the completed profile.` (zh variant provided).
- Tool row labels (zh / en): `probe_device_profile` 探测设备档案, `probe_tools` 检查所需工具, `update_task_plan` 任务计划与验证记录, `remember_target_note` 记录设备事实, `verify_target_file` 校验目标机文件, `verify_target_service` 校验目标机服务, `probe_download_tools` 探测目标机下载工具, `download_to_target` 下载到目标机, `download_to_computer` 下载到当前电脑 / 手机, `run_shell_command` 执行 Shell 命令, `monitor_serial_execution` 持续观察执行, `read_serial_log` 读取串口日志, `read_web_page` 读取网页, `search_serial_log` 查找串口日志, `get_device_status` 读取设备状态, `send_serial_input` 请求发送串口输入, `get_accessory_diagnostics` 读取配件诊断, `set_uart_config` 修改桥接串口参数, `wifi_scan` 扫描附近 WiFi, `set_wifi` 配置配件 WiFi, `set_webdav` 配置日志上传, `wait_for_serial_output` 等待串口输出, `inspect_serial_execution` 核查执行结果 (+ gated `read_target_file`, `watch_serial_output` on the mobile runtime).
- Verify verdicts: `Match`/`一致`, `Mismatch`/`不一致`, `Indeterminate`/`无法判定`, `Measured only, not verified`/`仅测量，未校验`.
- Target binding actions map to `#targetVerify|targetBind|targetUnbind|targetRegenerate` → `verify|bind|clear|regenerate`; 35000 ms abort timer; Bee commands `@linkr target?`, `@linkr target clear`, `@linkr target=<uuid>`; marker `LINKR_ID_<uuid-without-dashes>`; profile cache key `linkr-target-profile:<targetId>`; errors include `Connect Bee over BLE to manage its Flash binding.`, `TARGET_SUDO_INPUT`, `TARGET_PERMISSION_DENIED`.

---

## 8. serial_watch — Patterns & Presentation

**Engine (pure, no DOM):** `createSerialWatch({patterns, now, bootLoop, maxFindings})` with `feed(text, at)`, `feedBytes(bytes, at)`, `findings()`, `clear()`, `reset()`, `snapshot()` → `{findings, lines, bootCount, buffered, dropped}`; `describeFindings(findings, lang)`; `isBootLoopHint(state)`.

**Caps:** `MAX_LINE_CHARS 1024`, `MAX_FINDINGS 200` (default `maxFindings = 20`), `MAX_PATTERNS 64`, `MAX_BOOT_HISTORY 64`, `MAX_EVIDENCE 240`; boot-loop defaults `threshold 3`, `windowMs 120000`, clamps `1…1000` and `1…3600000`.

**Finding shape:** `{kind: "panic"|"boot"→"boot-loop"|"pattern", id, label, line, at, count, evidence}`; dedup by id (+message for panics) so repeats increment `count`/`at`.

**`PANIC_PATTERNS` (order matters, case-insensitive literal substring):**

| id | text | label en | zh label |
|---|---|---|---|
| `watchdog-bug` | `watchdog: BUG` | Watchdog reported a bug | 看门狗 BUG 报告 |
| `kernel-panic` | `Kernel panic` | Kernel panic message | 内核崩溃信息 |
| `oops` | `Oops:` | Kernel oops report | 内核 oops 报告 |
| `bug` | `BUG: ` | Kernel BUG warning | 内核 BUG 警告 |
| `oom` | `Out of memory` | Kernel out-of-memory report | 内核内存耗尽报告 |
| `oom-kill` | `oom-kill` | OOM killer invoked | OOM 终止进程 |
| `null-deref` | `Unable to handle kernel NULL pointer dereference` | Kernel NULL pointer dereference | 内核空指针解引用 |
| `segfault` | `segfault at` | Userspace segmentation fault | 用户态段错误 |
| `hardware-error` | `Hardware Error` | Hardware error report | 硬件错误报告 |
| `cpu-warning` | `WARNING: CPU` | Kernel CPU warning | 内核 CPU 警告 |

**`BOOT_PATTERNS`:** `u-boot` (`U-Boot `, Bootloader banner/引导程序启动横幅), `linux-version` (`Linux version `, Linux kernel banner/Linux 内核启动横幅), `booting-linux` (`Booting Linux`, Linux boot message/Linux 启动信息), `starting-kernel` (`Starting kernel`, Kernel handoff message/内核交接信息).

**Presentation sentences (`describeFindings`):**
- en panic/pattern: `Observed <label> at line <n>: <evidence>`
- en boot-loop: `Observed <label> <count> times within the watch window; the target may be restarting before it settles: <evidence>`
- zh panic/pattern: `第 <n> 行出现<label>：<evidence>`
- zh boot-loop: `观察窗口内出现 <count> 次<label>，设备可能在稳定前反复重启：<evidence>`

Semantics to preserve: one panic finding per line (first/most-specific rule wins); a panic between banners marks the burst `failed` and clears the banner window; each banner rule reports at most once per burst; user patterns are never compiled as regexes; empty pattern text is rejected.

Related console classification (`inspectSerialConsole`) kinds: `login, sudo-password, confirmation, pager, password, bootloader, shell, panic, unknown`; wait statuses `settled|streaming|no-output|awaiting-input`; tracked commands wrap as `sh -c '<cmd>'; printf '\n<LINKR_EXIT_uuid>:<code>\n'` and require an idle POSIX shell prompt (max input 2048 chars, exit code ≤ 255).

---

## 9. localStorage Persistence Keys

### 9.1 Core app (`app.js`)

| Key | Values | Written by |
|---|---|---|
| `linkr-theme` | `dark` \| `light` (default: `prefers-color-scheme` → `dark`/`light`) | theme buttons |
| `linkr-lang` | `en` \| `zh` (default from `navigator.language`) | language buttons |
| `linkr-font-family` | `system`\|`symbols`\|`jetbrains`\|`meslo`\|`firacode` | `#fontSelect` |
| `linkr-last-device-id` | BLE device id (removed on failed restore) | connect/restore |
| `linkr-transport` | `ble` \| `ws` | transport switch |
| `linkr-ws-host` | host or full `ws(s)://` URL | `#wsHostInput` change/connect |
| `linkr-uart` | UART string (default `115200,8,n,1,n`) | `#uartInput` change |
| `linkr-enter` | `raw`\|`cr`\|`lf`\|`crlf` | `#enterSelect` |
| `linkr-chunk` | `1`–`244` (default `20`) | `#chunkInput` |
| `linkr-echo` | `"1"` \| `"0"` | `#localEchoInput` |
| `linkr-debug` | `"1"` \| `"0"` | `#debugInput` |
| `linkr-font` | `10`–`28` string (default 13 / 11 harmony) | zoom buttons |
| `linkr-sidebar` | `"open"` \| `"collapsed"` (default collapsed) | sidebar toggle |
| `linkr-settings-page` | `connection`\|`terminal`\|`network`\|`ai` | tab click |

### 9.2 Connection / assistant stores

| Key | Shape & caps |
|---|---|
| `linkr-lan-tokens-v1` | `{tokens: {"device:<id>"\|"host:<h>": "<32hex>"}, hosts: {"<host>": "device:<id>"}}` |
| `linkr-agent-model` | validated AI config JSON (endpoint, model, apiKey, headers, provider, reasoning, contextWindow, maxTokens) |
| `linkr-agent-pricing-v1` | `{input, output}` per 1M tokens, 0–100000 |
| `linkr-agent-session-v1` | `{[deviceKey]: {display, messages, updatedAt}}` — 40 messages, 60 display rows, 2000 chars/entry, 96000 B, 8 devices (oldest evicted) |
| `linkr-agent-tasks-v1` | array, last 20 tasks (redacted goal/summary, ≤8 plan steps, ≤8 executions) |
| `linkr-agent-notes-v1` | array — 12/device, 600 chars text, 200 chars evidence, global trim `NOTE_LIMIT*4` |
| `linkr-agent-command-policy-v1` | `{[deviceKey]: {alwaysAsk: [], allow: []}}` — 20 entries/list, 200 chars/entry |
| `linkr-target-profile:<targetId>` | cached device profile JSON (optional history cache) |

`deviceIdentity(status)` = `target:<targetId>` when binding verified, else `JSON.stringify([transport, deviceId, uart])`, else `null` (blocks task/note/policy saves).

---

### Cross-cutting constants a Rust TUI must mirror

- Timeouts: WS connect 15000, WS handshake 5000, WiFi scan 50000, WiFi event fallback 45000, diagnostics collect 8000, accessory status poll 20000/1500 (UART 6000/800, WebDAV 6000), target binding 35000, execution monitor 30000, run watchdog 900000, Full Auto window 900000, mgmt response 10000, geometry debounce 180, agent log tick 100, WiFi refreshes 1500/3500/7000, task/session save debounce 500.
- Sizes: ATT 244, reliable payload `min(limit, 232)`, mgmt default payload 512, journal 128 KiB (read 12000/16000), log 4 MiB, scrollback 10000, toast 3×2200+250, download 128 MiB, composer maxlength 4000, serial input 2048.
- Verification/binding markers: `LINKR_ID_<hex>`, `LINKR_EXIT_<hex>:<code>`, `LINKR_PART:`, `LINKR_SHA256:<64hex>`, `LINKR_BYTES: <n>`, `TOOL:<name>:available|missing`.
</subagent>