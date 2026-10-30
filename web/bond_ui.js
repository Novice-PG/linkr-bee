import {
  bondLabelQueryCommand,
  bondLabelSetCommand,
  parseBondLabel,
  sanitizeBondLabel,
} from "./board_io.js";
import { defaultHostLabel } from "./host_label.js";

export function parseBonds(text) {
  const lines = String(text).trim().split(/\r?\n/);
  const header = /^OK bonds pending=([01]) result=(-?\d+)$/.exec(lines.shift());
  if (!header) throw new Error(String(text));
  const peers = lines.filter(Boolean).map(line => {
    const match = /^BOND ([01]\/(?:[0-9A-F]{2}:){5}[0-9A-F]{2}) self=([01])$/i.exec(line);
    if (!match) throw new Error('Invalid paired-device reply');
    return { id: match[1], self: match[2] === '1' };
  });
  return { pending: header[1] === '1', result: Number(header[2]), peers };
}

/* One row per bonded host: the label on top (falling back to the identity
 * address) and the address itself underneath once a label exists. Labels are
 * fetched one address at a time after the list, so an eight-host board never
 * pushes the fixed 512-byte list reply over its limit.
 *
 * The label command is probed lazily: a firmware without it hides only the
 * "rename" button, never the whole card — the list and removal still work. */
