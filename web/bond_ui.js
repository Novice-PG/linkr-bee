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

export function createBondUi({ card, context, send, supported, beforeSelfDelete, afterSelfDelete }) {
  let peers = [], busy = false, generation = null;
  const list = card.querySelector('[data-bonds-list]');
  const status = card.querySelector('[role="status"]');
  const refresh = card.querySelector('[data-bonds-refresh]');
  const text = (zh, en) => context().lang.startsWith('zh') ? zh : en;
  const live = token => context().ready && context().generation === token;
  function render() {
    const ctx = context();
    if (generation !== ctx.generation) { peers = []; generation = ctx.generation; status.textContent = ''; busy = false; }
    card.querySelector('[data-bonds-title]').textContent = text('已配对设备', 'Paired devices');
    card.setAttribute('aria-label', text('已配对设备', 'Paired devices'));
    card.querySelector('[data-bonds-hint]').textContent = text('管理保存在 Bee 上的蓝牙配对记录。删除后需重新配对；主机系统中的旧记录需自行忽略。地址是配对身份地址，可能与扫描地址不同。', 'Manage Bluetooth pairings saved on Bee. Deleted hosts must pair again; forget their old OS pairing too. Identity addresses may differ from scan addresses.');
    refresh.textContent = text('刷新列表', 'Refresh list');
    refresh.disabled = busy || !ctx.ready;
    list.replaceChildren();
    for (const peer of peers) {
      const row = document.createElement('div'); row.className = 'bond-row';
      const label = document.createElement('span'); label.textContent = peer.id + (peer.self ? text(' · 当前主机', ' · This host') : '');
      const button = document.createElement('button'); button.type = 'button'; button.className = 'btn btn-soft';
      button.textContent = text('删除配对', 'Remove pairing'); button.disabled = busy || !ctx.ready || !ctx.control;
      button.setAttribute('aria-label', `${button.textContent} ${peer.id}`);
      button.onclick = () => remove(peer);
      row.append(label, button); list.append(row);
    }
  }
  async function query() {
    if (!context().ready || context().supported === false || busy) return;
    const token = context().generation; busy = true; render();
    try {
      const response = await send('@linkr bonds?');
      if (!live(token)) return;
      if (/ERR.*(?:unknown command|unsupported)/i.test(response)) { supported(false); return; }
      const result = parseBonds(response); supported(true); peers = result.peers;
      status.textContent = result.pending ? text('正在删除，请稍后刷新。', 'Removal pending; refresh shortly.') : result.result ? text('上次删除失败，请刷新后重试。', 'Last removal failed; refresh and retry.') : peers.length ? text(`共 ${peers.length} 个设备`, `${peers.length} paired devices`) : text('没有已配对设备。', 'No paired devices.');
    } catch (error) { if (live(token)) status.textContent = error.message; }
    finally { if (live(token)) { busy = false; render(); } }
  }
  async function remove(peer) {
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
      if (live(token)) { busy = false; render(); await query(); }
    }
  }
  refresh.onclick = query;
  render();
  return { render, query };
}
