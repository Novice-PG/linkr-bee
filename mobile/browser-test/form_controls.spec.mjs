import { test, expect } from '@playwright/test';

async function open(page, {theme='dark', lang='en', fallback=false}={}) {
  await page.addInitScript(({theme,lang,fallback})=>{
    localStorage.setItem('linkr-theme',theme);localStorage.setItem('linkr-lang',lang);localStorage.setItem('linkr-sidebar','open');
    if(fallback){Object.defineProperty(HTMLElement.prototype,'showPopover',{configurable:true,value:undefined});Object.defineProperty(HTMLElement.prototype,'hidePopover',{configurable:true,value:undefined});}
  },{theme,lang,fallback});
  await page.route('**/app.js?*',async route=>{
    const response=await route.fetch();await route.fulfill({response,body:await response.text()+'\nwindow.__forms={state,setSettingsPage,setMobileSidebarOpen,renderUartMode,installSelectControls};'});
  });
  await page.goto('/');await page.waitForFunction(()=>window.__forms?.state.term && document.querySelector('.select-trigger'));
  await page.evaluate(()=>{window.__forms.setSettingsPage('terminal');window.__forms.setMobileSidebarOpen(true);});
}
const mode=page=>page.locator('#uartModeSelect').locator('..').getByRole('combobox');

test('mouse selection preserves native value and the saved UART mode',async({page})=>{
  await open(page);await mode(page).click();
  await expect(page.getByRole('listbox')).toBeVisible();
  await page.getByRole('option',{name:'Reliability first',exact:true}).click();
  await expect(mode(page)).toHaveText('Reliability first');
  await expect(page.locator('#uartModeSelect')).toHaveValue('reliable');
  expect(await page.evaluate(()=>localStorage.getItem('linkr-uart-mode'))).toBe('reliable');
  await expect(page.getByRole('listbox')).toHaveCount(0);
});

test('Escape cancels navigation and closes the menu before the settings drawer',async({page})=>{
  await open(page);await mode(page).press('Enter');await mode(page).press('ArrowDown');await mode(page).press('Escape');
  await expect(mode(page)).toHaveAttribute('aria-expanded','false');
  await expect(page.locator('#uartModeSelect')).toHaveValue('interactive');
  await expect(page.locator('#controlsPanel')).toBeVisible();await expect(mode(page)).toBeFocused();
});

test('keyboard choice skips native popup and Tab does not trap focus',async({page})=>{
  await open(page);await mode(page).press('Space');await mode(page).press('End');await mode(page).press('Enter');
  await expect(page.locator('#uartModeSelect')).toHaveValue('reliable');
  await mode(page).press('Enter');await mode(page).press('Tab');
  await expect(mode(page)).toHaveAttribute('aria-expanded','false');
  await expect(page.locator('#enterSelect').locator('..').getByRole('combobox')).toBeFocused();
});

test('outside clicks dismiss without changing the setting',async({page})=>{
  await open(page);await mode(page).click();await page.locator('#uartInput').click();
  await expect(mode(page)).toHaveAttribute('aria-expanded','false');await expect(page.locator('#uartModeSelect')).toHaveValue('interactive');
});

test('programmatic values and native change events stay synchronized',async({page})=>{
  await open(page);
  await page.evaluate(()=>{document.querySelector('#uartModeSelect').value='reliable';});
  await expect(mode(page)).toHaveText('Reliability first');
  await page.locator('#uartModeSelect').selectOption('interactive');
  await expect(mode(page)).toHaveText('Interaction first');
});

test('translated label, options and accessible name update together',async({page})=>{
  await open(page);await page.locator('#drawerLangButton').click();
  await expect(page.getByRole('combobox',{name:'BLE 串口模式',exact:true})).toHaveText('交互优先');
  await mode(page).click();await expect(page.getByRole('option',{name:'可靠优先',exact:true})).toBeVisible();
  expect(await page.locator('#uartModeSelect').getAttribute('aria-hidden')).toBe('true');
});

