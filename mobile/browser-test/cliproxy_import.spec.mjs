import { test, expect } from '@playwright/test';
const oldConfig = { endpoint: 'https://old.test/v1', apiKey: 'old-client', model: 'old-model' };
const stored = page => page.evaluate(() => JSON.parse(localStorage.getItem('linkr-agent-model')));
async function openImport(page) {
  await page.goto('/');
  await expect(page.locator('#agentButton')).toBeVisible();
  await page.locator('#agentButton').click();
  await page.locator('#agentSettingsButton').click();
  await page.locator('#cliproxyImport > summary').click();
  await page.locator('#cliproxyAddress').fill('https://proxy.test/cpa/v1');
  await page.locator('#cliproxySecret').fill('temporary-management-secret');
}
async function proxy(page, { keys = ['client-one'], models = ['model-one'], status = 200 } = {}) {
  const calls = [];
  await page.route('https://proxy.test/**', async route => {
    const request = route.request();
    if (request.method() === 'OPTIONS') { await route.fulfill({ status: 204 }); return; }
    calls.push({ url: request.url(), auth: request.headers().authorization, method: request.method() });
    const management = request.url().endsWith('/config/access/api-keys');
    await route.fulfill({ status: management ? status : 200, contentType: 'application/json',
      body: JSON.stringify(management ? keys : { data: models.map(id => ({ id })) }) });
  });
  return calls;
}
test('one click saves one choice and never persists the management key', async ({ page }) => {
  const calls = await proxy(page);
  await openImport(page); await page.locator('#cliproxyConnect').click();
  await expect(page.locator('#cliproxyStatus')).toContainText('Imported and saved');
  await expect(page.locator('#cliproxySecret')).toHaveValue('');
  expect(await stored(page)).toMatchObject({ endpoint: 'https://proxy.test/cpa/v1', apiKey: 'client-one', model: 'model-one', headers: {}, provider: 'openai-completions' });
  expect(calls).toEqual([
    { url: 'https://proxy.test/cpa/v8/management/config/access/api-keys', auth: 'Bearer temporary-management-secret', method: 'GET' },
    { url: 'https://proxy.test/cpa/v1/models', auth: 'Bearer client-one', method: 'GET' },
  ]);
  expect(await page.evaluate(() => JSON.stringify({ ...localStorage, ...sessionStorage }))).not.toContain('temporary-management-secret');
  await page.reload(); expect(await stored(page)).toMatchObject({ apiKey: 'client-one', model: 'model-one' });
});
test('multiple keys and models require explicit choices and masked labels', async ({ page }) => {
  const calls = await proxy(page, { keys: ['client-one-1234', 'client-two-5678'], models: ['m1', 'm2'] });
  await openImport(page); await page.locator('#cliproxyConnect').click();
  await expect(page.locator('#cliproxyClient')).toBeVisible();
  expect(await page.locator('#cliproxyClient').textContent()).not.toContain('client-two');
  expect(await stored(page)).toBeNull();
  await page.locator('#cliproxyClient').selectOption('1');
  await expect(page.locator('#cliproxyModel')).toBeVisible();
  await expect(page.locator('#cliproxySave')).toBeDisabled();
  await page.locator('#cliproxyModel').selectOption('1'); await page.locator('#cliproxySave').click();
  expect(await stored(page)).toMatchObject({ apiKey: 'client-two-5678', model: 'm2' });
  expect(calls.at(-1).auth).toBe('Bearer client-two-5678');
});
for (const status of [401, 403, 404]) test(`HTTP ${status} preserves config and clears temporary credentials`, async ({ page }) => {
  const calls = await proxy(page, { status });
  await page.addInitScript(c => localStorage.setItem('linkr-agent-model', JSON.stringify(c)), oldConfig);
  await openImport(page); await page.locator('#cliproxyConnect').click();
  await expect(page.locator('#cliproxyStatus')).toHaveClass(/field-error/);
  expect(await stored(page)).toEqual(oldConfig);
  await expect(page.locator('#cliproxySecret')).toHaveValue('');
  await expect(page.locator('#agentSettingsSave')).toBeEnabled(); expect(calls).toHaveLength(1);
});
test('cancel ignores a late response and unlocks settings', async ({ page }) => {
  let release;
  await page.route('https://proxy.test/**', async route => {
    await new Promise(resolve => { release = resolve; });
    await route.fulfill({ status: 200, contentType: 'application/json', body: '["client-one"]' }).catch(() => {});
  });
  await openImport(page); await page.locator('#cliproxyConnect').click();
  await expect.poll(() => Boolean(release)).toBe(true);
  await expect(page.locator('#agentSettingsSave')).toBeDisabled();
  await page.locator('#cliproxyCancel').click(); release();
  await expect(page.locator('#cliproxyStatus')).toContainText('Import cancelled');
  expect(await stored(page)).toBeNull();
  await expect(page.locator('#cliproxySecret')).toHaveValue('');
  await expect(page.locator('#agentSettingsSave')).toBeEnabled();
});
test('denied storage preserves config and can retry without management key', async ({ page }) => {
  await proxy(page);
  await page.addInitScript(c => localStorage.setItem('linkr-agent-model', JSON.stringify(c)), oldConfig);
  await openImport(page);
  await page.evaluate(() => {
    window.originalSetItem = Storage.prototype.setItem;
    Storage.prototype.setItem = function(k, v) { if (k === 'linkr-agent-model') throw Error('denied'); return window.originalSetItem.call(this, k, v); };
  });
  await page.locator('#cliproxyConnect').click();
  await expect(page.locator('#cliproxyStatus')).toContainText('Save failed'); expect(await stored(page)).toEqual(oldConfig);
  await page.evaluate(() => { Storage.prototype.setItem = window.originalSetItem; });
  await page.locator('#cliproxySave').click(); await expect(page.locator('#cliproxyStatus')).toContainText('Imported and saved');
});
test('remote HTTP requires consent before sending credentials', async ({ page }) => {
  let calls = 0;
  await page.route('http://192.0.2.1/**', async route => { calls++; await route.abort(); });
  await openImport(page); await page.locator('#cliproxyAddress').fill('http://192.0.2.1:8317');
  await page.locator('#cliproxySecret').fill('temporary-management-secret');
  page.once('dialog', dialog => dialog.dismiss()); await page.locator('#cliproxyConnect').click();
  expect(calls).toBe(0); expect(await stored(page)).toBeNull();
});
for (const [name, width, height, language] of [['mobile', 390, 844, 'en'], ['desktop', 1440, 900, 'zh'], ['user-1552', 1552, 1221, 'zh']]) {
  test(`import layout at ${name} is translated and contained`, async ({ browser }) => {
    const context = await browser.newContext({ viewport: { width, height }, isMobile: width < 900, hasTouch: width < 900 });
    const page = await context.newPage(); await openImport(page); await page.locator('#cliproxySecret').fill('');
    if (language === 'zh') await page.locator(width < 900 ? '#drawerLangButton' : '#langButton').click();
    await expect(page.locator('#cliproxyConnect')).toHaveText(language === 'zh' ? '连接并导入' : 'Connect and import');
    await page.evaluate(() => {
      document.querySelector('#controlsPanel').scrollTop = 0;
      document.querySelector('.agent-settings-scroll').scrollTop = 0;
    });
    expect(await page.locator('#controlsPanel').evaluate(el => el.scrollWidth <= el.clientWidth)).toBe(true);
    for (const id of ['cliproxyAddress', 'cliproxySecret', 'cliproxyConnect']) {
      const box = await page.locator(`#${id}`).boundingBox();
      expect(box.width).toBeGreaterThan(100); expect(box.x).toBeGreaterThanOrEqual(0); expect(box.x + box.width).toBeLessThanOrEqual(width);
    }
    await page.screenshot({ path: `../.impeccable/review/${name}.png`, fullPage: true }); await context.close();
  });
}

test('closing import clears pending choices; changing address cannot reuse keys', async ({ page }) => {
  await proxy(page, { models: ['m1', 'm2'] });
  await openImport(page); await page.locator('#cliproxyConnect').click();
  await expect(page.locator('#cliproxyModel')).toBeVisible();
  await page.locator('#cliproxyAddress').fill('https://another.test');
  await expect(page.locator('#cliproxyChoices')).toBeHidden();
  await expect(page.locator('#agentSettingsSave')).toBeEnabled();
  expect(await stored(page)).toBeNull();
  await page.locator('#cliproxySecret').fill('new-temporary-secret');
  await page.locator('#cliproxyImport > summary').click();
  await expect(page.locator('#cliproxySecret')).toHaveValue('');
});
test('model lookup failure never writes a partially imported configuration', async ({ page }) => {
  await proxy(page, { models: [] });
  await page.addInitScript(c => localStorage.setItem('linkr-agent-model', JSON.stringify(c)), oldConfig);
  await openImport(page); await page.locator('#cliproxyConnect').click();
  await expect(page.locator('#cliproxyStatus')).toContainText('No models available');
  expect(await stored(page)).toEqual(oldConfig);
  await expect(page.locator('#cliproxySecret')).toHaveValue('');
});
