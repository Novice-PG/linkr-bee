import {test,expect} from '@playwright/test';
const sampleReasoning='Compare the observed console with the device status before suggesting a command.';
function sse({text='',thinking='',tool=null}) {
  const deltas=[{role:'assistant'},...(thinking?[{reasoning_content:thinking.slice(0,24)},{reasoning_content:thinking.slice(24)}]:[]),...(text?[{content:text}]:[]),...(tool?[{tool_calls:[{index:0,id:'call-'+crypto.randomUUID(),type:'function',function:{name:tool.name,arguments:JSON.stringify(tool.args||{})}}]}]:[])];
  const chunk=(delta,finish_reason)=>JSON.stringify({id:'activity',object:'chat.completion.chunk',created:1,model:'fixture',choices:[{index:0,delta,finish_reason}]});
  return [...deltas.map(delta=>`data: ${chunk(delta,null)}\n\n`),`data: ${chunk({},tool?'tool_calls':'stop')}\n\n`,'data: [DONE]\n\n'].join('');
}
async function setup(page,respond,{lang='en',theme='dark'}={}){
  const requests=[];
  await page.addInitScript(({lang,theme})=>{localStorage.setItem('linkr-lang',lang);localStorage.setItem('linkr-theme',theme);localStorage.setItem('linkr-sidebar','closed');localStorage.setItem('linkr-agent-model',JSON.stringify({endpoint:'https://activity.test/v1',model:'fixture',reasoning:'low'}));},{lang,theme});
  await page.route('**/app.js?*',async route=>{const response=await route.fetch();await route.fulfill({response,body:await response.text()+'\nwindow.__activity={state,setConnected,setMobileSidebarOpen,handleIncomingBytes};'});});
  await page.route('https://activity.test/v1/chat/completions',async route=>{
    const headers={'access-control-allow-origin':'*','access-control-allow-headers':'*'};
    if(route.request().method()==='OPTIONS'){await route.fulfill({status:204,headers});return;}
    const request=route.request().postDataJSON();requests.push(request);const response=await respond(request,requests.length);
    await route.fulfill({status:200,headers,contentType:'text/event-stream',body:sse(response)});
  });
  await page.goto('/');await page.waitForFunction(()=>window.__activity?.state.term);
  await page.evaluate(()=>{const c=window.__activity;window.sent=[];c.state.mode='ws';c.state.wsHost='activity-board';c.state.ws={readyState:1,send:bytes=>{const text=new TextDecoder().decode(bytes);if(!text.startsWith('stty '))window.sent.push(text)}};c.setConnected(true);c.setMobileSidebarOpen(false,{restoreFocus:false});});
  await page.locator('#agentButton').click();return requests;
}
async function ask(page,text='Check the device status') {await page.locator('#agentQuestion').fill(text);await page.locator('#agentQuestion').press('Enter');}
const reply=(request,round)=>round===1?{thinking:sampleReasoning,text:'I will check the connected device first.',tool:{name:'get_device_status'}}:{text:'## Result\nThe bridge is connected.\n\nNo command was sent.'};

test('reasoning, intermediate notes and tool output fold away from the final answer',async({page},info)=>{
  await setup(page,reply);await ask(page);await expect(page.locator('#agentAsk')).toBeEnabled();
  const group=page.locator('.agent-activity');await expect(group).toHaveCount(1);await expect(group).not.toHaveAttribute('open');await expect(group).toHaveAttribute('data-state','done');
  await expect(page.locator('.agent-assistant:not(.agent-process-entry)')).toContainText('The bridge is connected.');
  await group.locator(':scope > summary').click();await expect(group).toContainText('I will check the connected device first.');
  const reasoning=group.locator('.agent-reasoning');await expect(reasoning).not.toHaveAttribute('open');
  await reasoning.locator('summary').click();await expect(reasoning.locator('.agent-message-text')).toHaveText(sampleReasoning);
  await page.screenshot({path:info.outputPath('activity-expanded.png')});
  expect(await page.evaluate(()=>window.sent)).toEqual([]);
});

test('manual disclosure expansion survives streaming and completion',async({page})=>{
  let release;const gate=new Promise(resolve=>release=resolve);
  await setup(page,async(request,round)=>{if(round===2)await gate;return reply(request,round)});await ask(page);
  const group=page.locator('.agent-activity');await expect(group).toHaveAttribute('data-state','running');await group.locator(':scope > summary').click();
  release();await expect(page.locator('#agentAsk')).toBeEnabled();await expect(group).toHaveAttribute('open','');
});

test('a tool error remains expanded and final answer text stays ordinary text',async({page})=>{
  await setup(page,(request,round)=>round===1?{tool:{name:'missing_tool'}}:{text:'Thinking about the issue is not a separate reasoning channel. The tool is unavailable.'});
  await ask(page);await expect(page.locator('#agentAsk')).toBeEnabled();
  await expect(page.locator('.agent-activity')).toHaveAttribute('data-state','error');await expect(page.locator('.agent-activity')).toHaveAttribute('open','');
  await expect(page.locator('.agent-reasoning')).toHaveCount(0);await expect(page.locator('.agent-assistant').last()).toContainText('Thinking about the issue');
});