test('disabling a source select closes its menu and disables the trigger',async({page})=>{
  await open(page);await mode(page).click();await page.evaluate(()=>{document.querySelector('#uartModeSelect').disabled=true;});
  await expect(mode(page)).toBeDisabled();await expect(mode(page)).toHaveAttribute('aria-expanded','false');
});

test('new selects and async options are enhanced without losing change handlers',async({page})=>{
  await open(page);
  await page.evaluate(()=>{
    const label=document.createElement('label');label.className='field';label.innerHTML='<span class="field-label">Dynamic model</span><select id="dynamicSelect" disabled></select>';
    document.querySelector('[data-settings-page="terminal"]').append(label);
    document.querySelector('#dynamicSelect').addEventListener('change',()=>window.dynamicChanges=(window.dynamicChanges||0)+1);
  });
  const trigger=page.getByRole('combobox',{name:'Dynamic model',exact:true});await expect(trigger).toBeDisabled();
  await page.evaluate(()=>{const s=document.querySelector('#dynamicSelect');s.replaceChildren(new Option('First model','first'),new Option('Second model','second'));s.disabled=false;s.value='second';});
  await expect(trigger).toHaveText('Second model');await trigger.click();await page.getByRole('option',{name:'First model',exact:true}).click();
  expect(await page.evaluate(()=>window.dynamicChanges)).toBe(1);await expect(page.locator('#dynamicSelect')).toHaveValue('first');
});

test('groups, disabled options and prefix search remain usable with a long list',async({page})=>{
  await open(page);
  await page.evaluate(()=>{
    const s=document.querySelector('#fontSelect');s.replaceChildren();const group=document.createElement('optgroup');group.label='Models';
    const disabled=new Option('Unavailable','no');disabled.disabled=true;group.append(disabled);
    for(let i=0;i<35;i++)group.append(new Option(`Model ${String(i).padStart(2,'0')}`,'m'+i));s.append(group);s.value='m0';
  });
  const trigger=page.locator('#fontSelect').locator('..').getByRole('combobox');await trigger.press('Enter');await trigger.press('End');
  await expect(trigger).toHaveAttribute('aria-activedescendant',/option-35$/);
  const viewport=await page.getByRole('listbox').boundingBox();expect(viewport.y+viewport.height).toBeLessThanOrEqual(845);
  await trigger.press('Home');await expect(trigger).toHaveAttribute('aria-activedescendant',/option-1$/);
  await trigger.press('m');await trigger.press('Escape');await expect(page.locator('#fontSelect')).toHaveValue('m0');
});

for(const theme of ['light','dark'])test(`menu uses the ${theme} palette and fits a narrow viewport`,async({page})=>{
  await page.setViewportSize({width:338,height:600});await open(page,{theme});await mode(page).click();
  const menu=page.getByRole('listbox');await expect(menu).toBeVisible();const bounds=await menu.boundingBox();
  expect(bounds.x).toBeGreaterThanOrEqual(0);expect(bounds.x+bounds.width).toBeLessThanOrEqual(338);expect(bounds.y+bounds.height).toBeLessThanOrEqual(600);
  const colors=await menu.evaluate(el=>({color:getComputedStyle(el).color,bg:getComputedStyle(el).backgroundColor,topLayer:el.matches(':popover-open')}));
  expect(colors.topLayer).toBe(true);expect(colors.color).not.toBe(colors.bg);
  expect(colors.bg).toMatch(/^rgb\(/); // An opaque menu must cover the form behind it.
  await page.screenshot({path:test.info().outputPath(`controls-${theme}.png`)});
});

test('menu fallback works when the WebView has no Popover API',async({page})=>{
  await open(page,{fallback:true});await mode(page).click();await expect(page.getByRole('listbox')).toBeVisible();
  await page.getByRole('option',{name:'Reliability first',exact:true}).click();await expect(mode(page)).toHaveText('Reliability first');
});

test('hidden controls cannot be revived by an explicit layout style',async({page})=>{
  await open(page);
  const visible=await page.evaluate(()=>{const el=document.createElement('button');el.className='term-tool';el.hidden=true;el.textContent='Hidden';document.body.append(el);return getComputedStyle(el).display;});
  expect(visible).toBe('none');await expect(page.locator('#fontPreview')).toHaveText('user@bee:~ $ echo ready');
});


test('drawer opening does not steal focus from a field the user already chose',async({page})=>{
  await open(page);
  await page.evaluate(async()=>{
    window.__forms.setMobileSidebarOpen(false,{restoreFocus:false});
    window.__forms.setMobileSidebarOpen(true);
    document.querySelector('#uartModeSelect').parentElement.querySelector('.select-trigger').focus();
    await new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve)));
  });
  await expect(mode(page)).toBeFocused();await mode(page).press('Enter');
  await expect(page.getByRole('listbox')).toBeVisible();
});

