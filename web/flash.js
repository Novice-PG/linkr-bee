import { ESPLoader, Transport } from "./vendor/esptool-js/bundle.js";

const DEFAULT_REPO = "radxa/linkr-bee";
const BOARD_NAMES = {
  "esp32-wroom-32": "ESP32-WROOM-32 DevKitC",
  "esp32-cam": "ESP32-CAM (ESP32-S module, UART0)",
  "esp32c3-devkitc": "ESP32-C3 DevKitC",
  "esp32c3-devkitm": "ESP32-C3 DevKitM",
  "esp32c3-supermini": "ESP32-C3 Super Mini",
  "esp32c5-devkitc": "ESP32-C5 DevKitC",
};

const $ = (id) => document.getElementById(id);
const boardSelect = $("boardSelect");
const baudSelect = $("baudSelect");
const manualBoot = $("manualBoot");
const camPrep = $("camPrep");
const connectButton = $("connectButton");
const downloadButton = $("downloadButton");
const resetButton = $("resetButton");
const statusLine = $("status");
const progressBar = $("progressBar");
const progressTrack = progressBar.parentElement;
const logView = $("log");

const state = {
  manifest: null,
  board: null,
  port: null,
  transport: null,
  loader: null,
  image: null,
};

// The flasher is deployed from the fork's Pages site while the PR is open and
// from origin after the merge. Prefer the repository the manifest was built
// from; on a github.io site the owner/repository are in the URL; every other
// host (including localhost) uses the upstream project.
function detectRepo() {
  const meta = document.querySelector('meta[name="linkr-repo"]')?.content?.trim();
  if (meta) return meta;
  const pages = location.hostname.match(/^([a-z0-9-]+)\.github\.io$/i);
  if (pages) {
    const repository = location.pathname.split("/").filter(Boolean)[0];
    if (repository) return `${pages[1]}/${repository}`;
  }
  return DEFAULT_REPO;
}

function repo() {
  return state.manifest?.repo || detectRepo();
}

function logLine(text) {
  logView.textContent += `${text}\n`;
  logView.scrollTop = logView.scrollHeight;
}

function setStatus(text, kind = "") {
  statusLine.textContent = text;
  statusLine.className = `status ${kind}`;
}

function setProgress(fraction) {
  const value = Math.max(0, Math.min(1, fraction));
  progressBar.style.width = `${value * 100}%`;
  progressTrack.classList.toggle("active", value > 0);
}

function formatBytes(bytes) {
  if (!Number.isFinite(bytes) || bytes <= 0) return "—";
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`;
  return `${(bytes / 1024 / 1024).toFixed(2)} MiB`;
}

function shortHash(hash) {
  if (!hash) return "—";
  return `${hash.slice(0, 16)}…`;
}

function boardLabel(slug, fallback) {
  return BOARD_NAMES[slug] || fallback || slug;
}

function releaseAssetUrl(board) {
  const tag = state.manifest?.tag || `v${state.manifest?.version}`;
  return `https://github.com/${repo()}/releases/download/${tag}/linkr-bee-${tag}-firmware-${board.slug}.zip`;
}

async function loadManifest() {
  try {
    const response = await fetch(`./firmware/manifest.json?cb=${Date.now()}`, {
      cache: "no-store",
    });
    if (response.ok) {
      const manifest = await response.json();
      manifest.source = "pages";
      return manifest;
    }
  } catch {
    // Local development, or a deployment made before the first release.
  }
  return loadLatestRelease();
}