test('manual serial approval automatically opens the activity without sending input',async({page})=>{
  await setup(page,(request,round)=>round===1?{tool:{name:'read_serial_log'}}:round===2?{tool:{name:'send_serial_input',args:{text:'uname -a',appendEnter:true}}}:{text:'Request rejected; no input was sent.'});
  await page.evaluate(()=>window.__activity.handleIncomingBytes(new TextEncoder().encode('root@board:~# ')));
  await page.locator('#agentModeButton').click();await page.locator('[name="agentMode"][value="manual"]').locator('..').click();
  await ask(page);const group=page.locator('.agent-activity');await expect(group).toHaveAttribute('open','');
  await expect(group.getByRole('button',{name:'Send',exact:true})).toBeVisible();expect(await page.evaluate(()=>window.sent)).toEqual([]);
  await group.getByRole('button',{name:'Reject',exact:true}).click();await expect(page.locator('#agentAsk')).toBeEnabled();expect(await page.evaluate(()=>window.sent)).toEqual([]);
});

test('stop settles the activity indicator and restored progress remains folded',async({page})=>{
  let release;const gate=new Promise(resolve=>release=resolve);
  await setup(page,async(request,round)=>{if(round===2)await gate;return reply(request,round)});await ask(page);
  await expect(page.locator('.agent-activity')).toHaveAttribute('data-state','running');await page.locator('#agentStop').click();release();
  await expect(page.locator('.agent-activity')).toHaveAttribute('data-state','interrupted');await expect(page.locator('#agentAsk')).toBeEnabled();
  await page.waitForTimeout(600);await page.reload();await page.waitForFunction(()=>window.__activity?.state.term);
  await page.evaluate(()=>{const c=window.__activity;c.state.mode='ws';c.state.wsHost='activity-board';c.state.ws={readyState:1,send:()=>{}};c.setConnected(true);c.setMobileSidebarOpen(false,{restoreFocus:false})});
  await page.locator('#agentButton').click();await expect(page.locator('.agent-activity')).toHaveCount(1);await expect(page.locator('.agent-activity')).not.toHaveAttribute('open');
});

test('Enter sends, Shift+Enter adds a line, and IME composition never sends',async({page})=>{
  const requests=await setup(page,()=>({text:'Received.'}));const input=page.locator('#agentQuestion');await input.fill('First line');await input.press('Shift+Enter');await expect(input).toHaveValue('First line\n');
  await input.dispatchEvent('keydown',{key:'Enter',isComposing:true});await input.dispatchEvent('keydown',{key:'Enter',keyCode:229});expect(requests).toHaveLength(0);
  await input.press('Enter');await expect(page.locator('#agentAsk')).toBeEnabled();expect(requests).toHaveLength(1);expect(requests[0].messages.findLast(message=>message.role==='user').content).toEqual([{type:'text',text:'First line'}]);
  await input.fill('Second message');await input.press('Control+Enter');await expect(page.locator('#agentAsk')).toBeEnabled();expect(requests).toHaveLength(2);expect(await page.evaluate(()=>window.sent)).toEqual([]);
});

test('Enter while running queues a follow-up instead of writing to UART',async({page})=>{
  let release;const gate=new Promise(resolve=>release=resolve);
  const requests=await setup(page,async(request,round)=>{if(round===1)await gate;return{text:round===1?'First result.':'Follow-up result.'}});await ask(page);
  await expect(page.locator('#agentFollowUp')).toBeEnabled();await page.locator('#agentQuestion').fill('Next task');await page.locator('#agentQuestion').press('Enter');
  await expect(page.locator('#agentQueue')).toContainText('Next task');release();await expect(page.locator('#agentAsk')).toBeEnabled();expect(requests).toHaveLength(2);expect(await page.evaluate(()=>window.sent)).toEqual([]);
});

for(const width of [338,390,834,1440])for(const lang of ['en','zh'])for(const theme of ['dark','light'])test(`polished assistant ${width}px ${lang} ${theme}`,async({page},info)=>{
  await page.setViewportSize({width,height:width<600?844:1000});await setup(page,lang==='zh'?((request,round)=>round===1?{thinking:'先对照串口证据与设备状态，再决定下一步排查范围。',text:'先读取当前设备状态。',tool:{name:'get_device_status'}}:{text:'## 检查结果\n桥接器已连接。\n\n本轮只查询状态，没有向串口发送命令。'}):reply,{lang,theme});await ask(page,lang==='zh'?'检查设备连接状态':'Check the device status');
  await expect(page.locator('#agentAsk')).toBeEnabled();await expect(page.locator('.agent-activity')).not.toHaveAttribute('open');
  expect(await page.locator('#agentPanel').evaluate(el=>el.scrollWidth<=el.clientWidth+1)).toBe(true);
  expect(await page.locator('#agentForm').evaluate(el=>el.getBoundingClientRect().bottom<=innerHeight)).toBe(true);
  await page.screenshot({path:info.outputPath(`assistant-${lang}-${theme}-${width}.png`)});
});
