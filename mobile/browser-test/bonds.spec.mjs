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
    window.bondLabels={};
    window.toHex=s=>Array.from(new TextEncoder().encode(s),b=>b.toString(16).padStart(2,'0')).join('');
    window.fromHex=h=>new TextDecoder().decode(Uint8Array.from(h.match(/../g)||[],x=>parseInt(x,16)));
    t.mock(async cmd=>{window.commands.push(cmd); if(cmd==='@linkr bonds?')return window.bondsReply;
      if(cmd.startsWith('@linkr bonds label ')){const [id,value]=cmd.slice('@linkr bonds label '.length).split(' ');
        if(!value){const l=window.bondLabels[id]||'';return l?`OK bonds label=${id} hex=${window.toHex(l)}`:`OK bonds label=${id} none`;}
        if(value==='clear'){delete window.bondLabels[id];return `OK bonds label=${id} none`;}
        window.bondLabels[id]=window.fromHex(value);return `OK bonds label=${id} hex=${value}`;}
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
  page.once('dialog',d=>d.dismiss());await page.locator('.bond-row').last().locator('[data-bond-action="remove"]').click();
  expect(await page.evaluate(()=>window.commands.filter(c=>c.includes('remove')))).toEqual([]);
  page.once('dialog',d=>d.accept());await page.locator('.bond-row').last().locator('[data-bond-action="remove"]').click();
  await expect(page.locator('.bond-row')).toHaveCount(1);
  expect(await page.evaluate(()=>window.commands)).toContain('@linkr bonds remove 1/AA:BB:CC:DD:EE:02');
});
test('deleting this host does not schedule automatic reconnect',async({page})=>{
  page.once('dialog',d=>d.accept());await page.locator('.bond-row').first().locator('[data-bond-action="remove"]').click();
  await expect.poll(()=>page.evaluate(()=>window.bondTest.state.connected)).toBe(false);
  expect(await page.evaluate(()=>window.bondTest.autoReconnect.active)).toBe(false);
  await expect(page.locator('.bond-row')).toHaveCount(0);
});
test('readonly host can list but cannot delete or rename',async({page})=>{
  await page.evaluate(()=>{const t=window.bondTest;t.state.serialControl={mode:'readonly',self:1,owner:2,peers:2};t.renderSerialControl()});
  await expect(page.locator('.bond-row').first().locator('[data-bond-action="remove"]')).toBeDisabled();
  await expect(page.locator('.bond-row').first().locator('[data-bond-action="rename"]')).toBeDisabled();
  await expect(page.locator('[data-bonds-refresh]')).toBeEnabled();
});
test('renaming stores a label and shows it above the address',async({page})=>{
  const first=page.locator('.bond-row').first();
  await first.locator('[data-bond-action="rename"]').click();
  const input=first.locator('.bond-input');
  await expect(input).toBeVisible();
  await input.fill('我的手机');
  await first.locator('[data-bond-action="save"]').click();
  await expect(first.locator('.bond-name')).toHaveText('我的手机');
  await expect(first.locator('.bond-meta')).toContainText('0/AA:BB:CC:DD:EE:01');
  expect(await page.evaluate(()=>window.commands)).toContain('@linkr bonds label 0/AA:BB:CC:DD:EE:01 e68891e79a84e6898be69cba');
});
test('cancelling an edit changes nothing',async({page})=>{
  const first=page.locator('.bond-row').first();
  const before=await first.locator('.bond-name').textContent();
  await page.evaluate(()=>window.commands.length=0);
  await first.locator('[data-bond-action="rename"]').click();
  await first.locator('.bond-input').fill('nope');
  await first.locator('[data-bond-action="cancel"]').click();
  await expect(first.locator('.bond-name')).toHaveText(before);
  const sets=await page.evaluate(()=>window.commands.filter(c=>/^@linkr bonds label \S+ \S+$/.test(c)));
  expect(sets).toEqual([]);
});
test('clearing a label returns the row to the address only',async({page})=>{
  const first=page.locator('.bond-row').first();
  await first.locator('[data-bond-action="rename"]').click();
  await first.locator('.bond-input').fill('temp');
  await first.locator('[data-bond-action="save"]').click();
  await expect(first.locator('.bond-name')).toHaveText('temp');
  await first.locator('[data-bond-action="rename"]').click();
  await first.locator('.bond-input').fill('');
  await first.locator('[data-bond-action="save"]').click();
  await expect(first.locator('.bond-name')).toHaveText('0/AA:BB:CC:DD:EE:01');
  expect(await page.evaluate(()=>window.commands)).toContain('@linkr bonds label 0/AA:BB:CC:DD:EE:01 clear');
});
test('reports this browser name once when the board has none',async({page})=>{
  const sets=await page.evaluate(()=>window.commands.filter(c=>c.startsWith('@linkr bonds label 0/AA:BB:CC:DD:EE:01 ')));
  expect(sets).toHaveLength(1);
  const first=page.locator('.bond-row').first();
  await expect(first.locator('.bond-name')).not.toHaveText('0/AA:BB:CC:DD:EE:01');
  await page.evaluate(()=>window.bondTest.query());
  const again=await page.evaluate(()=>window.commands.filter(c=>c.startsWith('@linkr bonds label 0/AA:BB:CC:DD:EE:01 ')));
  expect(again).toHaveLength(1);
});

test('firmware without labels keeps the list but hides rename',async({page})=>{
  await page.evaluate(async()=>{
    const t=window.bondTest;
    t.mock(async cmd=>{
      if(cmd==='@linkr bonds?')return 'OK bonds pending=0 result=0\nBOND 0/AA:BB:CC:DD:EE:01 self=1\n';
      if(cmd.startsWith('@linkr bonds label '))return 'ERR unknown command';
      return 'ERR unsupported';
    });
    await t.query();
  });
  await expect(page.locator('.bond-row')).toHaveCount(1);
  await expect(page.locator('[data-bond-action="rename"]')).toHaveCount(0);
  await expect(page.locator('[data-bond-action="remove"]')).toHaveCount(1);
  await expect(page.locator('#boardBondsCard')).toBeVisible();
});
test('unsupported firmware hides card and unavailable board section',async({page})=>{
  await page.evaluate(async()=>{window.bondsReply='ERR unknown command';await window.bondTest.query()});
  await expect(page.locator('#boardBondsCard')).toBeHidden();
  await expect(page.locator('[data-settings-target="board"]')).toBeHidden();
});