async function loadLatestRelease() {
  const repository = repo();
  const response = await fetch(`https://api.github.com/repos/${repository}/releases/latest`, {
    headers: { Accept: "application/vnd.github+json" },
  });
  if (!response.ok) {
    throw new Error(`GitHub Releases 返回 ${response.status}`);
  }
  const release = await response.json();
  const boards = release.assets
    .filter((asset) => /^linkr-bee-.+-firmware-(.+)\.zip$/.test(asset.name))
    .map((asset) => {
      const slug = asset.name.replace(/^linkr-bee-.+-firmware-(.+)\.zip$/, "$1");
      return {
        slug,
        name: boardLabel(slug),
        chip: "",
        flash_address: "",
        image: "",
        size: asset.size,
        version: release.tag_name.replace(/^v/, ""),
        zip_url: asset.browser_download_url,
        image_url: "",
      };
    })
    .sort((a, b) => a.slug.localeCompare(b.slug));
  if (!boards.length) {
    throw new Error("最新发布里没有固件 ZIP");
  }
  return {
    schema: 1,
    version: release.tag_name.replace(/^v/, ""),
    tag: release.tag_name,
    generated_at: release.published_at,
    source: "api",
    repo: repository,
    boards,
  };
}

function parseChip(text) {
  return String(text || "")
    .toLowerCase()
    .replace(/[^a-z0-9]/g, "");
}

function selectBoard(slug) {
  const board = state.manifest.boards.find((item) => item.slug === slug);
  state.board = board || null;
  state.loader = null;
  state.image = null;
  $("factChip").textContent = board?.chip || "读取中…";
  $("factAddress").textContent = board?.flash_address || "读取中…";
  $("factSize").textContent = board ? formatBytes(board.size) : "—";
  $("factHash").textContent = board ? shortHash(board.sha256) : "—";
  $("factHash").title = board?.sha256 || "";
  connectButton.disabled = !board;
  downloadButton.disabled = !board;
  const isCam = board?.slug === "esp32-cam";
  camPrep.hidden = !isCam;
  // ESP32-CAM baseboards often use a CH340 without auto-download wiring. Start
  // slow so a marginal USB cable or power rail still completes the handshake.
  if (isCam) baudSelect.value = "115200";
  setProgress(0);
  if (board) {
    setStatus(
      isCam
        ? `已选择 ${board.name}。底板不能自动下载时，先按住 IO0 点一下 RST，再勾选“已手动进入下载模式”。`
        : `已选择 ${board.name}。点击“连接串口并烧录”开始。`,
    );
  }
}

function populateBoards(manifest) {
  state.manifest = manifest;
  boardSelect.replaceChildren();
  for (const board of manifest.boards) {
    const option = document.createElement("option");
    option.value = board.slug;
    option.textContent = board.name || boardLabel(board.slug);
    boardSelect.append(option);
  }
  boardSelect.disabled = false;
  const releasePill = $("releasePill");
  releasePill.classList.toggle("stale", manifest.source === "api");
  releasePill.textContent =
    manifest.source === "api"
      ? `最新发布 v${manifest.version}（在线）`
      : `最新编译固件 v${manifest.version}`;
  releasePill.title = manifest.generated_at
    ? `staged ${manifest.generated_at}`
    : "";
  $("heroVersion").textContent = `v${manifest.version}`;
  $("heroBoards").textContent = `${manifest.boards.length} 款`;
  $("releaseVersion").textContent = `v${manifest.version}`;
  $("releaseTag").textContent = manifest.tag || "—";
  $("releaseRepo").textContent = repo();
  const link = $("releaseLink");
  if (manifest.tag) {
    link.href = `https://github.com/${repo()}/releases/tag/${manifest.tag}`;
  }
  selectBoard(manifest.boards[0].slug);
}

async function fetchBytes(url, label, expectedSize = 0) {
  logLine(`下载 ${label} …`);
  const response = await fetch(url);
  if (!response.ok) throw new Error(`下载失败：HTTP ${response.status}`);
  const total = Number(response.headers.get("content-length")) || expectedSize || 0;
  if (!response.body) {
    const buffer = await response.arrayBuffer();
    setProgress(1);
    return new Uint8Array(buffer);
  }
  const reader = response.body.getReader();
  const chunks = [];
  let read = 0;
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    chunks.push(value);
    read += value.length;
    if (total) setProgress(read / total);
  }
  const image = new Uint8Array(read);
  let offset = 0;
  for (const chunk of chunks) {
    image.set(chunk, offset);
    offset += chunk.length;
  }
  setProgress(1);
  logLine(`下载完成：${formatBytes(read)}`);
  return image;
}