export function createBondUi({ card, context, send, supported, beforeSelfDelete, afterSelfDelete }) {
  let peers = [];
  let labels = new Map();
  let editing = null;
  let draft = "";
  let labelSupported = null;
  let selfReported = false;
  let busy = false;
  let generation = null;
  const list = card.querySelector('[data-bonds-list]');
  const status = card.querySelector('[role="status"]');
  const refresh = card.querySelector('[data-bonds-refresh]');
  const text = (zh, en) => context().lang.startsWith('zh') ? zh : en;
  const live = token => context().ready && context().generation === token;

  function syncGeneration() {
    const ctx = context();
    if (generation === ctx.generation) return;
    generation = ctx.generation;
    peers = [];
    labels = new Map();
    editing = null;
    draft = "";
    labelSupported = ctx.label ?? null;
    selfReported = false;
    busy = false;
    status.textContent = '';
  }

  function suggestion() {
    const userAgent = typeof navigator !== 'undefined' ? navigator.userAgent : '';
    return defaultHostLabel({ userAgent });
  }

  function setLabelSupported(value) {
    if (labelSupported === value) return;
    labelSupported = value;
    context().supportedLabel?.(value);
  }

  function metaText(peer) {
    const parts = [];
    if (labels.get(peer.id)) parts.push(peer.id);
    if (peer.self) parts.push(text('当前主机', 'This host'));
    return parts.join(' · ');
  }

  function button(action, word) {
    const element = document.createElement('button');
    element.type = 'button';
    element.className = 'btn btn-soft bond-action';
    element.dataset.bondAction = action;
    element.textContent = word;
    element.disabled = busy || !context().ready || !context().control;
    return element;
  }

  function render() {
    syncGeneration();
    const ctx = context();
    card.querySelector('[data-bonds-title]').textContent = text('已配对设备', 'Paired devices');
    card.setAttribute('aria-label', text('已配对设备', 'Paired devices'));
    card.querySelector('[data-bonds-hint]').textContent = text(
      '管理保存在 Bee 上的蓝牙配对记录。标签只用于显示，按身份地址存在板子上；删除后需重新配对；主机系统中的旧记录需自行忽略。',
      'Manage Bluetooth pairings saved on Bee. Labels are display-only and stored on the board by identity address. Deleted hosts must pair again; forget their old OS pairing too.');
    refresh.textContent = text('刷新列表', 'Refresh list');
    refresh.disabled = busy || !ctx.ready;
    list.replaceChildren();
    for (const peer of peers) {
      const row = document.createElement('div');
      row.className = 'bond-row';
      row.dataset.bondId = peer.id;

      if (editing === peer.id) {
        row.classList.add('is-editing');
        const form = document.createElement('div');
        form.className = 'bond-edit';
        const input = document.createElement('input');
        input.className = 'bond-input';
        input.type = 'text';
        input.value = draft;
        input.placeholder = text('设备标签', 'Device label');
        input.setAttribute('aria-label', text('设备标签', 'Device label'));
        input.addEventListener('input', () => { draft = input.value; });
        input.addEventListener('keydown', (event) => {
          if (event.key === 'Enter') {
            event.preventDefault();
            saveLabel(peer.id, input.value);
          } else if (event.key === 'Escape') {
            event.preventDefault();
            cancelEdit();
          }
        });
        const save = button('save', text('保存', 'Save'));
        save.addEventListener('click', () => saveLabel(peer.id, input.value));
        save.disabled = busy || !ctx.ready || !ctx.control;
        const cancel = button('cancel', text('取消', 'Cancel'));
        cancel.addEventListener('click', cancelEdit);
        cancel.disabled = busy;
        form.append(input, save, cancel);
        row.append(form);
      } else {
        const main = document.createElement('div');
        main.className = 'bond-main';
        const name = document.createElement('span');
        name.className = 'bond-name';
        name.textContent = labels.get(peer.id) || peer.id;
        main.append(name);
        const meta = metaText(peer);
        if (meta) {
          const metaLine = document.createElement('span');
          metaLine.className = 'bond-meta';
          metaLine.textContent = meta;
          main.append(metaLine);
        }
        const actions = document.createElement('div');
        actions.className = 'bond-actions';
        if (labelSupported !== false) {
          const rename = button('rename', text('改名', 'Rename'));
          rename.setAttribute('aria-label', `${rename.textContent} ${peer.id}`);
          rename.addEventListener('click', () => startEdit(peer));
          actions.append(rename);
        }
        const remove = button('remove', text('删除配对', 'Remove pairing'));
        remove.setAttribute('aria-label', `${remove.textContent} ${peer.id}`);
        remove.addEventListener('click', () => removePeer(peer));
        actions.append(remove);
        row.append(main, actions);
      }

      list.append(row);
    }
    if (editing !== null) {
      const input = list.querySelector('.bond-input');
      if (input) {
        input.focus();
        input.setSelectionRange(input.value.length, input.value.length);
      }
    }
  }

  function startEdit(peer) {
    if (busy || !context().ready || !context().control) return;
    editing = peer.id;
    draft = labels.get(peer.id) || (peer.self ? suggestion() : '');
    render();
  }

  function cancelEdit() {
    editing = null;
    draft = '';
    render();
  }

  async function saveLabel(id, value) {
    if (busy || !context().ready || !context().control) return;
    const token = context().generation;
    const clean = sanitizeBondLabel(value);
    busy = true;
    render();
    try {
      const response = await send(bondLabelSetCommand({ id, label: clean }));
      if (!live(token)) return;
      const parsed = parseBondLabel(response);
      if (!parsed) {
        status.textContent = text('保存失败，请重试。', 'Save failed; try again.');
        return;
      }
      if (parsed.label) labels.set(id, parsed.label);
      else labels.delete(id);
      setLabelSupported(true);
      editing = null;
      draft = '';
      status.textContent = '';
    } catch (error) {
      if (live(token)) status.textContent = error.message;
    } finally {
      if (live(token)) { busy = false; render(); }
    }
  }

  async function loadLabels(token) {
    if (labelSupported === false) return;
    for (const peer of peers) {
      if (!live(token) || labelSupported === false) return;
      let response;
      try {
        response = await send(bondLabelQueryCommand({ id: peer.id }));
      } catch {
        return;
      }
      if (!live(token)) return;
      if (/ERR.*(?:unknown command|unsupported)/i.test(response)) {
        setLabelSupported(false);
        render();
        return;
      }
      const parsed = parseBondLabel(response);
      if (!parsed) continue;
      if (parsed.label) labels.set(peer.id, parsed.label);
      else labels.delete(peer.id);
      render();
    }
    setLabelSupported(true);
    // A freshly paired host has no label yet; report this browser's UA once so
    // the list shows a friendly name. A label the owner already stored is
    // never touched, and a failed report is silent — the list still works.
    const own = peers.find(peer => peer.self);
    if (!own || labels.get(own.id) || selfReported || !context().control) return;
    const name = sanitizeBondLabel(suggestion());
    if (!name) return;
    selfReported = true;
    try {
      const response = await send(bondLabelSetCommand({ id: own.id, label: name }));
      if (!live(token)) return;
      const parsed = parseBondLabel(response);
      if (parsed && parsed.label) {
        labels.set(own.id, parsed.label);
        render();
      }
    } catch {
      // The board may have become read-only; the address still identifies it.
    }
  }

  async function query() {
    if (!context().ready || context().supported === false || busy) return;
    const token = context().generation; busy = true; render();
    try {
      const response = await send('@linkr bonds?');
      if (!live(token)) return;
      if (/ERR.*(?:unknown command|unsupported)/i.test(response)) { supported(false); return; }
      const result = parseBonds(response); supported(true); peers = result.peers; labels = new Map();
      status.textContent = result.pending ? text('正在删除，请稍后刷新。', 'Removal pending; refresh shortly.') : result.result ? text('上次删除失败，请刷新后重试。', 'Last removal failed; refresh and retry.') : peers.length ? text(`共 ${peers.length} 个设备`, `${peers.length} paired devices`) : text('没有已配对设备。', 'No paired devices.');
      // Stay busy through the label series: a row that is half-loaded must
      // not offer an edit or a removal that the next label render would
      // rebuild underneath the user.
      render();
      await loadLabels(token);
    } catch (error) { if (live(token)) status.textContent = error.message; }
    finally { if (live(token)) { busy = false; render(); } }
  }

  async function removePeer(peer) {
    if (busy || !context().ready || !context().control) return;
    const token = context().generation;
    const warning = text(`删除 ${peer.id} 的配对记录？${peer.self ? '当前连接将断开。' : '该主机若在线也将断开。'}重新连接需要按固件策略重新授权配对。`, `Remove pairing for ${peer.id}? ${peer.self ? 'This connection will close.' : 'That host will disconnect if online.'} Reconnection requires pairing under the firmware policy.`);
    if (!window.confirm(warning) || !live(token) || !context().control) return;
    busy = true; render();
    let queued = false;
    if (peer.self) beforeSelfDelete();
    try {
      const response = await send(`@linkr bonds remove ${peer.id}`);
      if (!live(token)) return;
      if (String(response).trim() !== 'OK bonds queued') throw new Error(String(response));
      queued = true;
      status.textContent = text('删除请求已受理，正在等待固件处理。', 'Removal queued; waiting for firmware.');
      await new Promise(resolve => setTimeout(resolve, 900));
    } catch (error) { if (live(token)) status.textContent = error.message; }
    finally {
      if (peer.self) afterSelfDelete(token, queued);
      if (live(token)) { busy = false; labels.delete(peer.id); render(); await query(); }
    }
  }

  refresh.onclick = query;
  render();
  return { render, query };
}
