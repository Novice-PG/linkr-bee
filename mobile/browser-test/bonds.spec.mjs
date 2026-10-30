import { test, expect } from '@playwright/test';
test.beforeEach(async ({page}) => {
  await page.route('**/app.js?*', async route => {
    const response = await route.fetch();
    await route.fulfill({response, body: await response.text() + '\nwindow.bondTest={state,setConnected,setSettingsPage,renderSerialControl,onDisconnected,autoReconnect, query:()=>bondUi.query(),mock:fn=>{sendControl=fn}};'});
  });
  await page.goto('/');
  await page.waitForFunction(()=>window.bondTest?.state.term);
  await page.evaluate(()=>{
    const t=window.bondTest; window.commands=[]; window.bondsReply='OK bonds pending=0 result=0\nBOND 0/AA:BB:CC:DD:EE:01 self=1\nBOND 1/AA:BB:CC:DD:EE:02 self=0\n';
    t.mock(async cmd=>{window.commands.push(cmd); if(cmd==='@linkr bonds?')return window.bondsReply;
      if(cmd.startsWith('@linkr bonds remove ')) {
        if(cmd.endsWith('01'))setTimeout(()=>t.onDisconnected({unexpected:true}),50);
        else window.bondsReply='OK bonds pending=0 result=0\nBOND 0/AA:BB:CC:DD:EE:01 self=1\n';
        return 'OK bonds queued';
      } return 'ERR unsupported';
    });
    t.state.mode='ble';t.state.mgmtReady=true;t.setConnected(true);t.setSettingsPage('board');
  });
  await page.locator('#panelToggle').tap();
  await expect(page.locator('.bond-row')).toHaveCount(2);
});
test('lists identities, cancel is inert, removal is confirmed by refreshed list',async({page})=>{
  await expect(page.locator('.bond-row').first()).toContainText('This host');
  page.once('dialog',d=>d.dismiss());await page.locator('.bond-row button').last().click();
  expect(await page.evaluate(()=>window.commands.filter(c=>c.includes('remove')))).toEqual([]);
  page.once('dialog',d=>d.accept());await page.locator('.bond-row button').last().click();
  await expect(page.locator('.bond-row')).toHaveCount(1);
  expect(await page.evaluate(()=>window.commands)).toContain('@linkr bonds remove 1/AA:BB:CC:DD:EE:02');
});
test('deleting this host does not schedule automatic reconnect',async({page})=>{
  page.once('dialog',d=>d.accept());await page.locator('.bond-row button').first().click();
  await expect.poll(()=>page.evaluate(()=>window.bondTest.state.connected)).toBe(false);
  expect(await page.evaluate(()=>window.bondTest.autoReconnect.active)).toBe(false);
  await expect(page.locator('.bond-row')).toHaveCount(0);
});
test('readonly host can list but cannot delete',async({page})=>{
  await page.evaluate(()=>{const t=window.bondTest;t.state.serialControl={mode:'readonly',self:1,owner:2,peers:2};t.renderSerialControl()});
  await expect(page.locator('.bond-row button').first()).toBeDisabled();
  await expect(page.locator('[data-bonds-refresh]')).toBeEnabled();
});
test('unsupported firmware hides card and unavailable board section',async({page})=>{
  await page.evaluate(async()=>{window.bondsReply='ERR unknown command';await window.bondTest.query()});
  await expect(page.locator('#boardBondsCard')).toBeHidden();
  await expect(page.locator('[data-settings-target="board"]')).toBeHidden();
});