async function ensureImage(board) {
  if (state.image) return state.image;
  if (board.image_url) {
    state.image = await fetchBytes(board.image_url, board.image || board.slug, board.size);
    return state.image;
  }

  const zipUrl = board.zip_url || releaseAssetUrl(board);
  const zip = await fetchBytes(zipUrl, `${board.slug}.zip`, board.size);
  logLine("解压固件包 …");
  const archive = await JSZip.loadAsync(zip);
  const metadataEntry = Object.keys(archive.files).find((name) => name.endsWith("firmware.json"));
  if (!metadataEntry) throw new Error("固件包里没有 firmware.json");
  const metadata = JSON.parse(await archive.file(metadataEntry).async("string"));
  const imageName = String(metadata.image || "").split("/").pop();
  const imageEntry = Object.keys(archive.files).find((name) => name.endsWith(`/${imageName}`));
  if (!imageEntry) throw new Error(`固件包里没有 ${imageName}`);
  const image = await archive.file(imageEntry).async("uint8array");

  board.chip = metadata.chip || board.chip;
  board.flash_address = metadata.flash_address || board.flash_address;
  board.image = imageName;
  board.size = image.length;
  board.sha256 = "";
  state.image = image;
  $("factChip").textContent = board.chip || "—";
  $("factAddress").textContent = board.flash_address || "—";
  $("factSize").textContent = formatBytes(image.length);
  $("factHash").textContent = "在线 ZIP";
  $("factHash").title = "";
  return image;
}

// esptool-js 0.5.x takes a binary string, not a Uint8Array (newer releases
// accept the typed array). Build it in slices so a ~1 MiB image does not
// overflow String.fromCharCode's argument limit.
function toBinaryString(bytes) {
  let text = "";
  const chunk = 0x8000;
  for (let offset = 0; offset < bytes.length; offset += chunk) {
    text += String.fromCharCode(...bytes.subarray(offset, offset + chunk));
  }
  return text;
}

function terminal() {
  return {
    clean() {
      logView.textContent = "";
    },
    writeLine(text) {
      logLine(text);
    },
    write(text) {
      logView.textContent += text;
      logView.scrollTop = logView.scrollHeight;
    },
  };
}

async function flash() {
  if (!state.board) return;
  connectButton.disabled = true;
  resetButton.disabled = true;
  setProgress(0);
  setStatus("正在准备固件…");
  try {
    const image = await ensureImage(state.board);

    if (!navigator.serial) {
      throw new Error("当前浏览器不支持 Web Serial");
    }
    if (!state.port) {
      setStatus("请选择开发板串口…");
      state.port = await navigator.serial.requestPort();
    }

    // A failed attempt leaves the Web Serial port open, and reopening an open
    // port throws. Always start from a fresh transport so the user can retry
    // (or switch from auto reset to manual download mode) without reloading.
    if (state.transport) {
      try {
        await state.transport.disconnect();
      } catch {
        // The port may already be closed; the new transport still works.
      }
    }
    state.transport = new Transport(state.port, false);
    state.loader = null;

    const manual = manualBoot.checked;
    setStatus(manual ? "正在连接（已跳过自动复位）…" : "正在连接引导程序…");
    const loader = new ESPLoader({
      transport: state.transport,
      baudrate: Number(baudSelect.value),
      terminal: terminal(),
      debugLogging: false,
    });
    state.loader = loader;
    const chip = await loader.main(manual ? "no_reset" : "default_reset");
    logLine(`检测到芯片：${chip}`);

    const expected = parseChip(state.board.chip);
    if (expected && !parseChip(chip).includes(expected)) {
      throw new Error(`板型不匹配：选择的是 ${state.board.name}（${state.board.chip}），实际是 ${chip}`);
    }

    const address = Number.parseInt(state.board.flash_address || "0x0", 16);
    if (!Number.isFinite(address)) {
      throw new Error(`无效的烧录地址：${state.board.flash_address}`);
    }

    setStatus("正在写入固件，请勿断开 USB…");
    logLine(`写入 ${state.board.image || "firmware"} 到 0x${address.toString(16)} …`);
    await loader.writeFlash({
      fileArray: [{ data: toBinaryString(image), address }],
      flashSize: "keep",
      flashMode: "keep",
      flashFreq: "keep",
      eraseAll: false,
      compress: true,
      reportProgress: (_fileIndex, written, total) => {
        setProgress(total ? written / total : 0);
        if (total) {
          setStatus(`正在写入固件… ${Math.round((written / total) * 100)}%`);
        }
      },
    });

    await loader.after("hard_reset");
    setProgress(1);
    resetButton.disabled = false;
    if (manual) {
      setStatus("烧录完成。请按一下底板 RST 键启动新固件。", "ok");
      logLine("烧录完成。底板可能没有自动复位，请手动按 RST。");
    } else {
      setStatus("烧录完成。设备已重启，可以打开终端连接。", "ok");
      logLine("烧录完成，设备已重启。");
    }
  } catch (error) {
    console.error(error);
    let message = String(error?.message || error);
    if (
      state.board?.slug === "esp32-cam" &&
      !manualBoot.checked &&
      /fail|connect|timeout|serial|open/i.test(message)
    ) {
      message +=
        "。ESP32-CAM 底板可能没有自动下载电路：请按住 IO0 点一下 RST，松开 IO0，勾选“已手动进入下载模式”后再试。";
    }
    setStatus(`烧录失败：${message}`, "error");
    logLine(`错误：${message}`);
    // Close the failed session so the next click can open the port again.
    try {
      await state.transport?.disconnect();
    } catch {
      // Already closed.
    }
    state.transport = null;
    state.loader = null;
    resetButton.disabled = true;
  } finally {
    connectButton.disabled = !state.board;
  }
}