test('hidden UART status does not describe a retired connection to assistive technology',async({page})=>{
  await open(page);
  const status=await page.evaluate(()=>{
    const c=window.__forms;Object.assign(c.state,{connected:true,nusReady:true,reliableReady:false});c.renderUartMode();
    const active=document.querySelector('#uartTransportStatus').textContent;
    c.state.connected=false;c.renderUartMode();
    return {active,disconnected:document.querySelector('#uartTransportStatus').textContent};
  });
  expect(status.active).toContain('NUS');expect(status.disconnected).toBe('');
});


for(const width of [338,1440])test(`checkbox labels retain a full touch target at ${width}px`,async({page})=>{
  await page.setViewportSize({width,height:900});await open(page);
  const label=page.locator('label.check').filter({has:page.locator('#localEchoInput')});
  const box=await label.boundingBox();expect(box.height).toBeGreaterThanOrEqual(44);
  await label.click();await expect(page.locator('#localEchoInput')).toBeChecked();
  expect(await page.evaluate(()=>localStorage.getItem('linkr-echo'))).toBe('1');
});


test('model suggestions share the themed menu while preserving custom input',async({page})=>{
  await open(page);await page.evaluate(()=>{
    window.__forms.setSettingsPage('ai');
    document.querySelector('#agentModelOptions').replaceChildren(new Option('model-a','model-a'),new Option('model-b','model-b'));
  });
  const model=page.locator('#agentModel');await model.fill('my-custom-id');
  await model.press('ArrowDown');await page.getByRole('option',{name:'model-b',exact:true}).click();
  await expect(model).toHaveValue('model-b');await expect(model).toBeFocused();
  await model.fill('another-custom-id');await expect(model).toHaveValue('another-custom-id');
  await expect(page.getByRole('listbox')).toHaveCount(0);
});

test('SSID suggestions use the same menu and emit the original input event',async({page})=>{
  await open(page);await page.evaluate(()=>{
    window.__forms.setSettingsPage('network');
    document.querySelector('#wifiSsidList').replaceChildren(new Option('Workshop WiFi','Workshop WiFi'));
    document.querySelector('#wifiSsidInput').addEventListener('input',()=>window.ssidChanges=(window.ssidChanges||0)+1);
  });
  await page.locator('#wifiSsidInput').press('ArrowDown');await page.getByRole('option',{name:'Workshop WiFi',exact:true}).click();
  await expect(page.locator('#wifiSsidInput')).toHaveValue('Workshop WiFi');expect(await page.evaluate(()=>window.ssidChanges)).toBe(1);
});

test('empty suggestion lists leave the editable input without an inactive arrow',async({page})=>{
  await open(page);await page.evaluate(()=>window.__forms.setSettingsPage('ai'));
  await expect(page.locator('#agentModel').locator('..').getByRole('combobox')).toHaveCount(0);
  await page.locator('#agentModel').fill('manual-model');await expect(page.locator('#agentModel')).toHaveValue('manual-model');
});
