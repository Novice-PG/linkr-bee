// Device identity owns BLE-provisioned tokens; host aliases only select a device.
const KEY = 'linkr-lan-tokens-v1';
export function lanHostKey(host) {
  try {
    const url = new URL(/^wss?:\/\//.test(host) ? host : `ws://${host}/ws`);
    return url.host.toLowerCase();
  } catch { return ''; }
}
export function createLanTokenStore(storage) {
  let data;
  try { data = JSON.parse(storage.getItem(KEY)); } catch { /* unavailable storage */ }
  const tokens = new Map(Object.entries(data?.tokens || {}));
  const hosts = new Map(Object.entries(data?.hosts || {}));
  let selected = '', value = '', dirty = false;
  const persist = () => {
    try { storage.setItem(KEY, JSON.stringify({tokens:Object.fromEntries(tokens),hosts:Object.fromEntries(hosts)})); }
    catch { /* The current session can still use its token. */ }
  };
  function select(key) {
    if (key !== selected) {
      selected = key; value = tokens.get(key) || ''; dirty = false;
    }
    return value;
  }
  return {
    get value() { return value; },
    selectDevice(id) { return select(`device:${id}`); },
    selectHost(host) {
      const key = lanHostKey(host);
      return select(hosts.get(key) || `host:${key}`);
    },
    edit(text) { value = text; dirty = true; },
    save() { if (selected) { tokens.set(selected,value); persist(); } },
    capture(id, token, host = '') {
      if (!id || (token !== '' && !/^[0-9a-f]{32}$/.test(token))) return value;
      const key = `device:${id}`;
      select(key);
      tokens.set(key, token);
      if (host && lanHostKey(host)) hosts.set(lanHostKey(host),key);
      if (!dirty) value = token;
      persist();
      return value;
    },
  };
}
