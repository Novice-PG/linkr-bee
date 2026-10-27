import { test, expect } from '@playwright/test';

async function fixture(page) {
  await page.route('**/mcp-review-fixture', route => route.fulfill({ contentType: 'text/html', body: '<!doctype html><body><aside id="mount"></aside></body>' }));
  await page.goto('/mcp-review-fixture');
  await page.evaluate(async () => {
    const { createMcpUi } = await import('/mcp_ui.js');
    const state = { enabled: true, running: true, foreground: true, token: 'test-token', url: 'http://127.0.0.1:8712/mcp' };
    const config = { enabled: true, port: 8712, bind: 'loopback', access: 'confirm' };
    const listeners = {};
    window.__approvalResults = [];
    window.__notifications = [];
    window.__cancelledNotifications = [];
    const bridge = {
      async getState() { return { ...state }; },
      async showApproval(input) {
        window.__notifications.push(input);
        if (window.__approveImmediately) listeners.approvalAction?.({ id: input.id, approved: true });
        return true;
      },
      async cancelApproval(id) { window.__cancelledNotifications.push(id); },
      async addListener(event, callback) { listeners[event] = callback; return { async remove() { delete listeners[event]; } }; },
    };
    const host = { getConfig: () => ({ ...config }), getState: () => ({ ...state }), getAudit: () => [], async refreshState() { return { ...state }; } };
    window.__approvalState = state;
    window.__approvalUi = createMcpUi({ mount: document.querySelector('#mount'), getLang: () => 'en', host, getBridge: () => bridge });
    window.__askApproval = (name = 'request') => window.__approvalUi.requestApproval({ tool: 'wifi_scan', args: {}, summary: name, timeoutMs: 5000 }).then(result => { window.__approvalResults.push({ name, ...result }); return result; });
  });
}

test('concurrent approvals reserve the slot before the first native await', async ({ page }) => {
  await fixture(page);
  await page.evaluate(() => { void window.__askApproval('first'); void window.__askApproval('second'); });
  await page.waitForTimeout(100);
  const early = await page.evaluate(() => window.__approvalResults);
  expect(early).toHaveLength(1);
  expect(early[0]).toMatchObject({ name: 'second', approved: false });
  await page.locator('[data-mcp="approvalAllow"]').click();
  await expect.poll(() => page.evaluate(() => window.__approvalResults)).toHaveLength(2);
  expect(await page.evaluate(() => window.__approvalResults.find(r => r.name === 'first').approved)).toBe(true);
});

test('a native pause migrates the visible banner even when WebView visibility stays visible', async ({ page }) => {
  await fixture(page);
  await page.evaluate(() => { void window.__askApproval(); });
  await expect(page.locator('[data-mcp="approvalAllow"]')).toBeVisible();
  await page.evaluate(async () => { window.__approvalState.foreground = false; await window.__approvalUi.refresh(); });
  expect(await page.evaluate(() => document.visibilityState)).toBe('visible');
  await expect.poll(() => page.evaluate(() => window.__notifications.length)).toBe(1);
});

test('an immediate notification action is not lost while showApproval resolves', async ({ page }) => {
  await fixture(page);
  await page.evaluate(() => {
    window.__approvalState.foreground = false;
    window.__approveImmediately = true;
    void window.__askApproval();
  });
  await page.waitForTimeout(100);
  expect(await page.evaluate(() => window.__approvalResults)).toMatchObject([{ approved: true }]);
});

test('aborting an approval dismisses its notification and never grants it', async ({ page }) => {
  await fixture(page);
  await page.evaluate(() => {
    window.__approvalState.foreground = false;
    window.__approvalAbort = new AbortController();
    void window.__approvalUi.requestApproval({ tool: 'set_wifi', args: {}, signal: window.__approvalAbort.signal })
      .then(result => window.__approvalResults.push(result));
  });
  await expect.poll(() => page.evaluate(() => window.__notifications.length)).toBe(1);
  await page.evaluate(() => window.__approvalAbort.abort(new Error('MCP stopped')));
  await expect.poll(() => page.evaluate(() => window.__approvalResults.length)).toBe(1);
  expect(await page.evaluate(() => window.__approvalResults[0].approved)).toBe(false);
  await expect.poll(() => page.evaluate(() => window.__cancelledNotifications.length)).toBe(1);
});