async function resetDevice() {
  try {
    if (state.loader) {
      await state.loader.after("hard_reset");
      setStatus("已发送重启命令。", "ok");
    } else if (state.transport) {
      await state.transport.setDTR(false);
      await new Promise((resolve) => setTimeout(resolve, 100));
      await state.transport.setDTR(true);
      setStatus("已发送重启命令。", "ok");
    }
  } catch (error) {
    setStatus(`重启失败：${error?.message || error}`, "error");
  }
}

const THEME_KEY = "linkr-theme";

function applyTheme(next) {
  document.documentElement.dataset.theme = next;
  const meta = $("themeColorMeta");
  if (meta) meta.content = next === "dark" ? "#05080D" : "#E2E9F4";
  const label = $("themeLabel");
  if (label) label.textContent = next === "dark" ? "浅色" : "深色";
}

function initTheme() {
  const theme =
    document.documentElement.dataset.theme === "light" ? "light" : "dark";
  applyTheme(theme);
  $("themeButton").addEventListener("click", () => {
    const next =
      document.documentElement.dataset.theme === "dark" ? "light" : "dark";
    try {
      localStorage.setItem(THEME_KEY, next);
    } catch {
      // Private/embedded storage can reject writes; the theme still applies.
    }
    applyTheme(next);
  });
}

async function init() {
  initTheme();
  if (!navigator.serial) {
    $("unsupported").hidden = false;
    connectButton.disabled = true;
  }
  boardSelect.addEventListener("change", () => selectBoard(boardSelect.value));
  connectButton.addEventListener("click", flash);
  resetButton.addEventListener("click", resetDevice);
  downloadButton.addEventListener("click", () => {
    if (!state.board) return;
    const url = state.board.zip_url || releaseAssetUrl(state.board);
    window.open(url, "_blank", "noopener");
  });
  $("clearLog").addEventListener("click", () => {
    logView.textContent = "";
  });

  logLine("读取最新固件清单…");
  try {
    populateBoards(await loadManifest());
    logLine(`已加载 v${state.manifest.version}，${state.manifest.boards.length} 个板型。`);
  } catch (error) {
    console.error(error);
    $("releasePill").classList.add("stale");
    $("releasePill").textContent = "无法读取最新固件";
    setStatus(`无法读取固件清单：${error?.message || error}`, "error");
    logLine(`错误：${error?.message || error}`);
    logLine("请从 GitHub Releases 下载固件 ZIP，或稍后刷新页面。");
  }
}

init();
